//! A quote post mapped as data (ledger #2675): the hard case for per-source mappings, a post
//! that tags alone cannot classify. `tests/fixtures/mappings/quotes.example.rq` is ONE SPARQL
//! CONSTRUCT over the normalized feed and its lifted HTML (`urn:iki:feed:html:{feed}`), and
//! writes a post kind, the quotation as a part of the item with its source and its own
//! creator, and the source's tags scoped to the source.
//!
//! The feed is `tests/fixtures/quote-atom.xml`, synthetic and shaped like a blog's Atom quote
//! entry; no real site's text is in it.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Space, Verb,
};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const FEED: &str = "http://quotes.example/feed.atom";
const ATOM: &str = include_str!("fixtures/quote-atom.xml");
const MAPPING: &str = include_str!("fixtures/mappings/quotes.example.rq");

// The entries by their `atom:id`, which is the item's `dc:identifier` and not its name: the
// lift may name an item however it scopes it, so the tests find it by its id.
const QUOTE: &str = "http://quotes.example/2026/Oct/30/quoting-ida-quoted/";
const SELF_QUOTE: &str = "http://quotes.example/2026/Oct/29/quoting-quinn-quoter/";
const NOT_A_QUOTE: &str = "http://quotes.example/2026/Oct/28/quoting-season/";
const QUOTER: &str = "Quinn Quoter";
const QUOTED_TEXT: &str = "Every feed is a graph if you squint.";
const SCHEME: &str = "http://quotes.example/tags";
const KIND: &str = "http://quotes.example/kinds#quote";

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_VALUE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#value";
const RSS_ITEM: &str = "http://purl.org/rss/1.0/item";
const DC_CREATOR: &str = "http://purl.org/dc/elements/1.1/creator";
const DC_IDENTIFIER: &str = "http://purl.org/dc/elements/1.1/identifier";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const DCAM_MEMBER_OF: &str = "http://purl.org/dc/dcam/memberOf";
const SCHEMA: &str = "https://schema.org/";
const PROV_WAS_QUOTED_FROM: &str = "http://www.w3.org/ns/prov#wasQuotedFrom";
const HTM: &str = "https://ikigai-rs.dev/ns/html#";

struct Host {
    kernel: Kernel,
    root: PathBuf,
}

impl Drop for Host {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// A host as `tests/mapped.rs` builds one, with ikigai-html mounted beside the feed modules
/// and the synthetic mapping in its config home.
fn host(label: &str) -> Host {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "ikigai-feed-quote-{}-{label}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(root.join("feed/mappings")).unwrap();
    std::fs::write(root.join("feed/mappings/quotes.example.rq"), MAPPING).unwrap();
    let files: Arc<dyn Space> = Arc::new(ikigai_fs::cacheable_space(&root));
    let http = EndpointSpace::new().bind(
        Exact::new("urn:http:get"),
        FnEndpoint::new("fixture-get", |inv| match inv.inline_str("url")? {
            FEED => Ok(Representation::new(
                ReprType::new("application/atom+xml"),
                ATOM.as_bytes().to_vec(),
            )
            .cacheable()),
            other => Err(Error::NotFound(other.to_string())),
        }),
    );
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_feed::mapped_space()),
        Arc::new(ikigai_feed::mappings(Arc::clone(&files))),
        files,
        Arc::new(ikigai_html::space()),
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_rdf::space()),
        Arc::new(http),
    ])));
    Host { kernel, root }
}

fn request(iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    )
}

impl Host {
    fn issue(&self, request: Request) -> ikigai_core::Result<Representation> {
        block_on(self.kernel.issue(request, &Capability::root()))
    }

    fn graph(&self, request: Request) -> HashSet<Triple> {
        parse(&self.issue(request).unwrap().bytes)
    }

    fn mapped(&self) -> HashSet<Triple> {
        self.graph(request(&format!("urn:iki:feed:mapped:{FEED}"), &[]))
    }

    fn normalized(&self) -> HashSet<Triple> {
        self.graph(request(&format!("urn:iki:feed:graph:{FEED}"), &[]))
    }

    /// The mapping's CONSTRUCT run over `graphs`, as a mapping author would run it.
    fn construct(&self, graphs: &str) -> HashSet<Triple> {
        self.graph(
            request("urn:sparql:construct", &[("graph", graphs)]).with_arg(
                "query",
                ArgRef::Reference(Iri::parse("urn:iki:feed:mapping:quotes.example").unwrap()),
            ),
        )
    }

