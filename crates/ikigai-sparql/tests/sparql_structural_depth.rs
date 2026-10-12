//! Nesting that no bound in this crate counts does not abort a SPARQL door (ledger #1326).
//!
//! The claim (ledger #1326, from ikigai-rs/ikigai-layers PR 10): a door runs requests on a
//! tokio worker with a 2 MiB stack, a size bound does not bound recursion depth, and a recursive
//! parser over deeply nested caller input aborts the whole process. This crate bounds caller
//! SPARQL (ledger #962, `tests/sparql_nesting.rs`) and each `graph=` source's triple-term and
//! JSON-LD depth (ledger #1043, `tests/sparql_graph_depth.rs`). This file sweeps what neither
//! counts:
//!
//! - **`graph=` sources nested some other way**: Turtle blank-node property lists, collections
//!   and RDF 1.2 annotation blocks, and RDF/XML striped element nesting, loaded into the
//!   per-query dataset and answered by every query form. oxttl and oxrdfxml parse these with
//!   explicit stacks and build only blank nodes, so nothing recurses.
//! - **A triple term grown across updates.** No one request may nest a triple term deeper than
//!   64, but an update can wrap the term the store already holds in more levels, so a caller
//!   holding [`ikigai_sparql::CAP_UPDATE`] could grow it without bound, 56 levels a request
//!   here. Before the fix (`c4a7013`, a debug build) the UPDATE itself aborted the child at
//!   8,400 levels (the 150th request), though it runs on a 16 MiB thread, and every query form
//!   reading the term at 16,800. Now an update that would leave a term deeper than 64 is
//!   refused and applies nothing (`src/stored.rs`).
//!
//! Each probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in its
//! environment) on a 2 MiB thread, the size of a tokio worker's, so an abort kills the child
//! and reads here as a failure with the signal named.
//!
//! ⚠ RDF 1.2 is on (the `oxigraph/rdf-12` dev-dependency), as it is in the CLI's build.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use ikigai_sparql::Store;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_SPARQL_STRUCTURAL_DEPTH_PROBE";
const SOURCE: &str = "urn:test:deep";

/// How deep the Turtle `graph=` probes nest: 500 times the shared bound of 64.
const DEEP: usize = 32_000;
/// How deep the RDF/XML one nests: lower, because oxrdfxml 0.2.4 is quadratic in element depth
/// (see ikigai-rdf's `tests/structural_depth.rs`), which is time, not stack.
const DEEP_RDFXML: usize = 8_000;

/// A document nesting `levels` deep in the way `syntax` says, and the media type it is served
/// with.
fn document(syntax: &str, levels: usize) -> (&'static str, String) {
    let turtle = |open: &str, middle: &str, close: &str| {
        (
            "text/turtle",
            format!(
                "@prefix ex: <urn:ex:> .\nex:s ex:p {}{middle}{} .\n",
                open.repeat(levels),
                close.repeat(levels)
            ),
        )
    };
    match syntax {
        "bnode" => turtle("[ ex:p ", "ex:o", " ]"),
        "collection" => turtle("( ", "ex:o", " )"),
        "annotation" => turtle(
            "",
            &format!("ex:o {}", "{| ex:q ex:r ".repeat(levels)),
            " |}",
        ),
        "rdfxml-striped" => (
            "application/rdf+xml",
            format!(
                "<?xml version=\"1.0\"?>\n<rdf:RDF \
                 xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:ex:\">\
                 <rdf:Description rdf:about=\"urn:ex:s\">{}<ex:p rdf:resource=\"urn:ex:o\"/>{}\
                 </rdf:Description></rdf:RDF>\n",
                "<ex:p><rdf:Description>".repeat(levels),
                "</rdf:Description></ex:p>".repeat(levels)
            ),
        ),
        other => panic!("no syntax named {other}"),
    }
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

const FORMS: [&str; 4] = ["select", "ask", "construct", "describe"];

/// `urn:sparql:{form}` over `graph=<SOURCE>`, the source serving `syntax` at `levels`.
fn issue_graph(form: &str, syntax: &str, levels: usize) -> ikigai_core::Result<String> {
    let (media, body) = document(syntax, levels);
    let deep = FnEndpoint::new("deep", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new(media),
            body.clone().into_bytes(),
        ))
    })
    .with_description(Description::new("deep").verb(Verb::Source));
    let kernel = Kernel::new(Arc::new(
        ikigai_sparql::space().bind(Exact::new(SOURCE), deep),
    ));
    let req = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(query(form).as_bytes().to_vec()))
    .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()));
    block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// Levels one update adds to the stored term: inside the 64 one request may nest.
const LEVELS_PER_UPDATE: usize = 56;

