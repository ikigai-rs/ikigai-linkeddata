//! RSS 2.0, RSS 1.0 and Atom lifted into ONE graph form, so everything downstream reads one
//! shape (ledger #2624).
//!
//! `urn:feed:graph content=<feed bytes>` answers the form as Turtle (other RDF faces through
//! `as=`, by [`ikigai_rdf`]'s transreption). The form is RSS 1.0's own vocabulary with Dublin
//! Core, plus the published terms Atom needs; the crate README documents it, with the item IRI
//! rule, the date rule and the category shape. Every node is an IRI: there are no blank nodes.
//!
//! Parsing is feed-rs's ([`feed_rs`]); this crate is the MAPPING from its model to the graph,
//! plus a bounding pre-scan (`src/scan.rs`) that refuses deep input before the parse and recovers the
//! three things feed-rs's model drops and the form needs (RSS 2.0's `isPermaLink`, RSS 1.0's
//! `rdf:about`, `xml:lang`).
#![forbid(unsafe_code)]

mod scan;

pub use scan::{ACCEPTED, MAX_FEED_DEPTH};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use feed_rs::model::{Category, Entry, Feed, FeedType, Link, Person, Text};
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FailureClass,
    Invocation, Iri, ReprType, Representation, Request, Result, Verb,
};
use ikigai_rdf::canonical_triples;
use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfSerializer};
use scan::{Root, Seen};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// The largest feed admitted, in bytes. Past it the body is refused, not truncated.
pub const MAX_FEED_BYTES: usize = 8 << 20;
/// The identity claimed by the configuration-free [`space`].
pub const SPACE_ID: &str = "urn:iki:space:feed";

/// RSS 1.0 (`rss:`), its content module (`content:`) and enclosure module (`enc:`), Dublin
/// Core (`dc:`, `dcterms:`) and DCMI's abstract model (`dcam:`): the form's vocabularies.
pub mod vocab {
    pub const RSS: &str = "http://purl.org/rss/1.0/";
    pub const CONTENT: &str = "http://purl.org/rss/1.0/modules/content/";
    pub const ENC: &str = "http://purl.oclc.org/net/rss_2.0/enc#";
    pub const DC: &str = "http://purl.org/dc/elements/1.1/";
    pub const DCTERMS: &str = "http://purl.org/dc/terms/";
    pub const DCAM: &str = "http://purl.org/dc/dcam/";
    pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
}

/// The published RSS 1.0, content-module, enclosure-module and `dcam:` terms the form speaks,
/// restated as Turtle from their specifications (`dc:`, `dcterms:`, RDF and RDFS are well known).
pub const VOCABULARY: &str = include_str!("vocabulary.ttl");

/// The lift with its transreption declared.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:feed:graph"), FeedGraph)
        .named(space_iri("feed"))
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
/// The feed media types the transreptor reads. RDF/XML is read only with an RSS 1.0 root.
const FEED_TYPES: [&str; 3] = [
    "application/rss+xml",
    "application/atom+xml",
    "application/rdf+xml",
];
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}

struct FeedGraph;

