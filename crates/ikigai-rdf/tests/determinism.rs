//! Every cacheable face here answers the same bytes for the same request, on every fresh
//! kernel (ledger #1566).
//!
//! A `.cacheable()` answer is a promise that recomputing it gives the same bytes:
//! `ikigai-conformance`'s RECOMPUTE check (ikigai-rs/ikigai-conformance PR 39) cuts a cached
//! answer's threads, recomputes it and compares. `urn:rdf:union` broke that promise: it
//! serialized an `oxrdf::Graph` in iteration order, and that order follows the interner's
//! per-graph `RandomState`, so 8 runs on fresh kernels gave 4 distinct outputs. `urn:rdf:diff`
//! did the same. And every face, `urn:rdf:transrept` included, passed blank-node labels through
//! from the parsers, which mint a RANDOM label for every anonymous node (`[ … ]`, a collection,
//! a JSON-LD node object without `@id`, an RDF/XML node element without `rdf:about`).
//!
//! Measured against the code before the fix (`dce6a6b`): every test here but
//! `transrept_keeps_document_order` failed, each with 8 distinct answers in 8 runs. That one is
//! a pin, not a regression test: transreption streams in document order and was never the
//! order problem, only the label one. And `ikigai-conformance` main's RECOMPUTE walk
//! (`tests/conformance.rs`, run against it through an uncommitted `--config` patch) failed
//! `rdf-union` before the fix and passes after it.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::collections::BTreeSet;
use std::sync::Arc;

/// How many fresh kernels each face is resolved on. Union varied 4 ways in 8 runs; with the
/// graphs below a single repeat already differs nearly every time.
const RUNS: usize = 8;

/// Enough distinct subjects that a hash order and a sorted order cannot agree by luck.
fn named_graph(prefix: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("<urn:{prefix}:{i}> <urn:p:value> \"{i}\" .\n"))
        .collect()
}

/// Anonymous nodes the Turtle parser labels at random: a `[ … ]`, and a collection.
const ANONYMOUS: &str = "@prefix ex: <urn:ex:> .\n\
ex:a ex:knows [ ex:name \"b\" ; ex:knows [ ex:name \"c\" ] ] .\n\
ex:a ex:list ( ex:x ex:y ) .\n\
_:labeled ex:name \"d\" .\n";

/// The same shape as JSON-LD: node objects without `@id` are blank nodes.
const ANONYMOUS_JSON_LD: &str = r#"{"@context": {"ex": "urn:ex:"},
 "@id": "ex:a", "ex:knows": {"ex:name": "b", "ex:knows": {"ex:name": "c"}}}"#;

/// And as RDF/XML: node elements without `rdf:about`.
const ANONYMOUS_RDF_XML: &str = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ex="urn:ex:">
  <rdf:Description rdf:about="urn:ex:a">
    <ex:knows><rdf:Description><ex:name>b</ex:name></rdf:Description></ex:knows>
  </rdf:Description>
</rdf:RDF>"#;

/// Resolve `iri` with `args` on `RUNS` fresh kernels and return the distinct answers.
fn answers(iri: &str, args: &[(&str, &str)]) -> BTreeSet<String> {
    (0..RUNS)
        .map(|_| {
            let kernel = Kernel::new(Arc::new(ikigai_rdf::space()));
            let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
            for (key, value) in args {
                request = request.with_arg(*key, ArgRef::Inline(value.as_bytes().to_vec()));
            }
            let answer = block_on(kernel.issue(request, &Capability::root()))
                .unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"));
            String::from_utf8(answer.bytes).unwrap()
        })
        .collect()
}

fn assert_one(iri: &str, args: &[(&str, &str)]) -> String {
    let distinct = answers(iri, args);
    assert_eq!(
        distinct.len(),
        1,
        "{iri} {args:?} gave {} distinct answers on {RUNS} fresh kernels:\n{}",
        distinct.len(),
        distinct
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n----\n")
    );
    distinct.into_iter().next().unwrap()
}

