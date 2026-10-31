//! HTML lifted into structural RDF, so a SPARQL query can see inside it (ledger #2674).
//!
//! `urn:iki:html:graph content=<html>` answers one HTML document's tree as Turtle (other RDF
//! faces through `as=`, by [`ikigai_rdf`]'s transreption). `urn:iki:html:graph graph=<iri>
//! predicate=<iri>` lifts every HTML literal under that predicate in the graph (a feed's
//! `content:encoded`, say), each as a fragment rooted at its subject by `htm:tree`: one request
//! per feed.
//!
//! The tree is html5ever's ([`html5ever`]): the WHATWG tree builder, so the triples are the tree
//! a browser would build. This crate is the arena it builds into (`src/tree.rs`) and the walk
//! from that tree to triples. Every node is named by a digest of where it sits; there are no
//! blank nodes. The vocabulary ([`VOCABULARY`]) reuses `rdf:value`, `dcterms:hasPart` and
//! `schema:position`, and mints only what has no published term; the README says which.
//!
//! Both bounds refuse as [`Error::OverBudget`], never truncate: elements nested deeper than
//! [`MAX_HTML_DEPTH`], measured with an explicit stack before the tree is walked, and a lift that
//! would mint more than [`MAX_HTML_NODES`] nodes.
#![forbid(unsafe_code)]

mod tree;

use async_trait::async_trait;
use html5ever::ns;
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FailureClass,
    Invocation, Iri, ReprType, Representation, Request, Result, Verb,
};
use ikigai_rdf::canonical_triples;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use sha2::{Digest, Sha256};
use tree::{Kind, Tree};

/// The largest HTML input admitted, in bytes: one document, or a graph's literals together.
pub const MAX_HTML_BYTES: usize = 8 << 20;
/// The largest RDF graph read for the `graph` form, in bytes.
pub const MAX_GRAPH_BYTES: usize = 8 << 20;
/// The deepest element nesting lifted: a document's `html` element is at depth 1, a fragment's
/// top-level elements are. Deeper is refused.
pub const MAX_HTML_DEPTH: usize = 256;
/// The most nodes one request's lift mints: roots, elements, attributes and texts together.
pub const MAX_HTML_NODES: usize = 100_000;
/// The identity claimed by the configuration-free [`space`].
pub const SPACE_ID: &str = "urn:iki:space:html";

/// The namespaces the lift writes in.
pub mod vocab {
    /// This crate's own terms, published at <https://ikigai-rs.dev/ns/html>.
    pub const HTM: &str = "https://ikigai-rs.dev/ns/html#";
    pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    pub const DCTERMS: &str = "http://purl.org/dc/terms/";
    pub const SCHEMA: &str = "https://schema.org/";
    /// The HTML namespace: an element in it carries no `htm:namespace`.
    pub const XHTML: &str = "http://www.w3.org/1999/xhtml";
}

/// The `htm:` terms, the document served at <https://ikigai-rs.dev/ns/html>.
pub const VOCABULARY: &str = include_str!("vocabulary.ttl");

/// The lift.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:iki:html:graph"), HtmlGraph)
        .named(space_iri("html"))
}

const FACES: [&str; 7] = [
    "text/turtle",
    "application/n-triples",
    "application/n-quads",
    "application/trig",
    "application/rdf+xml",
    "application/ld+json",
    "text/html",
];
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}

fn over(detail: impl Into<String>) -> Error {
    Error::OverBudget {
        detail: detail.into(),
    }
}

struct HtmlGraph;