#[async_trait]
impl Endpoint for FeedGraph {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let face = optional(inv, "as")?.unwrap_or("text/turtle");
        if !FACES.contains(&face) {
            return Err(invalid("as", format!("unsupported RDF face: {face}")));
        }
        let base = optional(inv, "base")?.filter(|b| !b.is_empty());
        if let Some(base) = base {
            NamedNode::new(base).map_err(|e| invalid("base", format!("not an IRI: {e}")))?;
        }
        // Whichever way `content` came (inline, by reference, streamed), it is read under a
        // pool one byte past the bound, so a stream past it is refused as it arrives.
        let body = inv.content_body("content").await?;
        let bytes = inv
            .with_memory_limit(MAX_FEED_BYTES as u64 + 1)
            .collect(body)
            .await?;
        if bytes.len() > MAX_FEED_BYTES {
            return Err(Error::OverBudget {
                detail: format!("feed exceeds {MAX_FEED_BYTES} bytes"),
            });
        }
        let turtle = serialize(lift(&bytes, base)?)?;
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
        "feed-graph"
    }

    fn describe(&self) -> Description {
        let desc = Description::new("feed-graph")
            .title("Feed graph form")
            .summary(
                "An RSS 0.9x/2.0, RSS 1.0 or Atom 1.0 feed lifted into one skolemized RDF \
                 form: RSS 1.0's vocabulary with Dublin Core, items named by their own IRI",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary("the feed's bytes — usually piped in from a fetch")
                    .class(XSD_STRING)
                    .reads_body(),
            )
            .input(
                ArgSpec::new("base")
                    .summary(
                        "the feed's own IRI: names the channel, resolves relative links, and \
                         seeds the digest IRI of an item with no IRI of its own",
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
            // The lift is a projection: it keeps the form's fields and drops the rest (iTunes,
            // podcast and MediaRSS detail, unknown extensions), so it is declared LOSSY and a
            // plan routes through it only with the caller's consent (`lossy=allow`). RSS 1.0 is
            // RDF already, so ikigai-rdf's lossless RDF/XML edge is preferred where it is bound.
            .transreptor(FEED_TYPES, FACES)
            .lossy()
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

/// Lift a feed's bytes into the form's triples, in no particular order.
///
/// `base` is the feed's own IRI: it names the channel, resolves relative links (beneath any
/// `xml:base`), and seeds the digest IRI of an item with no IRI of its own. A body that is not
/// a feed, or nests deeper than [`MAX_FEED_DEPTH`], is [`Error::InvalidArgument`] on `content`.
pub fn lift(bytes: &[u8], base: Option<&str>) -> Result<Vec<Triple>> {
    let scan = scan::scan(bytes)?;
    let parser = feed_rs::parser::Builder::new()
        .base_uri(base)
        // An empty id is "the source gave none": the IRI rule below decides, not a generator.
        .id_generator(|_, _, _| String::new())
        .sanitize_content(false)
        .build();
    let feed = parser
        .parse(bytes)
        .map_err(|e| invalid("content", format!("{e}; {ACCEPTED}")))?;
    let mut seen = scan.entries;
    // The scan names entries as feed-rs does; if the counts ever disagree, its findings are
    // not trusted (each entry reads as if the scan saw nothing).
    if seen.len() != feed.entries.len() {
        seen = vec![Seen::default(); feed.entries.len()];
    }
    let mut out = Graph::default();
    let channel = channel_iri(&feed, base, scan.root, scan.channel_about.as_deref());
    let language = feed
        .language
        .clone()
        .or(scan.channel_lang)
        .map(|l| l.to_lowercase());
    out.channel(&feed, &channel, language.as_deref());
    for (entry, seen) in feed.entries.iter().zip(&seen) {
        let item = item_iri(entry, seen, scan.root, &channel);
        out.item(entry, seen, &item, &channel, language.as_deref());
    }
    Ok(out.0.into_iter().collect())
}

/// The form as Turtle, triples in one order, under the form's prefixes.
fn serialize(triples: Vec<Triple>) -> Result<Vec<u8>> {
    let failed = |e: std::io::Error| Error::Endpoint(e.to_string());
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle);
    for (prefix, iri) in [
        ("rss", vocab::RSS),
        ("content", vocab::CONTENT),
        ("enc", vocab::ENC),
        ("dc", vocab::DC),
        ("dcterms", vocab::DCTERMS),
        ("dcam", vocab::DCAM),
        ("rdf", vocab::RDF),
        ("rdfs", vocab::RDFS),
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

/// An absolute IRI, or nothing.
fn absolute(iri: &str) -> Option<NamedNode> {
    NamedNode::new(iri.trim()).ok()
}

/// A digest IRI (RFC 6920's `nih:` form): SHA-256 over `kind` and `parts`, each followed by
/// U+0000, which no XML text can hold, written in lowercase hex.
fn digest(kind: &str, parts: &[&str]) -> NamedNode {
    let mut hash = Sha256::new();
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    NamedNode::new_unchecked(format!("nih:sha-256;{hex}"))
}

/// The channel's IRI, the first of: `base`; an `atom:link rel="self"`; the Atom feed's `id`;
/// RSS 1.0's channel `rdf:about`; the channel's link; a digest of its title.
fn channel_iri(feed: &Feed, base: Option<&str>, root: Root, about: Option<&str>) -> NamedNode {
    base.and_then(absolute)
        .or_else(|| {
            feed.links
                .iter()
                .find(|l| l.rel.as_deref() == Some("self"))
                .and_then(|l| absolute(&l.href))
        })
        .or_else(|| (root == Root::Atom).then(|| absolute(&feed.id)).flatten())
        .or_else(|| about.and_then(absolute))
        .or_else(|| alternate(&feed.links).and_then(|l| absolute(&l.href)))
        .unwrap_or_else(|| digest("channel", &[text(&feed.title).unwrap_or("")]))
}

/// The item IRI rule, in order (the README states it):
///
/// 1. the entry's own IRI: Atom's `atom:id`, RSS 1.0's `rdf:about`, when absolute;
/// 2. an RSS `guid` with `isPermaLink="true"`, or with no `isPermaLink` when it is an
///    absolute IRI (`isPermaLink="false"` never names the item);
/// 3. the item's link;
/// 4. a digest IRI over the channel IRI, the title and the date.
fn item_iri(entry: &Entry, seen: &Seen, root: Root, channel: &NamedNode) -> NamedNode {
    let own = match root {
        Root::Atom => absolute(&entry.id),
        Root::Rdf => seen.about.as_deref().and_then(absolute),
        Root::Rss if seen.permalink != Some(false) => absolute(&entry.id),
        Root::Rss => None,
    };
    own.or_else(|| alternate(&entry.links).and_then(|l| absolute(&l.href)))
        .unwrap_or_else(|| {
            let date = entry.published.or(entry.updated).map(date_time);
            digest(
                "item",
                &[
                    channel.as_str(),
                    text(&entry.title).unwrap_or(""),
                    date.as_deref().unwrap_or(""),
                ],
            )
        })
}

/// The first link with no `rel` or `rel="alternate"`, and no other role (comments).
fn alternate(links: &[Link]) -> Option<&Link> {
    links
        .iter()
        .find(|l| l.target.is_none() && matches!(l.rel.as_deref(), None | Some("alternate")))
}

fn text(t: &Option<Text>) -> Option<&str> {
    t.as_ref()
        .map(|t| t.content.as_str())
        .filter(|s| !s.is_empty())
}

/// An instant as an `xsd:dateTime` lexical form, in UTC.
fn date_time(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

/// The triples under construction: a set, so a fact stated twice is one triple.
#[derive(Default)]
struct Graph(HashSet<Triple>);

impl Graph {
    fn add(&mut self, s: &NamedNode, ns: &str, local: &str, o: impl Into<Term>) {
        self.0.insert(Triple::new(
            NamedOrBlankNode::NamedNode(s.clone()),
            NamedNode::new_unchecked(format!("{ns}{local}")),
            o,
        ));
    }

    fn string(&mut self, s: &NamedNode, ns: &str, local: &str, value: Option<&str>) {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            self.add(s, ns, local, Literal::new_simple_literal(value));
        }
    }

    fn channel(&mut self, feed: &Feed, channel: &NamedNode, language: Option<&str>) {
        self.add(channel, vocab::RDF, "type", rss("channel"));
        self.string(channel, vocab::RSS, "title", text(&feed.title));
        self.string(
            channel,
            vocab::RSS,
            "link",
            alternate(&feed.links).map(|l| l.href.as_str()),
        );
        self.string(channel, vocab::RSS, "description", text(&feed.description));
        self.string(channel, vocab::DC, "language", language);
        if feed.feed_type == FeedType::Atom {
            self.string(channel, vocab::DC, "identifier", Some(&feed.id));
        }
        self.string(channel, vocab::DC, "rights", text(&feed.rights));
        self.people(channel, &feed.authors);
        self.dates(channel, feed.published, feed.updated);
        self.categories(channel, &feed.categories);
    }

    fn item(
        &mut self,
        entry: &Entry,
        seen: &Seen,
        item: &NamedNode,
        channel: &NamedNode,
        language: Option<&str>,
    ) {
        self.add(item, vocab::RDF, "type", rss("item"));
        self.add(item, vocab::DCTERMS, "isPartOf", channel.clone());
        self.string(item, vocab::RSS, "title", text(&entry.title));
        self.string(
            item,
            vocab::RSS,
            "link",
            alternate(&entry.links).map(|l| l.href.as_str()),
        );
        self.string(item, vocab::DC, "identifier", Some(&entry.id));
        self.string(item, vocab::RSS, "description", text(&entry.summary));
        self.string(
            item,
            vocab::CONTENT,
            "encoded",
            entry.content.as_ref().and_then(|c| c.body.as_deref()),
        );
        self.string(item, vocab::DC, "rights", text(&entry.rights));
        let lang = seen.lang.as_ref().map(|l| l.to_lowercase());
        if lang.as_deref() != language {
            self.string(item, vocab::DC, "language", lang.as_deref());
        }
        self.people(item, &entry.authors);
        self.dates(item, entry.published, entry.updated);
        self.categories(item, &entry.categories);
        for link in &entry.links {
            let Some(href) = absolute(&link.href) else {
                continue;
            };
            match link.rel.as_deref() {
                Some("enclosure") => {
                    self.enclosure(item, href, link.media_type.as_deref(), link.length)
                }
                Some("via") => self.add(item, vocab::DCTERMS, "source", href),
                Some("related") => self.add(item, vocab::DCTERMS, "relation", href),
                _ => {}
            }
        }
        // RSS 2.0's <enclosure>, and MediaRSS content, arrive as media objects.
        for content in entry.media.iter().flat_map(|m| &m.content) {
            if let Some(href) = content.url.as_ref().and_then(|u| absolute(u.as_str())) {
                let media_type = content.content_type.as_ref().map(|t| t.to_string());
                self.enclosure(item, href, media_type.as_deref(), content.size);
            }
        }
    }

    fn people(&mut self, s: &NamedNode, people: &[Person]) {
        for person in people {
            // RSS 2.0 writes `email (Name)`; feed-rs keeps the parentheses on the name.
            let name = person
                .name
                .as_deref()
                .map(|n| {
                    n.trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim()
                })
                .filter(|n| !n.is_empty())
                .or(person.email.as_deref().map(str::trim));
            self.string(s, vocab::DC, "creator", name);
        }
    }

    /// `dcterms:issued` the published instant, `dcterms:modified` the updated one when it is
    /// another, and `dc:date` the first of the two (the Book's river sorts on it).
    fn dates(
        &mut self,
        s: &NamedNode,
        published: Option<DateTime<Utc>>,
        updated: Option<DateTime<Utc>>,
    ) {
        let typed = |at| Literal::new_typed_literal(date_time(at), xsd::DATE_TIME);
        if let Some(at) = published.or(updated) {
            self.add(s, vocab::DC, "date", typed(at));
        }
        if let Some(at) = published {
            self.add(s, vocab::DCTERMS, "issued", typed(at));
        }
        // feed-rs copies RSS 2.0's pubDate into `updated` when there is no update: an equal
        // instant states nothing more, so it is not written twice.
        if let Some(at) = updated.filter(|at| Some(*at) != published) {
            self.add(s, vocab::DCTERMS, "modified", typed(at));
        }
    }

    /// Each category: `dc:subject` its term (the Book's queries match it), and `dcterms:subject`
    /// a node named by digest of scheme and term, carrying `rdf:value` the term, `rdfs:label`
    /// the label, and `dcam:memberOf` the scheme. A scheme that is no absolute IRI is a node
    /// named by digest of its text, labelled with it.
    fn categories(&mut self, s: &NamedNode, categories: &[Category]) {
        for category in categories {
            let term = category.term.trim();
            if term.is_empty() {
                continue;
            }
            let scheme = category
                .scheme
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let node = digest("category", &[scheme.unwrap_or(""), term]);
            self.string(s, vocab::DC, "subject", Some(term));
            self.add(s, vocab::DCTERMS, "subject", node.clone());
            self.add(
                &node,
                vocab::RDF,
                "value",
                Literal::new_simple_literal(term),
            );
            self.string(&node, vocab::RDFS, "label", category.label.as_deref());
            if let Some(scheme) = scheme {
                let named = absolute(scheme).unwrap_or_else(|| {
                    let named = digest("scheme", &[scheme]);
                    self.string(&named, vocab::RDFS, "label", Some(scheme));
                    named
                });
                self.add(&node, vocab::DCAM, "memberOf", named);
            }
        }
    }

    fn enclosure(
        &mut self,
        item: &NamedNode,
        href: NamedNode,
        media: Option<&str>,
        length: Option<u64>,
    ) {
        self.add(item, vocab::ENC, "enclosure", href.clone());
        self.string(&href, vocab::ENC, "type", media);
        if let Some(length) = length {
            self.add(
                &href,
                vocab::ENC,
                "length",
                Literal::new_typed_literal(length.to_string(), xsd::INTEGER),
            );
        }
    }
}

fn rss(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{}{local}", vocab::RSS))
}
