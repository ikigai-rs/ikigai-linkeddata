//! Deterministic RDF output: the same graph serializes to the same bytes on every run
//! (ledger #1566).
//!
//! A `.cacheable()` answer promises that recomputing it gives the same bytes, and two things in
//! the oxigraph stack break that promise for RDF without anyone asking them to:
//!
//! - **Set order.** An [`oxrdf::Graph`] iterates in the order of its interner's keys, which are
//!   hashes under a per-graph `RandomState`, and oxigraph's in-memory store is hashed the same
//!   way. So a graph serialized in iteration order comes out in a different order on every
//!   fresh graph: `urn:rdf:union` gave 4 distinct outputs in 8 runs.
//! - **Blank-node labels.** Every parser mints a RANDOM label for an anonymous node (`[ … ]`, a
//!   collection, a JSON-LD node object without `@id`, an RDF/XML node element without
//!   `rdf:about`), and so does a SPARQL CONSTRUCT template for each solution.
//!
//! The two tools here answer them, cheapest first:
//!
//! - [`BlankNodeLabels`] relabels blank nodes in the order a parser emits them, which is
//!   document order and so deterministic: `b0`, `b1`, … . It streams, and it is all
//!   transreption needs.
//! - [`canonical_triples`] puts a SET of triples in one order (sorted by their N-Triples text)
//!   and labels the blank nodes it was told are not already stable (a CONSTRUCT template's) by
//!   their structure, not by the order they arrived in.

use oxrdf::{BlankNode, GraphName, NamedOrBlankNode, Quad, Term, Triple};
use std::collections::{HashMap, HashSet};

/// Relabels blank nodes in the order they are first seen: `b0`, `b1`, … . Fed a parser's
/// output, that is document order, so the labels are the same on every run, where the
/// parser's own labels for anonymous nodes are random.
///
/// A label the document wrote (`_:x`) maps to one node within a document; whether it names the
/// SAME node across two documents is the caller's choice: call [`new_document`] between them
/// for RDF merge semantics (each document's blank nodes are its own), or do not, to keep a
/// shared label shared. The counter is never reset, so no two documents' nodes ever share a
/// label by accident.
///
/// ```
/// use ikigai_rdf::BlankNodeLabels;
/// use oxrdf::{BlankNode, NamedNode, Triple};
///
/// let p = NamedNode::new("urn:p").unwrap();
/// let random = BlankNode::default(); // what a parser mints for `[ ]`
/// let mut labels = BlankNodeLabels::new();
/// let t = labels.triple(Triple::new(random.clone(), p.clone(), random.clone()));
/// assert_eq!(t.to_string(), "_:b0 <urn:p> _:b0");
/// assert!(labels.is_issued(&BlankNode::new("b0").unwrap()));
///
/// labels.new_document();
/// let t = labels.triple(Triple::new(random, p, BlankNode::default()));
/// assert_eq!(t.to_string(), "_:b1 <urn:p> _:b2");
/// ```
///
/// ⚠ A blank node nested inside an RDF 1.2 triple term (`<<( [] :p :o )>>`) is not relabeled:
/// this crate does not name the triple-term variant, which only exists when some crate in the
/// build turns `oxrdf/rdf-12` on.
///
/// [`new_document`]: BlankNodeLabels::new_document
#[derive(Debug, Default)]
pub struct BlankNodeLabels {
    map: HashMap<BlankNode, BlankNode>,
    issued: HashSet<String>,
    next: usize,
}

impl BlankNodeLabels {
    /// A relabeler that has issued nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new document: its labels name new nodes, never one an earlier document named.
    pub fn new_document(&mut self) {
        self.map.clear();
    }

    /// Whether `node` carries a label this relabeler issued.
    pub fn is_issued(&self, node: &BlankNode) -> bool {
        self.issued.contains(node.as_str())
    }

    /// The label for `node` in the current document.
    pub fn node(&mut self, node: &BlankNode) -> BlankNode {
        if let Some(label) = self.map.get(node) {
            return label.clone();
        }
        let label = BlankNode::new_unchecked(format!("b{}", self.next));
        self.next += 1;
        self.issued.insert(label.as_str().to_string());
        self.map.insert(node.clone(), label.clone());
        label
    }

    /// `triple` with its subject and object relabeled.
    pub fn triple(&mut self, triple: Triple) -> Triple {
        Triple::new(
            self.subject(triple.subject),
            triple.predicate,
            self.object(triple.object),
        )
    }

