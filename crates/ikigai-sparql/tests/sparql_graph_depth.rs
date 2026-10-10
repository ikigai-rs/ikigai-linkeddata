//! A `graph=` source that nests deeply is refused before it is loaded, and every source is
//! loaded on a sized stack, at every query door of the per-query dataset (ledger #1043).
//!
//! The claim: `urn:sparql:*` resolves each `graph=` source through the kernel and loads it
//! with oxigraph's parsers on the CALLER'S thread, with no depth scan. Those parsers recurse
//! (ledger #992 for RDF 1.2 triple terms in Turtle and RDF/XML, ledger #1038 for JSON-LD node
//! objects), and a stack overflow aborts the WHOLE process, on any thread. ikigai-rdf's doors
//! were fixed for both; this door, which reads the same syntaxes, was not. Measured against the
//! code before the fix (`06ee94e`, a debug build, 2 MiB thread): every probe below that
//! expects a refusal aborted the child instead.
//!
//! Every probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in
//! its environment) on a 2 MiB thread, the size of a tokio worker's, so an abort kills the
//! child and reads here as a failure with the signal named.
//!
//! ⚠ **RDF 1.2 is on in these tests, as it is in the CLI** (the `oxigraph/rdf-12`
//! dev-dependency): without it `<<(` is a syntax error and nothing recurses.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_SPARQL_GRAPH_DEPTH_PROBE";
/// The bound, restated so these tests read the same against the code that predates it.
const BOUND: usize = 64;
const SOURCE: &str = "urn:test:deep";

/// A document in `syntax` whose one statement's object nests `levels` deep, and the media type
/// it is served with.
fn document(syntax: &str, levels: usize) -> (&'static str, String) {
    let nested = format!(
        "{}<urn:ex:o>{}",
        "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " )>>".repeat(levels)
    );
    match syntax {
        "turtle" => ("text/turtle", format!("<urn:ex:s> <urn:ex:p> {nested} .\n")),
        "ntriples" => (
            "application/n-triples",
            format!("<urn:ex:s> <urn:ex:p> {nested} .\n"),
        ),
        "nquads" => (
            "application/n-quads",
            format!("<urn:ex:s> <urn:ex:p> {nested} <urn:ex:g> .\n"),
        ),
        "trig" => (
            "application/trig",
            format!("<urn:ex:s> <urn:ex:p> {nested} .\n"),
        ),
        // Served with a type that names no RDF syntax, so the door SNIFFS it (as Turtle).
        "sniffed-turtle" => (
            "application/octet-stream",
            format!("<urn:ex:s> <urn:ex:p> {nested} .\n"),
        ),
        // `rdf:version="1.2"`: without it oxrdfxml drops a `parseType="Triple"` silently. The
        // `rdf:annotation` on the outermost element makes the parser itself copy the whole term.
        "rdfxml" => {
            let element = |extra: &str| {
                format!(
                    "<ex:p rdf:parseType=\"Triple\"{extra}><rdf:Description rdf:about=\"urn:ex:s\">"
                )
            };
            let open = if levels == 0 {
                String::new()
            } else {
                element(" rdf:annotation=\"urn:ex:a\"") + &element("").repeat(levels - 1)
            };
            let close = "</rdf:Description></ex:p>".repeat(levels);
            (
                "application/rdf+xml",
                format!(
                    "<?xml version=\"1.0\"?>\n<rdf:RDF \
                     xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
                     xmlns:ex=\"urn:ex:\" rdf:version=\"1.2\"><rdf:Description \
                     rdf:about=\"urn:ex:s\">{open}<ex:p rdf:resource=\"urn:ex:o\"/>{close}\
                     </rdf:Description></rdf:RDF>\n"
                ),
            )
        }
        // Node objects nested `levels` deep: `levels + 1` JSON levels with the innermost node.
        "jsonld" | "sniffed-jsonld" => (
            if syntax == "jsonld" {
                "application/ld+json"
            } else {
                "application/octet-stream"
            },
            format!(
                "{}{{\"@id\":\"urn:ex:o\"}}{}",
                "{\"@id\":\"urn:ex:s\",\"urn:ex:p\":".repeat(levels),
                "}".repeat(levels)
            ),
        ),
        other => panic!("no syntax named {other}"),
    }
}

/// The per-query space, plus [`SOURCE`] serving `body` as `media`.
fn kernel(media: &'static str, body: String) -> Kernel {
    let deep = FnEndpoint::new("deep", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new(media),
            body.clone().into_bytes(),
        ))
    })
    .with_description(Description::new("deep").verb(Verb::Source));
    Kernel::new(Arc::new(
        ikigai_sparql::space().bind(Exact::new(SOURCE), deep),
    ))
}