#[async_trait]
impl Endpoint for HtmlGraph {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let face = optional(inv, "as")?.unwrap_or("text/turtle");
        if !FACES.contains(&face) {
            return Err(invalid("as", format!("unsupported RDF face: {face}")));
        }
        let args = &inv.request.args;
        let triples = match (args.contains_key("content"), args.contains_key("graph")) {
            (true, false) => {
                if args.contains_key("predicate") {
                    return Err(invalid("predicate", "goes with `graph`, not `content`"));
                }
                // Read under a pool one byte past the bound, so a stream past it is refused
                // as it arrives.
                let body = inv.content_body("content").await?;
                let bytes = inv
                    .with_memory_limit(MAX_HTML_BYTES as u64 + 1)
                    .collect(body)
                    .await?;
                if bytes.len() > MAX_HTML_BYTES {
                    return Err(over(format!("HTML exceeds {MAX_HTML_BYTES} bytes")));
                }
                let mut lift = Lift::new();
                lift.document(&String::from_utf8_lossy(&bytes))?;
                lift.into_triples()
            }
            (false, true) => {
                let graph = inv.inline_str("graph")?.trim();
                Iri::parse(graph).map_err(|e| invalid("graph", e.to_string()))?;
                let predicate = inv.inline_str("predicate")?.trim();
                let predicate = NamedNode::new(predicate)
                    .map_err(|e| invalid("predicate", format!("not an IRI: {e}")))?;
                let source = resolve_graph(inv, graph).await?;
                if source.bytes.len() > MAX_GRAPH_BYTES {
                    return Err(over(format!("graph exceeds {MAX_GRAPH_BYTES} bytes")));
                }
                let media = source
                    .repr_type
                    .media_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim();
                let format = RdfFormat::from_media_type(media)
                    .ok_or_else(|| invalid("graph", format!("<{graph}> is not RDF ({media})")))?;
                let literals = literals(graph, format, &source.bytes, &predicate)?;
                let total: usize = literals.iter().map(|(_, html)| html.len()).sum();
                if total > MAX_HTML_BYTES {
                    return Err(over(format!(
                        "the graph's HTML literals exceed {MAX_HTML_BYTES} bytes together"
                    )));
                }
                let mut lift = Lift::new();
                for (subject, html) in &literals {
                    lift.fragment(subject, &predicate, html)?;
                }
                lift.into_triples()
            }
            (true, true) => return Err(invalid("graph", "give `content` or `graph`, not both")),
            (false, false) => return Err(Error::MissingArgument("content".into())),
        };
        let turtle = serialize(triples)?;
        if face == "text/turtle" {
            return Ok(Representation::new(
                ReprType::new("text/turtle").with_param("charset", "utf-8"),
                turtle,
            )
            .cacheable());
        }
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg("content", ArgRef::Inline(turtle))
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        inv.issue(request).await
    }

    fn name(&self) -> &str {
        "html-graph"
    }

    fn describe(&self) -> Description {
        let desc = Description::new("html-graph")
            .title("HTML structure graph")
            .summary(
                "HTML parsed by html5ever's tree builder and lifted into skolemized structural \
                 triples: elements, attributes, child order and text. One document, or every \
                 HTML literal under a predicate in a graph, each rooted at its subject",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary("one HTML document's bytes; give this or `graph`")
                    .class(XSD_STRING)
                    .optional()
                    .reads_body(),
            )
            .input(
                ArgSpec::new("graph")
                    .summary(
                        "an RDF graph's IRI, resolved through the kernel; its HTML literals \
                         under `predicate` are lifted. Give this or `content`",
                    )
                    .class(XSD_ANY_URI)
                    .optional(),
            )
            .input(
                ArgSpec::new("predicate")
                    .summary(
                        "with `graph`: the predicate whose literals are HTML, such as \
                         content:encoded or rss:description",
                    )
                    .class(XSD_ANY_URI)
                    .optional(),
            )
            .input(
                ArgSpec::new("as")
                    .summary("the RDF face; Turtle by default")
                    .class(XSD_STRING)
                    .optional()
                    .one_of(FACES)
                    .default_value("text/turtle"),
            )
            .fails(FailureClass::OverBudget);
        FACES.iter().fold(desc, |desc, face| desc.output(*face))
    }
}

fn optional<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    match inv.inline_str(name) {
        Ok(value) => Ok(Some(value)),
        Err(Error::MissingArgument(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn resolve_graph(inv: &Invocation<'_>, uri: &str) -> Result<Representation> {
    let result = if uri.starts_with("http://") || uri.starts_with("https://") {
        inv.issue(
            Request::new(Verb::Source, Iri::parse("urn:http:get").unwrap())
                .with_arg("url", ArgRef::Inline(uri.as_bytes().to_vec())),
        )
        .await
    } else {
        inv.source(&Iri::parse(uri).map_err(|e| invalid("graph", e.to_string()))?)
            .await
    };
    match result {
        Err(Error::Unresolved(iri)) if iri.as_str() == uri => Err(Error::NotFound(uri.into())),
        other => other,
    }
}

/// Each `(subject, lexical form)` of a literal under `predicate`, in document order. A blank
/// subject is refused: its tree could hang on no IRI, and the lift mints none.
fn literals(
    uri: &str,
    format: RdfFormat,
    bytes: &[u8],
    predicate: &NamedNode,
) -> Result<Vec<(NamedNode, String)>> {
    // As in ikigai-reason and ikigai-sparql: the depth scans precede the recursive parsers.
    match format {
        RdfFormat::RdfXml => ikigai_rdf::check_rdfxml_nesting(bytes, "graph")?,
        RdfFormat::JsonLd { .. } => ikigai_rdf::check_json_nesting(bytes, "graph")?,
        _ => ikigai_rdf::check_turtle_nesting(&String::from_utf8_lossy(bytes), "graph")?,
    }
    let parse = || {
        let mut found = Vec::new();
        for quad in RdfParser::from_format(format)
            .with_base_iri(uri)
            .map_err(|e| invalid("graph", e.to_string()))?
            .for_slice(bytes)
        {
            let quad = quad.map_err(|e| invalid("graph", format!("<{uri}>: {e}")))?;
            if quad.predicate != *predicate {
                continue;
            }
            let Term::Literal(literal) = quad.object else {
                continue;
            };
            let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
                return Err(invalid(
                    "graph",
                    format!("a <{predicate}> literal hangs on a blank node; name its subject"),
                ));
            };
            found.push((subject, literal.value().to_string()));
        }
        Ok(found)
    };
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(ikigai_rdf::JSON_LD_STACK)
                .spawn_scoped(scope, parse)
                .map_err(|e| Error::Endpoint(e.to_string()))?
                .join()
                .map_err(|_| Error::Endpoint("RDF parser panicked".into()))?
        })
    }
    #[cfg(target_family = "wasm")]
    parse()
}