    /// `quad` with its subject, object and graph name relabeled.
    pub fn quad(&mut self, quad: Quad) -> Quad {
        let graph_name = match quad.graph_name {
            GraphName::BlankNode(node) => self.node(&node).into(),
            other => other,
        };
        Quad::new(
            self.subject(quad.subject),
            quad.predicate,
            self.object(quad.object),
            graph_name,
        )
    }

    fn subject(&mut self, subject: NamedOrBlankNode) -> NamedOrBlankNode {
        match subject {
            NamedOrBlankNode::BlankNode(node) => self.node(&node).into(),
            other => other,
        }
    }

    fn object(&mut self, object: Term) -> Term {
        match object {
            Term::BlankNode(node) => self.node(&node).into(),
            // A literal, an IRI, or (under `oxrdf/rdf-12`) a triple term, kept as it is.
            other => other,
        }
    }
}

/// A set of triples in one order: deduplicated, sorted by N-Triples text, with every blank node
/// that `stable` refuses relabeled by STRUCTURE, so the result does not depend on the order the
/// triples arrived in, or on the random labels those nodes came with.
///
/// `stable` says which blank nodes already carry a deterministic label (a parse relabeled by
/// [`BlankNodeLabels`], say) and keep it. The rest are labeled `c0`, `c1`, … : the nodes are
/// split into connected components (two nodes sharing a triple are connected), each
/// component's nodes are ordered by color refinement over the triples they occur in, and the
/// components by their resulting canonical text. A tie refinement leaves is broken by
/// individualizing one node and refining again, which gives one answer whenever the tied nodes
/// are interchangeable (an automorphism), the case a CONSTRUCT template's instances are in.
///
/// ```
/// use ikigai_rdf::canonical_triples;
/// use oxrdf::{BlankNode, NamedNode, Triple};
///
/// let p = NamedNode::new("urn:p").unwrap();
/// let s = |i: u32| NamedNode::new(format!("urn:s:{i}")).unwrap();
/// // Two runs that minted different random labels and arrived in different orders.
/// let run = |order: [u32; 2]| {
///     let triples = order.map(|i| Triple::new(s(i), p.clone(), BlankNode::default()));
///     canonical_triples(triples, |_| false)
///         .iter()
///         .map(|t| format!("{t} .\n"))
///         .collect::<String>()
/// };
/// assert_eq!(run([1, 2]), run([2, 1]));
/// assert_eq!(run([1, 2]), "<urn:s:1> <urn:p> _:c0 .\n<urn:s:2> <urn:p> _:c1 .\n");
/// ```
///
/// ⚠ Not a full canonicalization: refinement can leave nodes tied that are NOT
/// interchangeable (highly regular blank-node structures), and then the label they get can
/// still depend on arrival order. That residue is the price of staying polynomial: a complete
/// canonicalization (RDFC-1.0, or oxrdf's `canonicalize`) is exponential in the worst case, on
/// input a caller controls.
pub fn canonical_triples(
    triples: impl IntoIterator<Item = Triple>,
    stable: impl Fn(&BlankNode) -> bool,
) -> Vec<Triple> {
    let mut triples: Vec<Triple> = triples.into_iter().collect();
    sort_dedup(&mut triples);

    // The blank nodes to label, and the triples each occurs in.
    let mut index: HashMap<BlankNode, usize> = HashMap::new();
    let mut nodes: Vec<BlankNode> = Vec::new();
    let mut occurs: Vec<Vec<usize>> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    for (i, triple) in triples.iter().enumerate() {
        for node in blank_nodes(triple) {
            if stable(node) {
                taken.insert(node.as_str().to_string());
                continue;
            }
            let n = *index.entry(node.clone()).or_insert_with(|| {
                nodes.push(node.clone());
                occurs.push(Vec::new());
                nodes.len() - 1
            });
            if occurs[n].last() != Some(&i) {
                occurs[n].push(i);
            }
        }
    }
    if nodes.is_empty() {
        return triples;
    }

    // Connected components: nodes that share a triple.
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    for triple in &triples {
        let ends: Vec<usize> = blank_nodes(triple)
            .filter_map(|node| index.get(node).copied())
            .collect();
        for pair in ends.windows(2) {
            let (a, b) = (find(&mut parent, pair[0]), find(&mut parent, pair[1]));
            parent[a] = b;
        }
    }
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for n in 0..nodes.len() {
        let root = find(&mut parent, n);
        components.entry(root).or_default().push(n);
    }

    // Each component in canonical order, then all of them by their canonical text.
    let mut ordered: Vec<(String, Vec<usize>)> = components
        .into_values()
        .map(|members| canonical_component(&triples, &index, &occurs, members))
        .collect();
    ordered.sort();

    let mut labels: HashMap<BlankNode, BlankNode> = HashMap::new();
    let mut next = 0usize;
    for (_, members) in &ordered {
        for &n in members {
            let label = loop {
                let candidate = format!("c{next}");
                next += 1;
                if !taken.contains(&candidate) {
                    break candidate;
                }
            };
            labels.insert(nodes[n].clone(), BlankNode::new_unchecked(label));
        }
    }

    let relabel = |node: &BlankNode| labels.get(node).cloned().unwrap_or_else(|| node.clone());
    let mut out: Vec<Triple> = triples
        .into_iter()
        .map(|t| {
            let subject = match t.subject {
                NamedOrBlankNode::BlankNode(node) => relabel(&node).into(),
                other => other,
            };
            let object = match t.object {
                Term::BlankNode(node) => relabel(&node).into(),
                other => other,
            };
            Triple::new(subject, t.predicate, object)
        })
        .collect();
    sort_dedup(&mut out);
    out
}

