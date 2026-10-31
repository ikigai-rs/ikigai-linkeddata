//! `src/vocabulary.ttl` is the document served at https://ikigai-rs.dev/ns/html: every `htm:`
//! term the lift writes is defined there, and its `owl:versionInfo` is the crate version, which
//! is what the hub's `ns-drift` compares with the deployed copy.

use ikigai_html::{vocab::HTM, Lift, VOCABULARY};
use oxrdf::{NamedNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::HashSet;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn vocabulary() -> Vec<Triple> {
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(VOCABULARY.as_bytes())
        .map(|q| {
            let q = q.unwrap();
            Triple::new(q.subject, q.predicate, q.object)
        })
        .collect()
}

#[test]
fn ontology_version_is_the_crate_version() {
    let versions: Vec<_> = vocabulary()
        .into_iter()
        .filter(|t| {
            t.subject.to_string() == "<https://ikigai-rs.dev/ns/html>"
                && t.predicate.as_str() == "http://www.w3.org/2002/07/owl#versionInfo"
        })
        .map(|t| t.object)
        .collect();
    assert_eq!(versions.len(), 1);
    let Term::Literal(version) = &versions[0] else {
        panic!("{versions:?}")
    };
    assert_eq!(version.value(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn every_term_the_lift_writes_is_defined() {
    let defined: HashSet<String> = vocabulary()
        .iter()
        .filter(|t| t.predicate.as_str() == RDF_TYPE)
        .map(|t| t.subject.to_string())
        .collect();
    let page = std::fs::read_to_string(format!(
        "{}/tests/fixtures/item.html",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut lift = Lift::new();
    lift.document(&page).unwrap();
    let item = NamedNode::new("https://example.org/i").unwrap();
    lift.fragment(&item, &item, &page).unwrap();
    let mut used = HashSet::new();
    for t in lift.into_triples() {
        used.insert(t.predicate.to_string());
        if t.predicate.as_str() == RDF_TYPE {
            used.insert(t.object.to_string());
        }
    }
    let used: HashSet<_> = used
        .into_iter()
        .filter(|t| t.starts_with(&format!("<{HTM}")))
        .collect();
    // All ten terms, so a term the lift stops writing is noticed too.
    assert_eq!(used.len(), 10, "{used:?}");
    let undefined: Vec<_> = used.difference(&defined).collect();
    assert!(undefined.is_empty(), "undefined: {undefined:?}");
}
