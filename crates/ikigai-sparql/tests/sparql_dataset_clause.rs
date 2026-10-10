//! A query's dataset clause (`FROM <g>`, `FROM NAMED <g>`) is HONORED on every query door of
//! this crate, never silently replaced by the union of every graph (ledger #840).
//!
//! The claim: through `space_with_store`, `SELECT … FROM <g> WHERE { … }` answered over the
//! whole dataset. It held, and on both variants: `evaluate` called
//! `set_default_graph_as_union()` on every query, which overwrites the default graph a `FROM`
//! clause had just set. So `FROM <g>` answered with every graph's rows, and `FROM NAMED <g>`
//! alone (whose default graph is EMPTY under SPARQL 1.1, section 13.2) answered with them too.
//! A wrong answer that looks right, with no error.
//!
//! Every test here uses only API that predates the fix, and the `from_*` and `from_named_*`
//! ones fail against 0.2.1.
//!
//! The union default graph stays, for a query that states no dataset: that convenience is what
//! makes `SELECT … WHERE { ?s ?p ?o }` span a federation, and the baseline test below pins it.

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result, Verb,
};
use ikigai_sparql::Store;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::NamedNodeRef;
use std::sync::Arc;

const G1: &str = "urn:test:g1";
const G2: &str = "urn:test:g2";
const G1_TTL: &str = "<urn:test:ada> <http://ex/name> \"Ada\" .";
const G2_TTL: &str = "<urn:test:bob> <http://ex/name> \"Bob\" .";
/// Only the shared store has a real default graph to put this in: the per-query dataset
/// loads every source as a named graph.
const DEFAULT_TTL: &str = "<urn:test:cy> <http://ex/name> \"Cy\" .";

/// A Turtle document at a fixed IRI: a `graph=` source for the per-query dataset.
struct Turtle(&'static str, &'static str);

#[async_trait]
impl Endpoint for Turtle {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Ok(
            Representation::new(ReprType::new("text/turtle"), self.1.as_bytes().to_vec())
                .cacheable(),
        )
    }
    fn name(&self) -> &str {
        self.0
    }
    fn describe(&self) -> Description {
        Description::new(self.0).verb(Verb::Source)
    }
}

/// The two variants, each holding G1 and G2; the shared store also holds `Cy` in its real
/// default graph. `graph` is what the per-query door needs to load the sources.
fn doors() -> Vec<(&'static str, Kernel, &'static str)> {
    let store = Store::new().unwrap();
    for (graph, ttl) in [(G1, G1_TTL), (G2, G2_TTL)] {
        store
            .load_from_slice(
                RdfParser::from_format(RdfFormat::Turtle)
                    .with_default_graph(NamedNodeRef::new(graph).unwrap()),
                ttl.as_bytes(),
            )
            .unwrap();
    }
    store
        .load_from_slice(
            RdfParser::from_format(RdfFormat::Turtle),
            DEFAULT_TTL.as_bytes(),
        )
        .unwrap();
    let shared = ikigai_sparql::space_with_store(Arc::new(store));

    let per_query: EndpointSpace = ikigai_sparql::space()
        .bind(Exact::new(G1), Turtle("g1", G1_TTL))
        .bind(Exact::new(G2), Turtle("g2", G2_TTL));

    vec![
        ("space_with_store", Kernel::new(Arc::new(shared)), ""),
        (
            "space",
            Kernel::new(Arc::new(per_query)),
            "urn:test:g1,urn:test:g2",
        ),
    ]
}

fn ask(kernel: &Kernel, form: &str, graph: &str, query: &str, media: &str) -> String {
    let mut request = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
    .with_arg("as", ArgRef::Inline(media.as_bytes().to_vec()));
    if !graph.is_empty() {
        request = request.with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()));
    }
    let repr = block_on(kernel.issue(request, &Capability::root()))
        .unwrap_or_else(|e| panic!("urn:sparql:{form} `{query}`: {e}"));
    String::from_utf8(repr.bytes).unwrap()
}

fn select(kernel: &Kernel, graph: &str, query: &str) -> String {
    ask(kernel, "select", graph, query, "text/csv")
}