fn query(form: &str) -> &'static str {
    match form {
        "select" => "SELECT * WHERE { ?s ?p ?o }",
        "ask" => "ASK { ?s ?p ?o }",
        "construct" => "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        "describe" => "DESCRIBE <urn:ex:s>",
        other => panic!("no form named {other}"),
    }
}

/// `urn:sparql:{form}` over `graph=<SOURCE>`, the source serving `syntax` at `levels`.
fn issue(form: &str, syntax: &str, levels: usize) -> ikigai_core::Result<String> {
    let (media, body) = document(syntax, levels);
    let req = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(query(form).as_bytes().to_vec()))
    .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()));
    block_on(kernel(media, body).issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, a tokio
/// worker's stack, prints the outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.split(':');
    let form = parts.next().unwrap().to_string();
    let syntax = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || match issue(&form, &syntax, n) {
            Ok(text) => format!("ok {}", text.replace('\n', " ")),
            Err(e) => format!("err {}", e.to_string().replace('\n', " ")),
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: run one probe in a child and return what it said, or `Err` naming how
/// it died. An abort is the defect.
fn try_probe(form: &str, syntax: &str, n: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{form}:{syntax}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "`{syntax}` at {n} through urn:sparql:{form} graph= did not survive a 2 MiB thread: \
             {} — {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("the `{form}` `{syntax}` probe reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

fn probe(form: &str, syntax: &str, n: usize) -> String {
    try_probe(form, syntax, n).unwrap_or_else(|died| panic!("{died}"))
}

/// A refusal on `graph`, naming the source and the bound.
fn assert_refused(outcome: &str, bound_name: &str, what: &str) {
    assert!(
        outcome.starts_with("err invalid argument `graph`")
            && outcome.contains(&format!("<{SOURCE}>"))
            && outcome.contains(&format!("deeper than 64 ({bound_name})")),
        "{what}: {outcome}"
    );
}

const FORMS: [&str; 4] = ["select", "ask", "construct", "describe"];

// ------------------------------------------------------------------ the reproduction

#[test]
fn three_thousand_triple_terms_are_refused_at_every_query_form_and_abort_nothing() {
    for form in FORMS {
        assert_refused(&probe(form, "turtle", 3000), "MAX_TURTLE_NESTING", form);
    }
}

#[test]
fn every_syntax_that_nests_triple_terms_is_refused() {
    for syntax in ["ntriples", "nquads", "trig", "sniffed-turtle", "rdfxml"] {
        assert_refused(&probe("select", syntax, 3000), "MAX_TURTLE_NESTING", syntax);
    }
}

/// oxjsonld's expander recurses once per nested node object: ~200 levels aborted a debug build
/// through this door. Refused past `MAX_JSON_NESTING`, counted in JSON levels, whether the
/// source says it is JSON-LD or is sniffed as it.
#[test]
fn deep_json_ld_is_refused_and_aborts_nothing() {
    for form in FORMS {
        assert_refused(&probe(form, "jsonld", 200), "MAX_JSON_NESTING", form);
    }
    assert_refused(
        &probe("select", "sniffed-jsonld", 3000),
        "MAX_JSON_NESTING",
        "sniffed",
    );
}

// ------------------------------------------------------------------ at the bound

/// At the bound the door answers, on the 2 MiB thread the probe gives it: a debug build needs
/// ~4.5 MiB to expand 64 JSON levels, so the JSON-LD case passes only because the load runs on
/// a sized thread of its own. `document("jsonld", n)` nests `n + 1` JSON levels.
#[test]
fn a_source_at_the_bound_is_answered_on_a_small_thread_and_one_past_is_refused() {
    for (syntax, at_bound, bound_name) in [
        ("jsonld", BOUND - 1, "MAX_JSON_NESTING"),
        ("turtle", BOUND, "MAX_TURTLE_NESTING"),
        ("rdfxml", BOUND, "MAX_TURTLE_NESTING"),
    ] {
        let ok = probe("select", syntax, at_bound);
        assert!(
            ok.starts_with("ok ") && ok.contains("urn:ex:o"),
            "{syntax} at the bound: {ok}"
        );
        assert_refused(&probe("select", syntax, at_bound + 1), bound_name, syntax);
    }
}

#[test]
fn brackets_in_literals_are_not_nesting() {
    let deep = "<<( [{".repeat(500);
    let turtle = format!("<urn:ex:s> <urn:ex:p> \"{deep}\" .\n");
    let json = format!("{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":\"{deep}\"}}");
    for (media, body) in [("text/turtle", turtle), ("application/ld+json", json)] {
        let req = Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap())
            .with_arg("query", ArgRef::Inline(query("select").as_bytes().to_vec()))
            .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()));
        let answer = block_on(kernel(media, body).issue(req, &Capability::root()))
            .unwrap_or_else(|e| panic!("{media}: {e}"));
        assert!(
            String::from_utf8_lossy(&answer.bytes).contains("urn:ex:s"),
            "{media}"
        );
    }
}
