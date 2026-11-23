//! A `graph=` source whose DEFAULT representation is plain JSON, and which also offers Turtle
//! when asked with `as=`, is never loaded as an empty graph (ledger #4101).
//!
//! The claim: `urn:sparql:select graph=urn:gk:repo:ikigai-cli:annotations` counted 0 triples
//! while `source … as=text/turtle` returned 18 annotations. The loader resolved the source
//! with a plain `Source` request and took its default face. oxigraph reads the media type
//! `application/json` as JSON-LD, and a JSON document with no `@context` and no IRI keys
//! expands to NO triples, so the source loaded as an empty graph with no error and a join
//! against it answered "none".
//!
//! The fix asks such a source for an RDF face first (Turtle, then N-Triples), and refuses by
//! name a plain-JSON source that still yields no triples.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Error, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::sync::{Arc, Mutex};

const SOURCE: &str = "urn:test:annotations";

const JSON: &str = r#"{"annotations":[{"subject":"urn:ex:a","note":"first"},{"subject":"urn:ex:b","note":"second"}]}"#;
const TURTLE: &str =
    "<urn:ex:a> <urn:ex:note> \"first\" .\n<urn:ex:b> <urn:ex:note> \"second\" .\n";

/// A source serving [`JSON`] by default and [`TURTLE`] for `as=` naming a type in `offers`;
/// it records the `as=` of every request it answers in `asked`.
fn kernel(offers: &'static [&'static str], asked: Arc<Mutex<Vec<Option<String>>>>) -> Kernel {
    let source = FnEndpoint::new("annotations", move |inv: &Invocation<'_>| {
        let as_type = inv.inline_str("as").ok().map(str::to_owned);
        asked.lock().unwrap().push(as_type.clone());
        let rep = match as_type.as_deref() {
            Some(t) if offers.contains(&t) => {
                Representation::new(ReprType::new(t), TURTLE.as_bytes().to_vec())
            }
            _ => Representation::new(ReprType::new("application/json"), JSON.as_bytes().to_vec()),
        };
        Ok(rep.cacheable())
    })
    .with_description(Description::new("annotations").verb(Verb::Source));
    Kernel::new(Arc::new(
        ikigai_sparql::space().bind(Exact::new(SOURCE), source),
    ))
}

fn count(k: &Kernel) -> ikigai_core::Result<String> {
    let request = Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap())
        .with_arg(
            "query",
            ArgRef::Inline(
                b"SELECT (COUNT(*) AS ?n) WHERE { GRAPH <urn:test:annotations> { ?s ?p ?o } }"
                    .to_vec(),
            ),
        )
        .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()))
        .with_arg("as", ArgRef::Inline(b"text/csv".to_vec()));
    block_on(k.issue(request, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// The reproduction: on main this answered `n\r\n0\r\n`, an empty graph with no error.
#[test]
fn a_json_default_source_offering_turtle_is_loaded_through_its_turtle_face() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let k = kernel(&["text/turtle"], Arc::clone(&asked));
    let answer = count(&k).unwrap_or_else(|e| panic!("query failed: {e}"));
    assert!(
        answer.lines().nth(1).map(str::trim) == Some("2"),
        "the source's two triples, not an empty graph: {answer:?}"
    );
    assert!(
        asked
            .lock()
            .unwrap()
            .contains(&Some("text/turtle".to_owned())),
        "the loader asked for the Turtle face: {:?}",
        asked.lock().unwrap()
    );
}

/// A source that offers only N-Triples is loaded through that face.
#[test]
fn a_json_default_source_offering_ntriples_is_loaded_through_it() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let k = kernel(&["application/n-triples"], asked);
    let answer = count(&k).unwrap_or_else(|e| panic!("query failed: {e}"));
    assert_eq!(
        answer.lines().nth(1).map(str::trim),
        Some("2"),
        "{answer:?}"
    );
}

/// A plain-JSON source with no RDF face is refused by name, never loaded as empty.
#[test]
fn a_json_source_with_no_rdf_face_is_refused_by_name() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let k = kernel(&[], asked);
    match count(&k) {
        Ok(answer) => panic!("loaded as a graph: {answer:?}"),
        Err(e) => {
            let text = e.to_string();
            assert!(text.contains(SOURCE), "names the source: {text}");
            assert!(text.contains("application/json"), "names its type: {text}");
            assert!(
                matches!(&e, Error::InvalidArgument { name, detail }
                    if name == "graph" && detail.starts_with("List a source that serves RDF")),
                "a typed refusal on `graph`, remedy first: {e:?}"
            );
        }
    }
}

/// JSON-LD served as plain `application/json`, with no RDF face, still loads: the refusal is
/// for JSON that yields no triples, not for the type.
#[test]
fn json_ld_served_as_plain_json_still_loads() {
    let source = FnEndpoint::new("jsonld", |_: &Invocation<'_>| {
        let body = r#"{"@context":{"note":"urn:ex:note"},"@id":"urn:ex:a","note":"first"}"#;
        Ok(
            Representation::new(ReprType::new("application/json"), body.as_bytes().to_vec())
                .cacheable(),
        )
    })
    .with_description(Description::new("jsonld").verb(Verb::Source));
    let k = Kernel::new(Arc::new(
        ikigai_sparql::space().bind(Exact::new(SOURCE), source),
    ));
    let answer = count(&k).unwrap_or_else(|e| panic!("query failed: {e}"));
    assert_eq!(
        answer.lines().nth(1).map(str::trim),
        Some("1"),
        "{answer:?}"
    );
}