/// Which of the three names an answer holds, in a fixed order, so a failure prints the set.
fn names(answer: &str) -> Vec<&'static str> {
    ["Ada", "Bob", "Cy"]
        .into_iter()
        .filter(|n| answer.contains(n))
        .collect()
}

/// Every door is checked before anything fails, so a failure names each door it holds on:
/// the defect lived in both variants, and a first-door panic would hide the second.
fn check(failures: &mut Vec<String>, door: &str, what: &str, got: &str, want: &[&str]) {
    if names(got) != want {
        failures.push(format!(
            "{door}: {what} answered {:?}, want {want:?}",
            names(got)
        ));
    }
}

fn verdict(failures: Vec<String>) {
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// One SELECT through every door, against the names each door should answer.
fn select_case(what: &str, query: &str, shared: &[&str], per_query: &[&str]) {
    let mut failures = Vec::new();
    for (door, kernel, graph) in doors() {
        let want = if door == "space" { per_query } else { shared };
        check(
            &mut failures,
            door,
            what,
            &select(&kernel, graph, query),
            want,
        );
    }
    verdict(failures);
}

#[test]
fn no_dataset_clause_keeps_the_union_default_graph() {
    select_case(
        "no dataset clause",
        "SELECT ?n WHERE { ?s <http://ex/name> ?n }",
        &["Ada", "Bob", "Cy"],
        &["Ada", "Bob"],
    );
}

#[test]
fn from_one_graph_answers_from_that_graph_only() {
    select_case(
        "FROM <g1>",
        "SELECT ?n FROM <urn:test:g1> WHERE { ?s <http://ex/name> ?n }",
        &["Ada"],
        &["Ada"],
    );
}

#[test]
fn from_two_graphs_merges_exactly_those() {
    select_case(
        "FROM <g1> FROM <g2>",
        "SELECT ?n FROM <urn:test:g1> FROM <urn:test:g2> WHERE { ?s <http://ex/name> ?n }",
        &["Ada", "Bob"],
        &["Ada", "Bob"],
    );
}

#[test]
fn from_a_graph_that_does_not_exist_is_empty_not_the_union() {
    select_case(
        "FROM <nowhere>",
        "SELECT ?n FROM <urn:test:nowhere> WHERE { ?s <http://ex/name> ?n }",
        &[],
        &[],
    );
}

#[test]
fn from_named_alone_leaves_the_default_graph_empty() {
    // SPARQL 1.1 section 13.2: a dataset clause with FROM NAMED and no FROM has an EMPTY
    // default graph, so a pattern outside GRAPH matches nothing.
    select_case(
        "FROM NAMED <g2> alone",
        "SELECT ?n FROM NAMED <urn:test:g2> WHERE { ?s <http://ex/name> ?n }",
        &[],
        &[],
    );
}

#[test]
fn from_named_limits_the_graphs_graph_can_reach() {
    // This one held before the fix: the override replaced the default graph, never the
    // named-graph list. It is pinned so the fix cannot trade one half for the other.
    select_case(
        "FROM NAMED <g2> with GRAPH ?g",
        "SELECT ?n FROM NAMED <urn:test:g2> WHERE { GRAPH ?g { ?s <http://ex/name> ?n } }",
        &["Bob"],
        &["Bob"],
    );
}

#[test]
fn every_query_form_honors_from() {
    // One evaluation path serves all four forms; this pins that it stays one.
    let mut failures = Vec::new();
    for (door, kernel, graph) in doors() {
        let answer = ask(
            &kernel,
            "ask",
            graph,
            "ASK FROM <urn:test:g1> { ?s <http://ex/name> \"Bob\" }",
            "application/sparql-results+json",
        );
        if !answer.contains("false") {
            failures.push(format!("{door}: ASK FROM <g1> for Bob answered {answer}"));
        }
        for (form, query) in [
            (
                "construct",
                "CONSTRUCT { ?s <http://ex/name> ?n } FROM <urn:test:g1> WHERE { ?s <http://ex/name> ?n }",
            ),
            (
                "describe",
                "DESCRIBE ?s FROM <urn:test:g1> WHERE { ?s <http://ex/name> ?n }",
            ),
        ] {
            let got = ask(&kernel, form, graph, query, "application/n-triples");
            check(&mut failures, door, form, &got, &["Ada"]);
        }
    }
    verdict(failures);
}
