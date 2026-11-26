use ikigai_rdf::canonicalize_dataset;
use oxrdf::{BlankNode, Dataset, GraphName, NamedNode, Quad};

#[test]
fn rdfc_returns_canonical_nquads_and_issued_identifiers() {
    let original = BlankNode::new("original").unwrap();
    let mut dataset = Dataset::new();
    dataset.insert(&Quad::new(
        original.clone(),
        NamedNode::new("urn:p").unwrap(),
        NamedNode::new("urn:o").unwrap(),
        GraphName::DefaultGraph,
    ));
    let (nquads, identifiers) = canonicalize_dataset(&dataset).unwrap();
    assert_eq!(nquads, "_:c14n0 <urn:p> <urn:o> .\n");
    assert_eq!(identifiers[&original].as_str(), "c14n0");
}