/// One request's lift: the triples so far, and the nodes minted against [`MAX_HTML_NODES`].
pub struct Lift {
    triples: Vec<Triple>,
    nodes: usize,
    max_depth: usize,
    max_nodes: usize,
}

impl Default for Lift {
    fn default() -> Self {
        Self::new()
    }
}

impl Lift {
    /// A lift under [`MAX_HTML_DEPTH`] and [`MAX_HTML_NODES`].
    pub fn new() -> Self {
        Self::with_bounds(MAX_HTML_DEPTH, MAX_HTML_NODES)
    }

    /// A lift under other bounds (tests use small ones).
    pub fn with_bounds(max_depth: usize, max_nodes: usize) -> Self {
        Lift {
            triples: Vec::new(),
            nodes: 0,
            max_depth,
            max_nodes,
        }
    }

    /// Lift one HTML document, rooted at an `htm:Document` named by digest of the text.
    pub fn document(&mut self, html: &str) -> Result<NamedNode> {
        let root = digest("document", &[html]);
        let tree = self.parse(html, false)?;
        self.mint()?;
        self.add(&root, rdf::TYPE, htm("Document"));
        self.walk(&tree, tree.top(false), &root)?;
        Ok(root)
    }

    /// Lift one HTML fragment held by `subject` under `predicate`: an `htm:Fragment`, named by
    /// digest of all three, that `subject htm:tree` points at and `htm:property` names
    /// `predicate` on.
    pub fn fragment(
        &mut self,
        subject: &NamedNode,
        predicate: &NamedNode,
        html: &str,
    ) -> Result<NamedNode> {
        let root = digest("fragment", &[subject.as_str(), predicate.as_str(), html]);
        let tree = self.parse(html, true)?;
        self.mint()?;
        self.add(&root, rdf::TYPE, htm("Fragment"));
        self.add(subject, htm("tree").as_ref(), root.clone());
        self.add(&root, htm("property").as_ref(), predicate.clone());
        self.walk(&tree, tree.top(true), &root)?;
        Ok(root)
    }

    /// The triples lifted so far, in no particular order.
    pub fn into_triples(self) -> Vec<Triple> {
        self.triples
    }

    fn parse(&self, html: &str, fragment: bool) -> Result<Tree> {
        // The in-parse guard is a cost bound at twice the answer's (see `src/tree.rs`).
        let left = self.max_nodes.saturating_sub(self.nodes);
        let tree = tree::parse(
            html,
            fragment,
            self.max_depth.saturating_mul(2),
            left.saturating_mul(2),
        )
        .map_err(|_| self.refused())?;
        let depth = tree.depth(tree.top(fragment));
        if depth > self.max_depth {
            return Err(over(format!(
                "HTML nests elements {depth} deep, deeper than {}",
                self.max_depth
            )));
        }
        Ok(tree)
    }

    fn refused(&self) -> Error {
        over(format!(
            "HTML deeper than {} or past {} nodes",
            self.max_depth, self.max_nodes
        ))
    }

    /// Count one node minted, refusing past the bound before anything is written for it.
    fn mint(&mut self) -> Result<()> {
        if self.nodes == self.max_nodes {
            return Err(over(format!(
                "the lift would mint more than {} nodes",
                self.max_nodes
            )));
        }
        self.nodes += 1;
        Ok(())
    }

