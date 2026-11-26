use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
use oxrdf::{BlankNode, Dataset, GraphName, NamedOrBlankNode, Term};
use std::collections::{HashMap, HashSet};

/// Canonicalize an RDF 1.1 dataset using RDFC-1.0 and SHA-256.
///
/// Returns canonical N-Quads (UTF-8, with a final newline for each quad) and a map from
/// original blank nodes to their issued `c14n` identifiers. The dataset is not modified.
///
/// To bound poisoning before entering oxrdf's non-interruptible algorithm, this function
/// refuses datasets with more than 10,000 quads, more than 64 blank nodes, or more than 16
/// blank nodes in a connected component, or a component with more than seven nodes that
/// is not a tree with at most one branching node. Components with a node adjacent to more
/// than three others are also refused. Nodes sharing a quad are connected. These limits
/// bound the number and size of hash-n-degree searches conservatively, so some valid datasets
/// are refused. RDF 1.2 triple terms are also refused because RDFC-1.0 does not define them.
///
/// ```
/// use ikigai_rdf::canonicalize_dataset;
/// use oxrdf::{BlankNode, Dataset, GraphName, NamedNode, Quad};
/// let node = BlankNode::new("input").unwrap();
/// let mut dataset = Dataset::new();
/// dataset.insert(&Quad::new(node.clone(), NamedNode::new("urn:p").unwrap(),
///     NamedNode::new("urn:o").unwrap(), GraphName::DefaultGraph));
/// let (nquads, issued) = canonicalize_dataset(&dataset).unwrap();
/// assert_eq!(nquads, "_:c14n0 <urn:p> <urn:o> .\n");
/// assert_eq!(issued[&node].as_str(), "c14n0");
/// ```
pub fn canonicalize_dataset(
    dataset: &Dataset,
) -> Result<(String, HashMap<BlankNode, BlankNode>), String> {
    canonicalize(dataset, CanonicalizationHashAlgorithm::Sha256)
}

fn canonicalize(
    dataset: &Dataset,
    hash_algorithm: CanonicalizationHashAlgorithm,
) -> Result<(String, HashMap<BlankNode, BlankNode>), String> {
    if dataset.len() > 10_000 {
        return Err(
            "The dataset exceeds the 10,000-quad canonicalization limit; reduce the dataset size."
                .into(),
        );
    }
    let mut neighbors: HashMap<BlankNode, HashSet<BlankNode>> = HashMap::new();
    for quad in dataset.iter() {
        let mut nodes = Vec::new();
        if let NamedOrBlankNode::BlankNode(b) = quad.subject.into_owned() {
            nodes.push(b);
        }
        match quad.object.into_owned() {
            Term::BlankNode(b) => nodes.push(b),
            Term::NamedNode(_) | Term::Literal(_) => {}
            #[allow(unreachable_patterns)]
            _ => return Err(
                "The dataset contains an RDF 1.2 triple term; use an RDF 1.1 dataset for RDFC-1.0."
                    .into(),
            ),
        }
        if let GraphName::BlankNode(b) = quad.graph_name.into_owned() {
            nodes.push(b);
        }
        for b in &nodes {
            neighbors
                .entry(b.clone())
                .or_default()
                .extend(nodes.iter().filter(|other| *other != b).cloned());
        }
        if neighbors.len() > 64 {
            return Err("The dataset exceeds the 64-blank-node canonicalization limit; reduce the dataset size.".into());
        }
    }
    let mut seen = HashSet::new();
    for node in neighbors.keys() {
        if seen.contains(node) {
            continue;
        }
        let mut pending = vec![node.clone()];
        let mut count = 0;
        let mut edges = 0;
        let mut branches = 0;
        while let Some(node) = pending.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            count += 1;
            let degree = neighbors[&node].len();
            edges += degree;
            branches += usize::from(degree > 2);
            if count > 16 || degree > 3 {
                return Err("The dataset exceeds the canonicalization limit of 16 blank nodes per component or three neighbors per node; reduce the component size or connectivity.".into());
            }
            pending.extend(
                neighbors[&node]
                    .iter()
                    .filter(|b| !seen.contains(*b))
                    .cloned(),
            );
        }
        if count > 7 && (edges / 2 != count - 1 || branches > 1) {
            return Err("The dataset exceeds the canonicalization limit for connected components larger than seven blank nodes; reduce the component size or connectivity.".into());
        }
    }
    let issued: HashMap<BlankNode, BlankNode> = dataset
        .canonicalize_blank_nodes(CanonicalizationAlgorithm::Rdfc10 { hash_algorithm })
        .into_iter()
        .map(|(a, b)| (a.into_owned(), b))
        .collect();
    let mut lines = Vec::with_capacity(dataset.len());
    for q in dataset.iter() {
        let mut q = q.into_owned();
        if let NamedOrBlankNode::BlankNode(b) = &mut q.subject {
            *b = issued[b].clone();
        }
        if let Term::BlankNode(b) = &mut q.object {
            *b = issued[b].clone();
        }
        if let GraphName::BlankNode(b) = &mut q.graph_name {
            *b = issued[b].clone();
        }
        lines.push(format!("{q} .\n"));
    }
    lines.sort_unstable();
    Ok((lines.concat(), issued))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;

    #[test]
    fn w3c_rdfc10_vectors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c-rdfc10");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.jsonld")).unwrap()).unwrap();
        let entries = manifest["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 86);
        for entry in entries {
            let id = entry["id"].as_str().unwrap();
            let input = std::fs::read(root.join(entry["action"].as_str().unwrap())).unwrap();
            let mut dataset = Dataset::new();
            for quad in oxttl::NQuadsParser::new().for_slice(&input) {
                dataset.insert(&quad.unwrap());
            }
            let hash = if entry["hashAlgorithm"] == "SHA384" {
                CanonicalizationHashAlgorithm::Sha384
            } else {
                CanonicalizationHashAlgorithm::Sha256
            };
            let result = canonicalize(&dataset, hash);
            if entry["type"] == "rdfc:RDFC10NegativeEvalTest" {
                assert!(result.is_err(), "{id} should be refused");
                continue;
            }
            let (output, issued) = result.unwrap_or_else(|error| panic!("{id}: {error}"));
            let expected = std::fs::read(root.join(entry["result"].as_str().unwrap())).unwrap();
            if entry["type"] == "rdfc:RDFC10MapTest" {
                let expected: HashMap<String, String> = serde_json::from_slice(&expected).unwrap();
                let actual: HashMap<String, String> = issued
                    .into_iter()
                    .map(|(k, v)| (k.as_str().into(), v.as_str().into()))
                    .collect();
                assert_eq!(actual, expected, "{id}");
            } else {
                assert_eq!(output.as_bytes(), expected, "{id}");
            }
        }
    }
}
