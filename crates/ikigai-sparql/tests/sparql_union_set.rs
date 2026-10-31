//! The union default graph is a SET (ledger #2715): RDF merge of documents is a set union, so a
//! triple two `graph=` sources both hold is one triple of the union, and a source listed twice is
//! the same source. Each source still loads as its own named graph, so `GRAPH <g>` keeps seeing
//! exactly that source's triples, and a blank node stays its document's own.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::sync::Arc;

/// Two feeds that share a category node and a link target, and each name one item of its own.
const A: &str = r#"@prefix ex: <urn:ex:> .
ex:a1 a ex:Item ; ex:category ex:rust ; ex:link ex:shared .
ex:rust ex:label "Rust" .
ex:shared ex:title "Shared" .
[] ex:note "a's anonymous note" .
"#;
const B: &str = r#"@prefix ex: <urn:ex:> .
ex:b1 a ex:Item ; ex:category ex:rust ; ex:link ex:shared .
ex:rust ex:label "Rust" .
ex:shared ex:title "Shared" .
"#;

fn kernel() -> Kernel {
    let mut graphs = EndpointSpace::new();
    for (name, ttl) in [("a", A), ("b", B)] {
        graphs = graphs.bind(
            Exact::new(format!("urn:test:{name}")),
            FnEndpoint::new("fixture", move |_| {
                Ok(
                    Representation::new(ReprType::new("text/turtle"), ttl.as_bytes().to_vec())
                        .cacheable(),
                )
            }),
        );
    }
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(graphs),
        Arc::new(ikigai_sparql::space()),
    ])))
}

/// The CSV body of a SELECT over `graph`, its header dropped.
fn select(query: &str, graph: &str) -> Vec<String> {
    let request = [("query", query), ("graph", graph), ("as", "text/csv")]
        .iter()
        .fold(
            Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap()),
            |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
        );
    let csv = block_on(kernel().issue(request, &Capability::root())).unwrap();
    String::from_utf8(csv.bytes)
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn count(query: &str, graph: &str) -> u64 {
    let rows = select(query, graph);
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows[0].trim().parse().unwrap()
}

/// Only the sources' triples: the vocabulary graph is always loaded beside them.
const ALL: &str = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o FILTER(STRSTARTS(STR(?p), \"urn:ex:\") || STRSTARTS(STR(?o), \"urn:ex:\")) }";

#[test]
fn a_triple_two_sources_hold_is_one_row_of_the_union() {
    let rows = select(
        "SELECT ?l WHERE { <urn:ex:rust> <urn:ex:label> ?l }",
        "urn:test:a,urn:test:b",
    );
    assert_eq!(rows, ["Rust"]);
    // a: 6 triples, b: 5, of which 2 are a's too.
    assert_eq!(count(ALL, "urn:test:a"), 6);
    assert_eq!(count(ALL, "urn:test:b"), 5);
    assert_eq!(count(ALL, "urn:test:a,urn:test:b"), 9);
}

#[test]
fn a_join_through_a_shared_node_counts_each_item_once() {
    let q = "SELECT ?item WHERE { ?item <urn:ex:category> ?c . ?c <urn:ex:label> \"Rust\" . \
             ?item <urn:ex:link> ?l . ?l <urn:ex:title> \"Shared\" } ORDER BY ?item";
    assert_eq!(
        select(q, "urn:test:a,urn:test:b"),
        ["urn:ex:a1", "urn:ex:b1"]
    );
}

#[test]
fn a_source_listed_twice_is_the_same_source() {
    for graph in ["urn:test:a,urn:test:a", "urn:test:a urn:test:b urn:test:a"] {
        let once = if graph.contains('b') {
            "urn:test:a,urn:test:b"
        } else {
            "urn:test:a"
        };
        assert_eq!(count(ALL, graph), count(ALL, once), "{graph}");
        // The anonymous note too: a repeat is not a second document with a second blank node.
        let notes = "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:ex:note> ?o }";
        assert_eq!(count(notes, graph), 1, "{graph}");
    }
}

#[test]
fn each_source_stays_its_own_named_graph() {
    let q = "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <urn:test:b> { ?s ?p ?o } }";
    assert_eq!(count(q, "urn:test:a,urn:test:b"), 5);
    let q = "SELECT ?g WHERE { GRAPH ?g { <urn:ex:rust> <urn:ex:label> \"Rust\" } } ORDER BY ?g";
    assert_eq!(
        select(q, "urn:test:a,urn:test:b"),
        ["urn:test:a", "urn:test:b"]
    );
    assert_eq!(
        select(q, "urn:test:b,urn:test:a,urn:test:b"),
        ["urn:test:a", "urn:test:b"]
    );
}