/// Sort by N-Triples text and drop duplicates: a set, in one order.
fn sort_dedup(triples: &mut Vec<Triple>) {
    triples.sort_by_cached_key(|t| t.to_string());
    triples.dedup();
}

fn find(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}

/// The blank nodes in subject and object position (a triple term's are not reached; see
/// [`BlankNodeLabels`]).
fn blank_nodes(triple: &Triple) -> impl Iterator<Item = &BlankNode> {
    let subject = match &triple.subject {
        NamedOrBlankNode::BlankNode(node) => Some(node),
        _ => None,
    };
    let object = match &triple.object {
        Term::BlankNode(node) => Some(node),
        _ => None,
    };
    subject.into_iter().chain(object)
}

/// One component's nodes in canonical order, with the component's canonical text (its triples
/// with each node written as its final color), which orders the components.
fn canonical_component(
    triples: &[Triple],
    index: &HashMap<BlankNode, usize>,
    occurs: &[Vec<usize>],
    members: Vec<usize>,
) -> (String, Vec<usize>) {
    let local: HashMap<usize, usize> = members.iter().enumerate().map(|(l, &n)| (n, l)).collect();
    let mut rows: Vec<usize> = members.iter().flat_map(|&n| occurs[n].clone()).collect();
    rows.sort_unstable();
    rows.dedup();

    // Write a triple with each of this component's nodes as `@` (the node being described)
    // or `#color` (any other); everything else as N-Triples.
    let render = |row: usize, colors: &[usize], me: Option<usize>| -> String {
        let triple = &triples[row];
        let term = |node: Option<&BlankNode>, text: String| match node
            .and_then(|b| index.get(b))
            .and_then(|n| local.get(n))
        {
            Some(&l) if Some(l) == me => "@".to_string(),
            Some(&l) => format!("#{}", colors[l]),
            None => text,
        };
        let subject = match &triple.subject {
            NamedOrBlankNode::BlankNode(node) => term(Some(node), node.to_string()),
            other => other.to_string(),
        };
        let object = match &triple.object {
            Term::BlankNode(node) => term(Some(node), node.to_string()),
            other => other.to_string(),
        };
        format!("{subject} {} {object}", triple.predicate)
    };

    let refine = |colors: &mut Vec<usize>| loop {
        let signatures: Vec<(usize, Vec<String>)> = (0..members.len())
            .map(|l| {
                let mut sig: Vec<String> = occurs[members[l]]
                    .iter()
                    .map(|&row| render(row, colors, Some(l)))
                    .collect();
                sig.sort();
                (colors[l], sig)
            })
            .collect();
        let mut distinct: Vec<&(usize, Vec<String>)> = signatures.iter().collect();
        distinct.sort();
        distinct.dedup();
        let before = {
            let mut c = colors.clone();
            c.sort_unstable();
            c.dedup();
            c.len()
        };
        for (l, sig) in signatures.iter().enumerate() {
            colors[l] = distinct.binary_search(&sig).expect("present");
        }
        if distinct.len() == before {
            break;
        }
    };

    let mut colors = vec![0usize; members.len()];
    refine(&mut colors);
    // Individualize until every node has a color of its own: the smallest tied color first, and
    // within it any node, which is the same answer when the tied nodes are interchangeable.
    loop {
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for &c in &colors {
            *counts.entry(c).or_default() += 1;
        }
        let Some(tied) = counts
            .iter()
            .filter(|(_, &count)| count > 1)
            .map(|(&c, _)| c)
            .min()
        else {
            break;
        };
        let chosen = colors.iter().position(|&c| c == tied).expect("present");
        colors[chosen] = members.len();
        refine(&mut colors);
    }

    let mut text: Vec<String> = rows.iter().map(|&row| render(row, &colors, None)).collect();
    text.sort();
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by_key(|&l| colors[l]);
    (
        text.join("\n"),
        order.into_iter().map(|l| members[l]).collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    fn iri(s: &str) -> NamedNode {
        NamedNode::new(s).unwrap()
    }

    fn text(triples: &[Triple]) -> String {
        triples.iter().map(|t| format!("{t} .\n")).collect()
    }

    /// What a CONSTRUCT `{ <urn:s> <urn:tag> _:x . _:x <urn:p> _:y . _:y <urn:v> ?v }` yields
    /// for `values`, each solution minting fresh random nodes, in the order given.
    fn instances(values: &[u32]) -> Vec<Triple> {
        values
            .iter()
            .flat_map(|v| {
                let (x, y) = (BlankNode::default(), BlankNode::default());
                [
                    Triple::new(iri("urn:s"), iri("urn:tag"), x.clone()),
                    Triple::new(x, iri("urn:p"), y.clone()),
                    Triple::new(y, iri("urn:v"), Literal::from(*v)),
                ]
            })
            .collect()
    }

    #[test]
    fn arrival_order_and_minted_labels_do_not_change_the_answer() {
        // `_:x` is told apart only through `_:y`: labeling each tied class on its own would
        // pair an `x` with the wrong `y` in some orders.
        let first = text(&canonical_triples(instances(&[1, 2, 3, 4]), |_| false));
        for order in [[4, 3, 2, 1], [2, 4, 1, 3], [3, 1, 4, 2]] {
            let mut triples = instances(&order);
            triples.reverse();
            assert_eq!(text(&canonical_triples(triples, |_| false)), first);
        }
        assert!(first.contains("_:c1 <urn:p> _:c0 .\n"), "{first}");
        assert_eq!(first.lines().count(), 12, "{first}");
    }

    #[test]
    fn interchangeable_components_get_one_answer() {
        // Two solutions that differ in nothing but their minted nodes.
        let make = || {
            let x = BlankNode::default();
            vec![
                Triple::new(iri("urn:s"), iri("urn:tag"), x.clone()),
                Triple::new(x, iri("urn:v"), Literal::from(1)),
            ]
        };
        let (a, b) = (make(), make());
        let ab = canonical_triples(a.iter().chain(&b).cloned(), |_| false);
        let ba = canonical_triples(b.iter().chain(&a).cloned(), |_| false);
        assert_eq!(text(&ab), text(&ba));
        assert_eq!(ab.len(), 4, "the two stay two: {}", text(&ab));
    }

    #[test]
    fn a_stable_node_keeps_its_label_and_no_new_label_takes_it() {
        let stable = BlankNode::new("c0").unwrap();
        let minted = BlankNode::default();
        let out = canonical_triples(
            [
                Triple::new(stable.clone(), iri("urn:p"), minted.clone()),
                Triple::new(minted, iri("urn:v"), Literal::from(1)),
            ],
            |b| b == &stable,
        );
        assert_eq!(
            text(&out),
            "_:c0 <urn:p> _:c1 .\n_:c1 <urn:v> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n"
        );
    }

    #[test]
    fn duplicates_merge_and_order_is_the_n_triples_text() {
        let t = |s: &str| Triple::new(iri(s), iri("urn:p"), iri("urn:o"));
        let out = canonical_triples([t("urn:b"), t("urn:a"), t("urn:b")], |_| true);
        assert_eq!(
            text(&out),
            "<urn:a> <urn:p> <urn:o> .\n<urn:b> <urn:p> <urn:o> .\n"
        );
    }
}