    fn add(&mut self, s: &NamedNode, p: oxrdf::NamedNodeRef<'_>, o: impl Into<Term>) {
        self.triples.push(Triple::new(s.clone(), p, o));
    }

    /// The tree under `top`, below `root`, with an explicit stack. Each node is named by digest
    /// of the root and its path of positions (`1.3.2`), so the same HTML in the same place is
    /// the same IRIs on every run.
    fn walk(&mut self, tree: &Tree, top: &[usize], root: &NamedNode) -> Result<()> {
        let mut stack: Vec<(&[usize], NamedNode, String)> =
            vec![(top, root.clone(), String::new())];
        while let Some((children, parent, path)) = stack.pop() {
            let mut position = 0;
            for &i in children {
                let node = &tree.nodes[i];
                if matches!(node.kind, Kind::Other | Kind::Document) {
                    continue;
                }
                position += 1;
                let path = if path.is_empty() {
                    position.to_string()
                } else {
                    format!("{path}.{position}")
                };
                let named = digest("node", &[root.as_str(), &path]);
                self.mint()?;
                self.add(&parent, dcterms("hasPart").as_ref(), named.clone());
                self.add(
                    &named,
                    schema("position").as_ref(),
                    Literal::new_typed_literal(position.to_string(), xsd::INTEGER),
                );
                match &node.kind {
                    Kind::Text(text) => {
                        self.add(&named, rdf::TYPE, htm("Text"));
                        self.add(&named, rdf::VALUE, Literal::new_simple_literal(&**text));
                    }
                    Kind::Element { name, attrs, .. } => {
                        self.add(&named, rdf::TYPE, htm("Element"));
                        self.add(
                            &named,
                            htm("localName").as_ref(),
                            Literal::new_simple_literal(&*name.local),
                        );
                        if name.ns != ns!(html) {
                            self.namespace(&named, &name.ns);
                        }
                        for attr in attrs {
                            let attribute = digest(
                                "attribute",
                                &[root.as_str(), &path, &attr.name.ns, &attr.name.local],
                            );
                            self.mint()?;
                            self.add(&named, htm("attribute").as_ref(), attribute.clone());
                            self.add(&attribute, rdf::TYPE, htm("Attribute"));
                            self.add(
                                &attribute,
                                htm("localName").as_ref(),
                                Literal::new_simple_literal(&*attr.name.local),
                            );
                            self.add(
                                &attribute,
                                rdf::VALUE,
                                Literal::new_simple_literal(&*attr.value),
                            );
                            if !attr.name.ns.is_empty() {
                                self.namespace(&attribute, &attr.name.ns);
                            }
                        }
                        if !node.children.is_empty() {
                            stack.push((&node.children, named, path));
                        }
                    }
                    Kind::Other | Kind::Document => unreachable!(),
                }
            }
        }
        Ok(())
    }

    fn namespace(&mut self, s: &NamedNode, ns: &str) {
        if let Ok(ns) = NamedNode::new(ns) {
            self.add(s, htm("namespace").as_ref(), ns);
        }
    }
}

fn htm(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{}{local}", vocab::HTM))
}

fn dcterms(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{}{local}", vocab::DCTERMS))
}

fn schema(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{}{local}", vocab::SCHEMA))
}

/// A digest IRI (RFC 6920's `nih:` form): SHA-256 over `kind` and `parts`, each followed by
/// U+0000, written in lowercase hex. HTML text can hold U+0000, so the parts are length-prefixed
/// as well, and no two part lists hash alike.
fn digest(kind: &str, parts: &[&str]) -> NamedNode {
    let mut hash = Sha256::new();
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    NamedNode::new_unchecked(format!("nih:sha-256;{hex}"))
}

/// The lift as Turtle, triples in one order, under the lift's prefixes.
fn serialize(triples: Vec<Triple>) -> Result<Vec<u8>> {
    let failed = |e: std::io::Error| Error::Endpoint(e.to_string());
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle);
    for (prefix, iri) in [
        ("htm", vocab::HTM),
        ("rdf", vocab::RDF),
        ("dcterms", vocab::DCTERMS),
        ("schema", vocab::SCHEMA),
    ] {
        serializer = serializer
            .with_prefix(prefix, iri)
            .map_err(|e| Error::Endpoint(e.to_string()))?;
    }
    let mut bytes = Vec::new();
    let mut writer = serializer.for_writer(&mut bytes);
    for triple in canonical_triples(triples, |_| true) {
        writer.serialize_triple(&triple).map_err(failed)?;
    }
    writer.finish().map_err(failed)?;
    Ok(bytes)
}
