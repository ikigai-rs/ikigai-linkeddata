//! A typed `bindings=` on the query doors (ledger #2517): a value reaches a query as a TERM,
//! bound into the parsed query, never as text spliced into it.
//!
//! The claim: a parameterized request (`urn:quakes:near:{region}`, the RSS demo's saved
//! search, ledger #2494) had no way to put a caller's value into a query except a string splice
//! into the SPARQL text, so `q=it's "x"` ended a string literal early: an injection and a broken
//! query in one. It held: before this change `bindings=` was not declared and was silently
//! dropped, so the bound queries below answered with every row (the `bound_*` tests fail
//! against 0.3.1 on that), and the only alternative was the splice, which the last test shows
//! breaking out of its literal.
//!
//! The shape is ikigai-store's `bindings=` (a JSON object of variable name → term), so a row
//! read out of either crate's query binds into the other's unchanged.

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_sparql::Store;
use oxigraph::io::{RdfFormat, RdfParser};
use std::sync::Arc;

const G: &str = "urn:test:quakes";
const TTL: &str = r#"
<urn:test:q1> <http://ex/region> <urn:test:region:north> ; <http://ex/title> "Quake one" ; <http://ex/mag> 5 .
<urn:test:q2> <http://ex/region> <urn:test:region:south> ; <http://ex/title> "Quake two" ; <http://ex/mag> 6 .
<urn:test:q3> <http://ex/region> <urn:test:region:north> ; <http://ex/title> "it's \"x\"" ; <http://ex/mag> 7 .
"#;

struct Turtle;

#[async_trait]
impl Endpoint for Turtle {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Ok(Representation::new(ReprType::new("text/turtle"), TTL.as_bytes().to_vec()).cacheable())
    }
    fn name(&self) -> &str {
        "quakes"
    }
    fn describe(&self) -> Description {
        Description::new("quakes").verb(Verb::Source)
    }
}

/// Both variants over the same triples, with the `graph=` the per-query door needs.
fn doors() -> Vec<(&'static str, Kernel, &'static str)> {
    let store = Store::new().unwrap();
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), TTL.as_bytes())
        .unwrap();
    let shared = ikigai_sparql::space_with_store(Arc::new(store));
    let per_query: EndpointSpace = ikigai_sparql::space().bind(Exact::new(G), Turtle);
    vec![
        ("space_with_store", Kernel::new(Arc::new(shared)), ""),
        ("space", Kernel::new(Arc::new(per_query)), G),
    ]
}

fn try_query(
    kernel: &Kernel,
    form: &str,
    graph: &str,
    query: &str,
    bindings: Option<ArgRef>,
) -> Result<String> {
    let mut request = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
    .with_arg("as", ArgRef::Inline(b"text/csv".to_vec()));
    if form == "ask" {
        request = request.with_arg(
            "as",
            ArgRef::Inline(b"application/sparql-results+json".to_vec()),
        );
    }
    if !graph.is_empty() {
        request = request.with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()));
    }
    if let Some(bindings) = bindings {
        request = request.with_arg("bindings", bindings);
    }
    let repr = block_on(kernel.issue(request, &Capability::root()))?;
    Ok(String::from_utf8(repr.bytes).unwrap())
}

fn select(kernel: &Kernel, graph: &str, query: &str, bindings: Option<&str>) -> String {
    try_query(
        kernel,
        "select",
        graph,
        query,
        bindings.map(|b| ArgRef::Inline(b.as_bytes().to_vec())),
    )
    .unwrap_or_else(|e| panic!("`{query}` with {bindings:?}: {e}"))
}

fn refusal(kernel: &Kernel, graph: &str, query: &str, bindings: &str) -> String {
    match try_query(
        kernel,
        "select",
        graph,
        query,
        Some(ArgRef::Inline(bindings.as_bytes().to_vec())),
    ) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "bindings", "{detail}");
            detail
        }
        other => panic!("`{query}` with {bindings} was not refused on `bindings`: {other:?}"),
    }
}

/// The CSV answer's rows after the header, sorted, so two answers compare as sets.
fn rows(csv: &str) -> Vec<String> {
    let mut rows: Vec<String> = csv.lines().skip(1).map(str::to_string).collect();
    rows.sort();
    rows
}

const BY_REGION: &str =
    "SELECT ?q ?region ?title WHERE { ?q <http://ex/region> ?region ; <http://ex/title> ?title }";