/// The update that wraps the term at `<urn:a> <urn:p>` in `levels` more `TRIPLE(…)` levels.
/// `TRIPLE(…)` calls, not `<<( … )>>`: the SPARQL scan cannot bound a `<` inside parentheses
/// with an IRI after it, and refuses 56 of those as too ambiguous before it would refuse depth.
fn wrap(levels: usize) -> String {
    format!(
        "DELETE {{ <urn:a> <urn:p> ?t }} INSERT {{ <urn:a> <urn:p> ?n }} \
         WHERE {{ <urn:a> <urn:p> ?t BIND({}?t{} AS ?n) }}",
        "TRIPLE(<urn:s>, <urn:p>, ".repeat(levels),
        ")".repeat(levels)
    )
}

/// A shared store holding `<urn:a> <urn:p> <urn:o>`, and a kernel over it.
fn shared() -> (Kernel, Arc<Store>) {
    let store = Arc::new(Store::new().unwrap());
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space_with_store(Arc::clone(
        &store,
    ))));
    update(
        &kernel,
        "INSERT DATA { <urn:a> <urn:p> <urn:o> }".to_string(),
    )
    .unwrap();
    (kernel, store)
}

fn update(kernel: &Kernel, text: String) -> ikigai_core::Result<String> {
    block_on(
        kernel.issue(
            Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap())
                .with_arg("content", ArgRef::Inline(text.into_bytes())),
            &Capability::root(),
        ),
    )
    .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

fn ask(kernel: &Kernel, text: &str) -> String {
    let req = Request::new(Verb::Source, Iri::parse("urn:sparql:ask").unwrap())
        .with_arg("query", ArgRef::Inline(text.as_bytes().to_vec()));
    block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
        .unwrap()
}

/// Whether the stored term at `<urn:a> <urn:p>` nests at least `levels` deep: `OBJECT`
/// applied `levels - 1` times still yields a triple. Asked of the store directly, past the
/// endpoint's nesting bound, on a thread big enough for 64 nested function calls in a debug
/// build.
fn at_least(store: &Arc<Store>, levels: usize) -> bool {
    let store = Arc::clone(store);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let query = format!(
                "ASK {{ <urn:a> <urn:p> ?t FILTER(isTRIPLE({}?t{})) }}",
                "OBJECT(".repeat(levels - 1),
                ")".repeat(levels - 1)
            );
            match SparqlEvaluator::new()
                .parse_query(&query)
                .unwrap()
                .on_store(&store)
                .execute()
                .unwrap()
            {
                QueryResults::Boolean(answer) => answer,
                _ => panic!("ASK answers a boolean"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

/// Try to grow `<urn:a> <urn:p> ?t` in a shared store by `rounds` updates of
/// [`LEVELS_PER_UPDATE`] levels each, carrying on past refusals as an attacker would, then read
/// it back with `form`. Says which rounds were refused, the first refusal, and the read.
fn issue_grown(form: &str, rounds: usize) -> ikigai_core::Result<String> {
    let (kernel, store) = shared();
    let mut refused = Vec::new();
    let mut first = None;
    for round in 1..=rounds {
        if let Err(e) = update(&kernel, wrap(LEVELS_PER_UPDATE)) {
            refused.push(round);
            first.get_or_insert(e.to_string());
        }
    }
    let text = match form {
        "select" => "SELECT ?t WHERE { <urn:a> <urn:p> ?t }",
        "ask" => "ASK { <urn:a> <urn:p> ?t }",
        "construct" => "CONSTRUCT { <urn:a> <urn:p> ?t } WHERE { <urn:a> <urn:p> ?t }",
        "describe" => "DESCRIBE <urn:a>",
        other => panic!("no form named {other}"),
    };
    let req = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(text.as_bytes().to_vec()));
    let read = block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| rep.bytes.len())
        .map_err(|e| e.to_string());
    Ok(format!(
        "refused {} of {rounds} from round {:?}; depth {}; read {read:?}; first refusal: {}",
        refused.len(),
        refused.first(),
        if at_least(&store, LEVELS_PER_UPDATE) && !at_least(&store, LEVELS_PER_UPDATE + 1) {
            "56"
        } else {
            "other"
        },
        first.unwrap_or_default()
    ))
}

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, prints
/// the outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.split(':');
    let kind = parts.next().unwrap().to_string();
    let form = parts.next().unwrap().to_string();
    let syntax = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let answer = if kind == "graph" {
                issue_graph(&form, &syntax, n)
            } else {
                issue_grown(&form, n)
            };
            match answer {
                Ok(text) if kind == "grown" => format!("ok {text}"),
                Ok(text) => format!(
                    "ok {} bytes, subject {}",
                    text.len(),
                    text.contains("urn:ex:s") || text.contains("urn:a") || text.contains("true")
                ),
                Err(e) => format!("err {}", e.to_string().replace('\n', " ")),
            }
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: run one probe in a child and return what it said, or `Err` naming how
/// it died. An abort is the defect.
fn try_probe(kind: &str, form: &str, syntax: &str, n: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{kind}:{form}:{syntax}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "{kind} `{syntax}` at {n} through urn:sparql:{form} did not survive a 2 MiB thread: \
             {} — {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout.find("\nOUTCOME ").ok_or_else(|| {
        format!("the {kind} `{form}` `{syntax}` probe reported nothing: {stdout}")
    })?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

