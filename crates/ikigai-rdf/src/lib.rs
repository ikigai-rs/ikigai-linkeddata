//! `ikigai-rdf` — RDF **transreption** as an ikigai resource.
//!
//! `urn:rdf:transrept` takes an RDF document and re-serializes it into another syntax,
//! chosen by the `as` argument (content negotiation): Turtle, N-Triples, N-Quads, TriG,
//! RDF/XML, JSON-LD, or a human-readable HTML table. The input syntax is sniffed (an
//! explicit format isn't needed for the common cases).
//!
//! It's the first step toward NetKernel-style *transreption* — lossless transformation
//! between representations of the same resource. Built on Oxigraph's **pure** parser/
//! serializer crates (`oxrdfio`/`oxrdf`) — no store, no rocksdb — so it compiles to
//! `wasm32` and the browser can transrept a fetched graph **client-side**:
//!
//! ```text
//! source urn:http:get url=https://example.org/thing | urn:rdf:transrept as=text/turtle
//! ```
//!
//! The kernel pipes the fetched bytes into `content`; `as` picks the representation.
//!
//! The default `transrept` feature provides `space()` and all formats. With
//! `default-features = false`, Turtle/JSON depth scans, canonicalization and
//! [`subclass_axioms`] remain available without oxrdfio. The `rdfxml` feature adds
//! `check_rdfxml_nesting` and quick-xml independently of transreption.

#![forbid(unsafe_code)]

mod canonical;
mod depth;
pub use canonical::{canonical_triples, BlankNodeLabels};
pub use depth::{
    check_json_nesting, check_turtle_nesting, JSON_LD_STACK, MAX_JSON_NESTING, MAX_TURTLE_NESTING,
};

#[cfg(feature = "rdfxml")]
pub use depth::check_rdfxml_nesting;

#[cfg(feature = "transrept")]
mod transrept;
#[cfg(feature = "transrept")]
pub use transrept::{space, SPACE_ID};

use oxrdf::{NamedOrBlankNode, Term};

/// The `rdfs:subClassOf` IRI.
const SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// Parse a Turtle alignment graph and return every `rdfs:subClassOf` axiom as a
/// `(subclass, superclass)` IRI pair — the closure pairs a host feeds to the kernel's
/// type-aware action selection (`Kernel::with_subclass_axioms`), so an entity typed
/// `foaf:Person` satisfies a `schema:Person` action. Best-effort: triples whose subject or
/// object isn't an IRI (blank nodes, literals) and any unparseable triples are skipped —
/// `subClassOf` relates named classes.
pub fn subclass_axioms(turtle: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for triple in oxttl::TurtleParser::new().for_slice(turtle.as_bytes()) {
        let Ok(triple) = triple else { continue };
        if triple.predicate.as_str() != SUBCLASS_OF {
            continue;
        }
        if let (NamedOrBlankNode::NamedNode(sub), Term::NamedNode(sup)) =
            (&triple.subject, &triple.object)
        {
            pairs.push((sub.as_str().to_string(), sup.as_str().to_string()));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subclass_axioms_extracts_rdfs_subclassof_pairs() {
        let ttl = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix schema: <https://schema.org/> .
foaf:Person rdfs:subClassOf schema:Person .
schema:Person rdfs:subClassOf schema:Thing .
foaf:Person foaf:name "ignored" ."#;
        let mut pairs = subclass_axioms(ttl);
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "http://xmlns.com/foaf/0.1/Person".to_string(),
                    "https://schema.org/Person".to_string()
                ),
                (
                    "https://schema.org/Person".to_string(),
                    "https://schema.org/Thing".to_string()
                ),
            ],
            "only the two rdfs:subClassOf triples, as IRI pairs"
        );
    }
}