const BY_TITLE: &str = "SELECT ?q ?title WHERE { ?q <http://ex/title> ?title }";
const SEARCH: &str =
    "SELECT ?q ?needle WHERE { ?q <http://ex/title> ?title FILTER(CONTAINS(?title, ?needle)) }";

#[test]
fn bound_iri_answers_as_the_hand_written_query() {
    for (door, kernel, graph) in doors() {
        let bound = select(
            &kernel,
            graph,
            BY_REGION,
            Some(r#"{"region": {"type": "uri", "value": "urn:test:region:north"}}"#),
        );
        let written = select(
            &kernel,
            graph,
            "SELECT ?q ?region ?title WHERE { ?q <http://ex/region> <urn:test:region:north> ; \
             <http://ex/title> ?title . BIND(<urn:test:region:north> AS ?region) }",
            None,
        );
        assert_eq!(rows(&bound).len(), 2, "{door}: {bound}");
        assert_eq!(rows(&bound), rows(&written), "{door}");
    }
}

#[test]
fn bound_literal_answers_as_the_hand_written_query() {
    for (door, kernel, graph) in doors() {
        let bound = select(&kernel, graph, BY_TITLE, Some(r#"{"title": "Quake two"}"#));
        let written = select(
            &kernel,
            graph,
            r#"SELECT ?q ?title WHERE { ?q <http://ex/title> "Quake two" . BIND("Quake two" AS ?title) }"#,
            None,
        );
        assert_eq!(
            rows(&bound),
            vec!["urn:test:q2,Quake two".to_string()],
            "{door}"
        );
        assert_eq!(rows(&bound), rows(&written), "{door}");
    }
}

#[test]
fn bound_typed_literal_and_filter_input_answer() {
    for (door, kernel, graph) in doors() {
        // A typed literal (the `since=` / `xsd:dateTime("…")` case of #2494, as an integer).
        let typed = select(
            &kernel,
            graph,
            "SELECT ?q ?mag WHERE { ?q <http://ex/mag> ?mag }",
            Some(r#"{"mag": 6}"#),
        );
        assert_eq!(rows(&typed), vec!["urn:test:q2,6".to_string()], "{door}");
        // A FILTER input that is not itself matched: the saved search of #2494.
        let search = select(
            &kernel,
            graph,
            "SELECT ?q ?needle WHERE { ?q <http://ex/title> ?t \
             FILTER(CONTAINS(LCASE(?t), LCASE(?needle))) }",
            Some(r#"{"needle": "IT'S \"X"}"#),
        );
        assert_eq!(rows(&search).len(), 1, "{door}: {search}");
        assert!(
            rows(&search)[0].starts_with("urn:test:q3,"),
            "{door}: {search}"
        );
    }
}

/// The value is a TERM: SPARQL syntax inside it is never parsed as query text.
#[test]
fn bound_value_containing_sparql_syntax_is_a_term() {
    let hostile = r#"") || true) } #"#;
    for (door, kernel, graph) in doors() {
        let json = format!(r#"{{"needle": {}}}"#, json_string(hostile));
        let bound = select(&kernel, graph, SEARCH, Some(&json));
        assert!(
            rows(&bound).is_empty(),
            "{door}: matched as syntax: {bound}"
        );
        // The quote-bearing title from #2494 matches as itself.
        let quoted = select(&kernel, graph, BY_TITLE, Some(r#"{"title": "it's \"x\""}"#));
        assert_eq!(rows(&quoted).len(), 1, "{door}: {quoted}");
        // An IRI has no escape, so `>` in one is refused, never closed early.
        let detail = refusal(
            &kernel,
            graph,
            BY_REGION,
            r#"{"region": {"type": "uri", "value": "urn:x> . ?q ?p ?o . <urn:y"}}"#,
        );
        assert!(detail.contains("not an IRI"), "{door}: {detail}");
    }
    // The splice the binding replaces: the same value written into the text does break out.
    let (_, kernel, graph) = doors().remove(0);
    let spliced = select(
        &kernel,
        graph,
        &format!(
            r#"SELECT * WHERE {{ ?q <http://ex/title> ?title FILTER(CONTAINS(?title, "{hostile}")) }}"#
        ),
        None,
    );
    assert!(
        rows(&spliced).len() > 1,
        "the splice did not inject: {spliced}"
    );
}

/// A binding is refused rather than ignored, so a filter you thought applied always does.
#[test]
fn a_binding_is_refused_rather_than_ignored() {
    for (door, kernel, graph) in doors() {
        // Not projected: oxigraph would refuse, and the refusal names the argument.
        let detail = refusal(
            &kernel,
            graph,
            "SELECT ?q WHERE { ?q <http://ex/title> ?title }",
            r#"{"title": "Quake two"}"#,
        );
        assert!(detail.contains("?title"), "{door}: {detail}");
        // Not in the query at all.
        let detail = refusal(&kernel, graph, BY_TITLE, r#"{"nowhere": "x"}"#);
        assert!(detail.contains("?nowhere"), "{door}: {detail}");
        // Malformed shapes, each refused with its reason.
        for bad in [
            "not json",
            r#"["title"]"#,
            r#"{"title": null}"#,
            r#"{"title": ["a"]}"#,
            r#"{"?title": "a"}"#,
            r#"{"title": {"type": "bnode", "value": "b0"}}"#,
            r#"{"title": {"type": "literal", "value": "a", "xml:lang": "en", "datatype": "urn:x"}}"#,
        ] {
            refusal(&kernel, graph, BY_TITLE, bad);
        }
        // Present but not inline is refused, never read as "no bindings" (every row).
        match try_query(
            &kernel,
            "select",
            graph,
            BY_TITLE,
            Some(ArgRef::Inline(vec![0xff, 0xfe])),
        ) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "bindings", "{door}"),
            other => panic!("{door}: non-UTF-8 bindings answered {other:?}"),
        }
    }
}

#[test]
fn every_query_form_binds() {
    for (door, kernel, graph) in doors() {
        let region = Some(r#"{"r": {"type": "uri", "value": "urn:test:region:south"}}"#);
        let arg = || region.map(|b| ArgRef::Inline(b.as_bytes().to_vec()));
        let ask = try_query(
            &kernel,
            "ask",
            graph,
            "ASK { <urn:test:q1> <http://ex/region> ?r }",
            arg(),
        )
        .unwrap();
        assert!(ask.contains("false"), "{door}: {ask}");
        for form in ["construct", "describe"] {
            let query = if form == "construct" {
                "CONSTRUCT { ?q <http://ex/region> ?r } WHERE { ?q <http://ex/region> ?r }"
            } else {
                // DESCRIBE projects the variables it names, so `?r` is named (measured: a bare
                // `DESCRIBE ?q` refuses a binding of `?r`, pinned below).
                "DESCRIBE ?q ?r WHERE { ?q <http://ex/region> ?r }"
            };
            let mut request = Request::new(
                Verb::Source,
                Iri::parse(format!("urn:sparql:{form}")).unwrap(),
            )
            .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
            .with_arg("as", ArgRef::Inline(b"application/n-triples".to_vec()))
            .with_arg("bindings", arg().unwrap());
            if !graph.is_empty() {
                request = request.with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()));
            }
            let out = block_on(kernel.issue(request, &Capability::root()))
                .unwrap_or_else(|e| panic!("{door} {form}: {e}"));
            let text = String::from_utf8(out.bytes).unwrap();
            assert!(text.contains("urn:test:q2"), "{door} {form}: {text}");
            assert!(!text.contains("urn:test:q1"), "{door} {form}: {text}");
        }
        let mut request = Request::new(Verb::Source, Iri::parse("urn:sparql:describe").unwrap())
            .with_arg(
                "query",
                ArgRef::Inline(b"DESCRIBE ?q WHERE { ?q <http://ex/region> ?r }".to_vec()),
            )
            .with_arg("bindings", arg().unwrap());
        if !graph.is_empty() {
            request = request.with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()));
        }
        match block_on(kernel.issue(request, &Capability::root())) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "bindings");
                assert!(detail.contains("DESCRIBE"), "{door}: {detail}");
            }
            other => panic!("{door}: DESCRIBE ?q bound ?r: {other:?}"),
        }
    }
}

#[test]
fn the_update_refuses_a_bindings() {
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space_with_store(Arc::new(
        Store::new().unwrap(),
    ))));
    let iri = Iri::parse("urn:sparql:update").unwrap();
    let request = Request::new(Verb::Sink, iri)
        .with_arg(
            "update",
            ArgRef::Inline(b"INSERT DATA { <urn:a> <urn:b> \"c\" }".to_vec()),
        )
        .with_arg("bindings", ArgRef::Inline(br#"{"x": "y"}"#.to_vec()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "bindings"),
        other => panic!("update accepted bindings: {other:?}"),
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