#[test]
fn union_is_identical_across_fresh_kernels() {
    let content = named_graph("a", 20);
    let with = named_graph("b", 20);
    let answer = assert_one("urn:rdf:union", &[("content", &content), ("with", &with)]);
    assert_eq!(answer.lines().count(), 40, "{answer}");
}

#[test]
fn diff_is_identical_across_fresh_kernels() {
    let content = named_graph("a", 30);
    let with = named_graph("a", 10);
    for mode in ["added", "removed"] {
        assert_one(
            "urn:rdf:diff",
            &[("content", &content), ("with", &with), ("mode", mode)],
        );
    }
    let added = assert_one("urn:rdf:diff", &[("content", &content), ("with", &with)]);
    assert_eq!(added.lines().count(), 20, "{added}");
}

#[test]
fn union_with_anonymous_nodes_is_identical_across_fresh_kernels() {
    let with = named_graph("b", 5);
    let answer = assert_one("urn:rdf:union", &[("content", ANONYMOUS), ("with", &with)]);
    // The anonymous nodes stay distinct: `b`, `c`, two list cells and the labeled one.
    let blanks: BTreeSet<&str> = answer
        .split_whitespace()
        .filter(|w| w.starts_with("_:"))
        .collect();
    assert_eq!(blanks.len(), 5, "{answer}");
}

#[test]
fn union_still_merges_a_shared_label_and_keeps_anonymous_nodes_apart() {
    // The behavior before the fix, kept: a document label names one node across the two
    // documents, and two anonymous nodes stay two.
    let content = "_:x <urn:p:name> \"one\" .\n<urn:a:1> <urn:p:knows> [ <urn:p:name> \"n\" ] .\n";
    // Inline `with=` Turtle starts with `@` or `<`: anything else is read as an IRI.
    let with = "@prefix p: <urn:p:> .\n_:x p:age \"2\" .\n<urn:a:1> p:knows [ p:name \"n\" ] .\n";
    let answer = assert_one("urn:rdf:union", &[("content", content), ("with", with)]);
    let blanks: BTreeSet<&str> = answer
        .split_whitespace()
        .filter(|w| w.starts_with("_:"))
        .collect();
    assert_eq!(blanks.len(), 3, "{answer}");
}

#[test]
fn diff_with_anonymous_nodes_is_identical_across_fresh_kernels() {
    let with = named_graph("b", 5);
    for mode in ["added", "removed"] {
        assert_one(
            "urn:rdf:diff",
            &[("content", ANONYMOUS), ("with", &with), ("mode", mode)],
        );
    }
}

/// Every face `as=` selects, from each input syntax that mints anonymous nodes.
#[test]
fn transrept_is_identical_across_fresh_kernels_for_every_face() {
    let faces = [
        "text/turtle",
        "application/n-triples",
        "application/n-quads",
        "application/trig",
        "application/rdf+xml",
        "application/ld+json",
        "text/html",
    ];
    let named = named_graph("a", 20);
    for input in [ANONYMOUS, ANONYMOUS_JSON_LD, ANONYMOUS_RDF_XML, &named] {
        for face in faces {
            assert_one("urn:rdf:transrept", &[("content", input), ("as", face)]);
        }
    }
}

#[test]
fn transrept_keeps_document_order() {
    // Transreption streams: it is deterministic because the parsers are, so it keeps the
    // document's order rather than sorting (only union and diff, which hold a set, sort).
    // Two-colon IRIs: a leading `<urn:x>` is sniffed as an XML tag (see `angle_token_is_iri`).
    let input = "<urn:z:1> <urn:p:v> \"1\" .\n<urn:a:1> <urn:p:v> \"2\" .\n";
    let answer = assert_one(
        "urn:rdf:transrept",
        &[("content", input), ("as", "application/n-triples")],
    );
    assert_eq!(answer, input);
}
