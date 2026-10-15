//! A blank node inside an RDF 1.2 triple term gets the same label on every fresh kernel
//! (ledger #1575).
//!
//! The parsers label an anonymous node at random, and `BlankNodeLabels` relabels them in
//! document order so a cacheable answer is one set of bytes (ledger #1566). It could not reach a
//! node inside a triple term (`<<( [] ex:p ex:o )>>`): oxrdf's triple-term variant exists only
//! under `oxrdf/rdf-12`, and this crate did not name it, so that node kept the parser's label
//! and every face over it varied. This crate's `rdf-12` feature names it now. Run with
//! `--features rdf-12` (CI's `features: rdf-12` job does): without it the tests here are not
//! compiled, because without it the relabeling is not either.
//!
//! Measured against the code before the fix: every test here failed with 8 distinct answers in
//! 8 runs.

#![cfg(feature = "rdf-12")]

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::collections::BTreeSet;
use std::sync::Arc;

const RUNS: usize = 8;

/// Anonymous nodes inside triple terms: one as a triple term's subject, one as the object of a
/// triple term nested in another, and one shared with the outer triple. (Turtle 1.2 allows `[]`
/// in a triple term but not `[ … ]` with properties.)
const IN_TRIPLE_TERMS: &str = "@prefix ex: <urn:ex:> .\n\
ex:a ex:says <<( [] ex:knows ex:c )>> .\n\
ex:a ex:said <<( ex:c ex:claims <<( ex:d ex:knows [] )>> )>> .\n\
_:x ex:says <<( _:x ex:knows ex:e )>> .\n";

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
fn transrept_relabels_blank_nodes_inside_triple_terms() {
    let answer = assert_one(
        "urn:rdf:transrept",
        &[
            ("content", IN_TRIPLE_TERMS),
            ("as", "application/n-triples"),
        ],
    );
    // Document order, through the triple terms: the first `[]` is b0, the nested one is b1, and
    // `_:x` is b2 in both places it occurs, outside the triple term and inside it.
    assert_eq!(
        answer,
        "<urn:ex:a> <urn:ex:says> <<( _:b0 <urn:ex:knows> <urn:ex:c> )>> .\n\
         <urn:ex:a> <urn:ex:said> <<( <urn:ex:c> <urn:ex:claims> <<( <urn:ex:d> <urn:ex:knows> _:b1 )>> )>> .\n\
         _:b2 <urn:ex:says> <<( _:b2 <urn:ex:knows> <urn:ex:e> )>> .\n"
    );
}

#[test]
fn union_and_diff_relabel_blank_nodes_inside_triple_terms() {
    let with = "<urn:ex:z> <urn:ex:says> <<( [] <urn:ex:knows> <urn:ex:y> )>> .\n";
    assert_one(
        "urn:rdf:union",
        &[("content", IN_TRIPLE_TERMS), ("with", with)],
    );
    for mode in ["added", "removed"] {
        assert_one(
            "urn:rdf:diff",
            &[("content", IN_TRIPLE_TERMS), ("with", with), ("mode", mode)],
        );
    }
}