    fn diff(&self, mode: &str) -> HashSet<Triple> {
        self.graph(
            request(
                "urn:rdf:diff",
                &[
                    ("with", &format!("urn:iki:feed:graph:{FEED}")),
                    ("mode", mode),
                ],
            )
            .with_arg(
                "content",
                ArgRef::Reference(Iri::parse(format!("urn:iki:feed:mapped:{FEED}")).unwrap()),
            ),
        )
    }
}

fn parse(bytes: &[u8]) -> HashSet<Triple> {
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(bytes)
        .map(|q| q.unwrap().into())
        .collect()
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn lit(s: &str) -> Term {
    Literal::new_simple_literal(s).into()
}

/// ikigai-feed's digest IRI: SHA-256 over a kind and its parts, each followed by U+0000.
fn digest(kind: &str, parts: &[&str]) -> NamedNode {
    let mut hash = Sha256::new();
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    iri(&format!("nih:sha-256;{hex}"))
}

/// The objects of `subject predicate ?o` in `g`.
fn objects(g: &HashSet<Triple>, subject: &NamedNode, predicate: &str) -> HashSet<Term> {
    g.iter()
        .filter(|t| {
            t.subject == NamedOrBlankNode::from(subject.clone())
                && t.predicate.as_str() == predicate
        })
        .map(|t| t.object.clone())
        .collect()
}

fn set<const N: usize>(terms: [Term; N]) -> HashSet<Term> {
    terms.into_iter().collect()
}

fn named(term: &Term) -> NamedNode {
    match term {
        Term::NamedNode(n) => n.clone(),
        other => panic!("not an IRI: {other}"),
    }
}

/// The item in `g` whose `dc:identifier` is `id`: exactly one.
fn item(g: &HashSet<Triple>, id: &str) -> NamedNode {
    let items: Vec<NamedNode> = g
        .iter()
        .filter(|t| t.predicate.as_str() == DC_IDENTIFIER && t.object == lit(id))
        .map(|t| match &t.subject {
            NamedOrBlankNode::NamedNode(n) => n.clone(),
            other => panic!("blank item {other}"),
        })
        .filter(|n| objects(g, n, RDF_TYPE).contains(&iri(RSS_ITEM).into()))
        .collect();
    assert_eq!(items.len(), 1, "one item has the id {id}: {items:?}");
    items.into_iter().next().unwrap()
}

/// The quotation the mapping hangs on the item with the id `id`: its one `dcterms:hasPart`.
fn quotation(g: &HashSet<Triple>, id: &str) -> NamedNode {
    let parts = objects(g, &item(g, id), &format!("{DCTERMS}hasPart"));
    assert_eq!(parts.len(), 1, "{id} has one part: {parts:?}");
    named(parts.iter().next().unwrap())
}

#[test]
fn the_normalized_feed_alone_cannot_classify_a_quote_post() {
    // The reproduction: run over the form alone, the mapping finds no blockquote, because
    // the quotation is inside an HTML literal. Only the tags (the form's own nodes) map.
    let host = host("alone");
    let constructed = host.construct(&format!("urn:iki:feed:graph:{FEED}"));
    assert!(!constructed.is_empty());
    assert!(
        constructed
            .iter()
            .all(|t| t.predicate.as_str() != PROV_WAS_QUOTED_FROM
                && t.predicate.as_str() != format!("{DCTERMS}type")),
        "{constructed:?}"
    );
    // With the lifted HTML beside it, the same file classifies the post.
    let both = host.construct(&format!(
        "urn:iki:feed:graph:{FEED},urn:iki:feed:html:{FEED}"
    ));
    assert!(both
        .iter()
        .any(|t| t.predicate.as_str() == PROV_WAS_QUOTED_FROM));
}

#[test]
fn the_mapped_feed_carries_the_kind_the_quotation_and_its_person() {
    let host = host("carries");
    let g = host.mapped();

    // The post kind, on the two quote posts and not on a note titled like one.
    let kind = format!("{DCTERMS}type");
    for id in [QUOTE, SELF_QUOTE] {
        assert_eq!(objects(&g, &item(&g, id), &kind), set([iri(KIND).into()]));
    }
    assert!(objects(&g, &item(&g, NOT_A_QUOTE), &kind).is_empty());
    assert!(objects(&g, &item(&g, NOT_A_QUOTE), &format!("{DCTERMS}hasPart")).is_empty());

    // The quotation: a part of the item, quoted from the blockquote's cite. Not `dcterms:source`,
    // which the form already uses for the entry's `link rel="via"`, and which stays that alone.
    let cite = "http://elsewhere.example/talks/squint";
    let q = quotation(&g, QUOTE);
    assert_eq!(q, digest("quotation", &[item(&g, QUOTE).as_str(), cite]));
    assert_eq!(
        objects(&g, &q, RDF_TYPE),
        set([iri(&format!("{SCHEMA}Quotation")).into()])
    );
    assert_eq!(
        objects(&g, &q, PROV_WAS_QUOTED_FROM),
        set([iri(cite).into()])
    );
    assert_eq!(
        objects(&g, &q, &format!("{SCHEMA}text")),
        set([lit(QUOTED_TEXT)])
    );
    assert_eq!(
        objects(&g, &item(&g, QUOTE), &format!("{DCTERMS}source")),
        set([iri("http://via.example/where-i-saw-it").into()])
    );

    // The quoted person, a digest over (name, href), named by the cite paragraph's first link
    // and not its second (the talk).
    let (name, href) = ("Ida Quoted", "http://elsewhere.example/ida");
    let agent = digest("agent", &[name, href]);
    assert_eq!(
        objects(&g, &q, &format!("{SCHEMA}creator")),
        set([agent.clone().into()])
    );
    assert_eq!(
        objects(&g, &agent, &format!("{SCHEMA}name")),
        set([lit(name)])
    );
    assert_eq!(
        objects(&g, &agent, &format!("{SCHEMA}url")),
        set([iri(href).into()])
    );
}

#[test]
fn the_kind_types_no_item_into_anything_new() {
    // `dcterms:type` has no rdfs:domain, and its rdfs:range is rdfs:Class: it classes the
    // KIND, never the item, under RDFS. The only typing the mapping adds is the quotation's.
    let host = host("types");
    let (mapped, normalized) = (host.mapped(), host.normalized());
    let added_types: HashSet<_> = mapped
        .difference(&normalized)
        .filter(|t| t.predicate.as_str() == RDF_TYPE)
        .map(|t| (t.subject.to_string(), t.object.to_string()))
        .collect();
    let quotations: HashSet<_> = [QUOTE, SELF_QUOTE]
        .iter()
        .map(|id| {
            (
                quotation(&mapped, id).to_string(),
                format!("<{SCHEMA}Quotation>"),
            )
        })
        .collect();
    assert_eq!(added_types, quotations);
}

#[test]
fn the_quoter_stays_the_items_creator_and_never_the_quoted_texts() {
    let host = host("creator");
    let g = host.mapped();
    for id in [QUOTE, SELF_QUOTE, NOT_A_QUOTE] {
        assert_eq!(
            objects(&g, &item(&g, id), DC_CREATOR),
            set([lit(QUOTER)]),
            "{id}"
        );
    }
    // Every node holding the quoted text: none is the quoter's, by `dc:creator` or by a
    // `schema:creator` named so.
    let creators = |node: &NamedNode| -> HashSet<Term> {
        let mut names = objects(&g, node, DC_CREATOR);
        for agent in objects(&g, node, &format!("{SCHEMA}creator")) {
            names.extend(objects(&g, &named(&agent), &format!("{SCHEMA}name")));
        }
        names
    };
    let holders: Vec<NamedNode> = g
        .iter()
        .filter(|t| t.object == lit(QUOTED_TEXT))
        .map(|t| match &t.subject {
            NamedOrBlankNode::NamedNode(n) => n.clone(),
            other => panic!("blank holder {other}"),
        })
        .collect();
    assert_eq!(holders, [quotation(&g, QUOTE)]);
    for holder in &holders {
        let names = creators(holder);
        assert_eq!(names, set([lit("Ida Quoted")]));
        assert!(!names.contains(&lit(QUOTER)));
    }
    // The self-quote is the exception: its quotation's creator IS the quoter, by name, and
    // is still an agent node of its own, not the item.
    let own = quotation(&g, SELF_QUOTE);
    assert_eq!(creators(&own), set([lit(QUOTER)]));
    assert_eq!(
        objects(&g, &own, &format!("{SCHEMA}creator")),
        set([digest("agent", &[QUOTER, "http://quotes.example/"]).into()])
    );
}

#[test]
fn the_sources_tags_are_scoped_to_the_source() {
    let host = host("tags");
    let (mapped, normalized) = (host.mapped(), host.normalized());
    let subject = format!("{DCTERMS}subject");
    // The scoped tag is exactly the node the lift mints for a category written with the
    // source's scheme: the mapping's digest is the lift's.
    let lifted = ikigai_feed::lift(
        format!(
            r#"<feed xmlns="http://www.w3.org/2005/Atom"><id>http://x.example/</id><title>x</title>
<entry><id>http://x.example/1</id><title>t</title><category term="go" scheme="{SCHEME}"/></entry></feed>"#
        )
        .as_bytes(),
        None,
    )
    .unwrap();
    let go = lifted
        .iter()
        .find(|t| t.predicate.as_str() == subject)
        .map(|t| named(&t.object))
        .unwrap();
    assert_eq!(go, digest("category", &[SCHEME, "go"]));

    let shared_go = digest("category", &["", "go"]);
    for id in [QUOTE, NOT_A_QUOTE] {
        let subjects = objects(&mapped, &item(&mapped, id), &subject);
        assert!(subjects.contains(&go.clone().into()), "{id}");
        // The shared node stays a subject: the mapping adds, it does not move.
        assert!(subjects.contains(&shared_go.clone().into()), "{id}");
    }
    assert_eq!(objects(&mapped, &go, RDF_VALUE), set([lit("go")]));
    assert_eq!(
        objects(&mapped, &go, DCAM_MEMBER_OF),
        set([iri(SCHEME).into()])
    );
    // The shared node gains no scheme: a scheme-less `go` is one node across every feed.
    assert!(objects(&mapped, &shared_go, DCAM_MEMBER_OF).is_empty());
    assert_eq!(
        objects(&mapped, &shared_go, RDF_VALUE),
        objects(&normalized, &shared_go, RDF_VALUE)
    );
    let graphs = digest("category", &[SCHEME, "graphs"]);
    assert_eq!(
        objects(&mapped, &item(&mapped, QUOTE), &subject).len(),
        4,
        "go and graphs, shared and scoped"
    );
    assert!(objects(&mapped, &item(&mapped, QUOTE), &subject).contains(&graphs.into()));
}

#[test]
fn the_diff_of_mapped_against_normalized_is_exactly_the_mappings_additions() {
    let host = host("diff");
    let added = host.diff("added");
    let constructed = host.construct(&format!(
        "urn:iki:feed:graph:{FEED},urn:iki:feed:html:{FEED}"
    ));
    assert_eq!(added, constructed);
    assert!(host.diff("removed").is_empty(), "a mapping only adds");
    // The HTML the mapping read is not in the mapped feed: only what it constructed is.
    let mapped = host.mapped();
    assert!(mapped.iter().all(|t| !t.predicate.as_str().starts_with(HTM)
        && t.object.to_string() != format!("<{HTM}Element>")));
    assert_eq!(
        mapped,
        host.normalized().union(&constructed).cloned().collect()
    );
}

#[test]
fn the_feeds_html_is_its_own_cacheable_graph() {
    let host = host("html");
    let html = request(&format!("urn:iki:feed:html:{FEED}"), &[]);
    let g = host.graph(html.clone());
    assert!(host.kernel.is_cached(&html, &Capability::root()));
    let normalized = host.normalized();
    // One tree per item summary, on the item.
    let tree = format!("{HTM}tree");
    for id in [QUOTE, SELF_QUOTE, NOT_A_QUOTE] {
        assert_eq!(objects(&g, &item(&normalized, id), &tree).len(), 1, "{id}");
    }
    // And it is what ikigai-html lifts from the form's `rss:description`.
    let direct = host.graph(request(
        "urn:iki:html:graph",
        &[
            ("graph", &format!("urn:iki:feed:graph:{FEED}")),
            ("predicate", "http://purl.org/rss/1.0/description"),
        ],
    ));
    assert_eq!(g, direct, "the fixture has no content:encoded");
}