#[test]
fn deep_graph_sources_nested_without_triple_terms_are_answered_by_every_form() {
    let mut failed = Vec::new();
    for (syntax, levels) in [
        ("bnode", DEEP),
        ("collection", DEEP),
        ("annotation", DEEP),
        ("rdfxml-striped", DEEP_RDFXML),
    ] {
        for form in FORMS {
            match try_probe("graph", form, syntax, levels) {
                Ok(outcome) if outcome.starts_with("ok ") && outcome.ends_with("true") => {}
                Ok(outcome) => failed.push(format!("{syntax} {form}: {outcome}")),
                Err(died) => failed.push(died),
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The reproduction: 300 updates of 56 levels, 16,800 levels if every one applied, which
/// aborted the child before the fix. Now the first applies (56 levels), every later one is
/// refused on `content` naming the bound, the store keeps the 56-level term, and every query
/// form reads it.
#[test]
fn a_triple_term_grown_across_updates_is_refused_past_the_bound_and_aborts_nothing() {
    let mut died = Vec::new();
    for form in FORMS {
        match try_probe("grown", form, "-", 300) {
            Ok(outcome) => assert!(
                outcome.starts_with("ok ")
                    && outcome
                        .contains("refused 299 of 300 from round Some(2); depth 56; read Ok(")
                    && outcome.contains("invalid argument `content`")
                    && outcome.contains("deeper than 64 (MAX_TURTLE_NESTING)"),
                "{form}: {outcome}"
            ),
            Err(e) => died.push(e),
        }
    }
    assert!(died.is_empty(), "{}", died.join("\n"));
}

/// At the bound an update commits; one level past it the update is refused and the store is
/// exactly as it was. The last step is a `<<( … )>>` template rather than `TRIPLE(…)`, so both
/// ways of building a term reach the check.
#[test]
fn an_update_reaching_the_bound_commits_and_one_past_it_applies_nothing() {
    let (kernel, store) = shared();
    update(&kernel, wrap(LEVELS_PER_UPDATE)).unwrap();
    update(&kernel, wrap(64 - LEVELS_PER_UPDATE)).unwrap();
    assert!(at_least(&store, 64) && !at_least(&store, 65));
    let past =
        "DELETE { <urn:a> <urn:p> ?t } INSERT { <urn:a> <urn:p> <<( <urn:s> <urn:p> ?t )>> } \
                WHERE { <urn:a> <urn:p> ?t }";
    let refusal = update(&kernel, past.to_string()).unwrap_err().to_string();
    assert!(
        refusal.starts_with("invalid argument `content`")
            && refusal.contains("nothing was applied"),
        "{refusal}"
    );
    assert!(at_least(&store, 64) && !at_least(&store, 65));
    // The DELETE half was rolled back with the INSERT: the triple is still there.
    assert!(ask(&kernel, "ASK { <urn:a> <urn:p> ?t }").contains("true"));
    // Named by `update=` when the text came that way.
    let named = block_on(
        kernel.issue(
            Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap())
                .with_arg("update", ArgRef::Inline(past.as_bytes().to_vec())),
            &Capability::root(),
        ),
    )
    .unwrap_err()
    .to_string();
    assert!(named.starts_with("invalid argument `update`"), "{named}");
}

/// An update that builds no triple term is not checked, and one that builds a shallow one in a
/// named graph commits.
#[test]
fn ordinary_updates_still_commit() {
    let (kernel, _) = shared();
    let receipt = update(
        &kernel,
        "INSERT DATA { <urn:s> <urn:p> \"o\" . GRAPH <urn:g> { <urn:s> <urn:p> <urn:o> } }"
            .to_string(),
    )
    .unwrap();
    assert!(receipt.starts_with("updated: 1 -> 3 quads"), "{receipt}");
    update(
        &kernel,
        "INSERT { GRAPH <urn:g> { <urn:s> <urn:q> <<( <urn:s> <urn:p> ?o )>> } } \
         WHERE { GRAPH <urn:g> { <urn:s> <urn:p> ?o } }"
            .to_string(),
    )
    .unwrap();
    assert!(ask(
        &kernel,
        "ASK { GRAPH <urn:g> { <urn:s> <urn:q> ?t FILTER(isTRIPLE(?t)) } }"
    )
    .contains("true"));
}
