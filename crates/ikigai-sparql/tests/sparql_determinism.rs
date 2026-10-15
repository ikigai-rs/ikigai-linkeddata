//! Every cacheable SPARQL answer is the same bytes on every fresh kernel (ledger #1566).
//!
//! The per-query dataset's answers are `.cacheable()`, and `ikigai-conformance`'s RECOMPUTE
//! check (ikigai-rs/ikigai-conformance PR 39) holds a cacheable answer to repeating its bytes
//! when it is recomputed. A query is answered over a FRESH in-memory store each time, so
//! whatever order or label that store's hashing picks is a fresh choice too.
//!
//! Two causes, both fresh per store. `Store::load_from_slice` renames every blank node of a
//! source to a RANDOM label, and the store's order follows that label, so every answer
//! that reached one (a selected `[ … ]`, a DESCRIBE of a subject that has one, even an
//! `ORDER BY` over one) differed. And a CONSTRUCT template's blank nodes are minted at random
//! for each solution. Sources are now relabeled in document order as they load, and a
//! CONSTRUCT or DESCRIBE graph is sorted with the query's own blank nodes labeled by structure
//! (`ikigai_rdf::canonical_triples`).
//!
//! - **SELECT**: a sequence, and its row order without `ORDER BY` is the query's business. Once
//!   the labels were fixed, the order measured stable on 8 fresh stores for every query tried
//!   (oxigraph's in-memory store hashes terms with fixed keys), with one exception:
//!   `SELECT ?g WHERE { GRAPH ?g {} }` enumerates the store's graph map, a `DashMap` under a
//!   per-store `RandomState`, and gave 8 orders in 8 runs over six sources. Not sorted here: a
//!   query that wants an order says `ORDER BY`, and no sort after the fact could repair a
//!   `LIMIT` that chose different rows.
//! - **ASK**: one boolean.
//!
//! Measured against the code before the fix (`dce6a6b`): every test here but
//! `ask_is_identical_across_fresh_kernels` failed, each with 8 distinct answers in 8 runs.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::collections::BTreeSet;
use std::sync::Arc;

const SOURCE: &str = "urn:test:graph";
const RUNS: usize = 8;

/// Named subjects enough that two hash orders cannot agree by luck, and anonymous nodes the
/// Turtle parser labels at random.
fn graph() -> String {
    let mut text = String::from("@prefix ex: <urn:ex:> .\n");
    for i in 0..20 {
        text.push_str(&format!(
            "<urn:ex:s:{i}> ex:value \"{i}\" ; ex:next <urn:ex:s:{}> .\n",
            i + 1
        ));
    }
    text.push_str("<urn:ex:s:0> ex:knows [ ex:name \"b\" ; ex:knows [ ex:name \"c\" ] ] .\n");
    text
}

fn kernel() -> Kernel {
    let body = graph();
    let source = FnEndpoint::new("graph", move |_: &Invocation<'_>| {
        Ok(
            Representation::new(ReprType::new("text/turtle"), body.clone().into_bytes())
                .cacheable(),
        )
    })
    .with_description(Description::new("graph").verb(Verb::Source));
    Kernel::new(Arc::new(
        ikigai_sparql::space().bind(Exact::new(SOURCE), source),
    ))
}

fn answers(form: &str, query: &str, as_type: Option<&str>) -> BTreeSet<String> {
    (0..RUNS)
        .map(|_| {
            let mut request = Request::new(
                Verb::Source,
                Iri::parse(format!("urn:sparql:{form}")).unwrap(),
            )
            .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
            .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()));
            if let Some(as_type) = as_type {
                request = request.with_arg("as", ArgRef::Inline(as_type.as_bytes().to_vec()));
            }
            let answer = block_on(kernel().issue(request, &Capability::root()))
                .unwrap_or_else(|e| panic!("{form} {query}: {e}"));
            String::from_utf8(answer.bytes).unwrap()
        })
        .collect()
}

fn assert_one(form: &str, query: &str, as_type: Option<&str>) -> String {
    let distinct = answers(form, query, as_type);
    assert_eq!(
        distinct.len(),
        1,
        "{form} {query} as={as_type:?} gave {} distinct answers on {RUNS} fresh kernels:\n{}",
        distinct.len(),
        distinct
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n----\n")
    );
    distinct.into_iter().next().unwrap()
}

const GRAPH_FACES: [Option<&str>; 7] = [
    None,
    Some("text/turtle"),
    Some("application/n-triples"),
    Some("application/n-quads"),
    Some("application/trig"),
    Some("application/rdf+xml"),
    Some("application/ld+json"),
];

#[test]
fn construct_is_identical_across_fresh_kernels() {
    let query = "CONSTRUCT { ?s ?p ?o } WHERE { GRAPH <urn:test:graph> { ?s ?p ?o } }";
    for face in GRAPH_FACES {
        assert_one("construct", query, face);
    }
}

#[test]
fn construct_with_a_template_blank_node_is_identical_across_fresh_kernels() {
    // A template blank node is minted fresh for every solution.
    let query = "PREFIX ex: <urn:ex:> \
                 CONSTRUCT { ?s ex:tag [ ex:of ?v ] } WHERE { GRAPH <urn:test:graph> { ?s ex:value ?v } }";
    let answer = assert_one("construct", query, Some("application/n-triples"));
    assert_eq!(answer.lines().count(), 40, "{answer}");
}

#[test]
fn describe_is_identical_across_fresh_kernels() {
    let query = "DESCRIBE <urn:ex:s:0>";
    for face in GRAPH_FACES {
        assert_one("describe", query, face);
    }
}

#[test]
fn select_with_order_by_is_identical_across_fresh_kernels() {
    let query = "SELECT ?s ?o WHERE { GRAPH <urn:test:graph> { ?s ?p ?o } } ORDER BY ?s ?o";
    for face in [
        None,
        Some("text/csv"),
        Some("application/sparql-results+xml"),
    ] {
        assert_one("select", query, face);
    }
}

#[test]
fn a_selected_anonymous_node_has_the_same_label_on_every_fresh_kernel() {
    let query = "PREFIX ex: <urn:ex:> \
                 SELECT ?b WHERE { GRAPH <urn:test:graph> { <urn:ex:s:0> ex:knows ?b } }";
    assert_one("select", query, Some("text/csv"));
}

#[test]
fn ask_is_identical_across_fresh_kernels() {
    let query = "ASK { GRAPH <urn:test:graph> { ?s ?p ?o } }";
    assert_one("ask", query, None);
}
