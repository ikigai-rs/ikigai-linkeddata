//! The form, fixture by fixture (all synthetic, `tests/fixtures/`): the item IRI rule branch by
//! branch, both date formats, categories, enclosures, `xml:base`, the refusals, and the one
//! logical item written as RSS 2.0 and as Atom lifting to the same triples.

use futures::executor::block_on;
use ikigai_core::{
    select_transreptor, select_transreptor_with, ArgRef, Capability, Error, Fallback, Iri, Kernel,
    Request, TransreptionPolicy, Verb,
};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;

const RSS: &str = "http://purl.org/rss/1.0/";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const DCAM: &str = "http://purl.org/dc/dcam/";
const ENC: &str = "http://purl.oclc.org/net/rss_2.0/enc#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn kernel() -> Kernel {
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_rdf::space()),
    ])))
}

fn issue(
    content: &[u8],
    args: &[(&str, &str)],
) -> ikigai_core::Result<ikigai_core::Representation> {
    let request = args.iter().fold(
        Request::new(Verb::Source, Iri::parse("urn:iki:feed:graph").unwrap())
            .with_arg("content", ArgRef::Inline(content.to_vec())),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    );
    block_on(kernel().issue(request, &Capability::root()))
}

/// The form of `content` through the resource, parsed back from its Turtle face.
fn graph(content: &[u8], base: Option<&str>) -> HashSet<Triple> {
    let args: Vec<_> = base.map(|b| ("base", b)).into_iter().collect();
    let rep = issue(content, &args).unwrap();
    assert!(rep.repr_type.media_type.starts_with("text/turtle"));
    parse(&rep.bytes, oxrdfio::RdfFormat::Turtle)
}

fn parse(bytes: &[u8], format: oxrdfio::RdfFormat) -> HashSet<Triple> {
    oxrdfio::RdfParser::from_format(format)
        .for_slice(bytes)
        .map(|q| q.unwrap().into())
        .collect()
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn objects(g: &HashSet<Triple>, s: &str, p: &str) -> Vec<Term> {
    let (s, p) = (NamedOrBlankNode::from(iri(s)), iri(p));
    let mut found: Vec<Term> = g
        .iter()
        .filter(|t| t.subject == s && t.predicate == p)
        .map(|t| t.object.clone())
        .collect();
    found.sort_by_key(|t| t.to_string());
    found
}

fn items(g: &HashSet<Triple>) -> Vec<String> {
    let mut items: Vec<String> = g
        .iter()
        .filter(|t| {
            t.predicate.as_str() == format!("{RDF}type")
                && t.object == iri(&format!("{RSS}item")).into()
        })
        .map(|t| match &t.subject {
            NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
            other => panic!("blank item {other}"),
        })
        .collect();
    items.sort();
    items
}

fn lit(s: &str) -> Term {
    Literal::new_simple_literal(s).into()
}

fn date(s: &str) -> Term {
    Literal::new_typed_literal(s, iri(XSD_DATE_TIME)).into()
}

/// The digest rule, recomputed here from the README's words: SHA-256 over the kind and each
/// part, each followed by U+0000, as `nih:sha-256;` and lowercase hex.
fn digest(kind: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("nih:sha-256;{hex}")
}

#[test]
fn no_fixture_lifts_a_blank_node() {
    for name in [
        "rss2.xml",
        "rss1.xml",
        "atom.xml",
        "same-rss2.xml",
        "same-atom.xml",
    ] {
        for t in graph(&fixture(name), None) {
            assert!(
                matches!(t.subject, NamedOrBlankNode::NamedNode(_)),
                "{name}: {t}"
            );
            assert!(!matches!(t.object, Term::BlankNode(_)), "{name}: {t}");
        }
    }
}

#[test]
fn rss2_item_iris_follow_the_rule_branch_by_branch() {
    let g = graph(&fixture("rss2.xml"), None);
    let channel = "http://feeds.example.org/";
    let no_link = digest(
        "item",
        &[channel, "No link and no guid", "2026-10-09T08:00:00Z"],
    );
    assert_eq!(
        items(&g),
        vec![
            // xml:base on the item resolves its relative link (step 3).
            "http://feeds.example.org/archive/g.html".to_string(),
            // isPermaLink="true": the guid, not the link (step 2).
            "http://feeds.example.org/posts/a".into(),
            // isPermaLink="false": never the guid; the link (step 3).
            "http://feeds.example.org/posts/b".into(),
            // No isPermaLink, the guid an absolute IRI: the guid, not the link (step 2).
            "http://feeds.example.org/posts/c".into(),
            // No isPermaLink, the guid no IRI: the link (step 3).
            "http://feeds.example.org/posts/d".into(),
            // isPermaLink="false" though the guid IS an absolute IRI: the link (step 3).
            "http://feeds.example.org/posts/f".into(),
            // No guid, no link: the digest of channel, title and date (step 4).
            no_link.clone(),
        ]
    );
    // The guid is kept as written, whatever named the item.
    assert_eq!(
        objects(
            &g,
            "http://feeds.example.org/posts/b",
            &format!("{DC}identifier")
        ),
        vec![lit("post-b-0001")]
    );
    assert_eq!(
        objects(
            &g,
            "http://feeds.example.org/posts/f",
            &format!("{DC}identifier")
        ),
        vec![lit("http://feeds.example.org/guids/f")]
    );
    // An item with no link has no rss:link, and still belongs to its channel.
    assert!(objects(&g, &no_link, &format!("{RSS}link")).is_empty());
    assert_eq!(
        objects(&g, &no_link, &format!("{DCTERMS}isPartOf")),
        vec![Term::from(iri(channel))]
    );
}

#[test]
fn atom_and_rss1_items_are_named_by_their_own_iri() {
    let g = graph(&fixture("atom.xml"), None);
    assert_eq!(
        items(&g),
        vec![
            // An atom:id that is not an absolute IRI falls to the link, resolved under xml:base.
            "http://atom.example.org/blog/posts/2".to_string(),
            // atom:id (step 1).
            "tag:atom.example.org,2026:entry-1".into(),
        ]
    );
    let g = graph(&fixture("rss1.xml"), None);
    // rdf:about (step 1): not the item's link, which differs.
    assert_eq!(
        items(&g),
        vec![
            "http://rdf.example.org/items/1".to_string(),
            "http://rdf.example.org/items/2".into()
        ]
    );
    assert_eq!(
        objects(
            &g,
            "http://rdf.example.org/items/1",
            &format!("{DCTERMS}isPartOf")
        ),
        vec![Term::from(iri("http://rdf.example.org/index.rdf"))]
    );
}

#[test]
fn the_digest_seeds_on_base_when_it_is_given() {
    let body = br#"<rss version="2.0"><channel><title>T</title><item><title>Untitled thing</title></item></channel></rss>"#;
    let base = "http://base.example.org/feed.xml";
    let g = graph(body, Some(base));
    // No date: the date part is empty.
    assert_eq!(
        items(&g),
        vec![digest("item", &[base, "Untitled thing", ""])]
    );
    // With no base and no link, the channel itself is named by digest of its title.
    let g = graph(body, None);
    let channel = digest("channel", &["T"]);
    assert_eq!(
        items(&g),
        vec![digest("item", &[&channel, "Untitled thing", ""])]
    );
}

#[test]
fn base_names_the_channel_and_resolves_relative_links() {
    let body = br#"<rss version="2.0"><channel><title>T</title><link>/</link><item><link>posts/x</link></item></channel></rss>"#;
    let g = graph(body, Some("http://base.example.org/feeds/main.xml"));
    assert_eq!(
        items(&g),
        vec!["http://base.example.org/feeds/posts/x".to_string()]
    );
    assert_eq!(
        objects(
            &g,
            "http://base.example.org/feeds/main.xml",
            &format!("{RSS}link")
        ),
        vec![lit("http://base.example.org/")]
    );
}

#[test]
fn atom_xml_base_resolves_links_enclosures_and_self() {
    let g = graph(&fixture("atom.xml"), None);
    let channel = "http://atom.example.org/blog/feed.atom";
    let entry = "tag:atom.example.org,2026:entry-1";
    assert_eq!(
        objects(&g, channel, &format!("{RSS}link")),
        vec![lit("http://atom.example.org/blog/")]
    );
    assert_eq!(
        objects(&g, channel, &format!("{DC}identifier")),
        vec![lit("tag:atom.example.org,2026:feed")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{RSS}link")),
        vec![lit("http://atom.example.org/blog/posts/1")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{ENC}enclosure")),
        vec![Term::from(iri("http://atom.example.org/media/1.ogg"))]
    );
    assert_eq!(
        objects(
            &g,
            "http://atom.example.org/media/1.ogg",
            &format!("{ENC}type")
        ),
        vec![lit("audio/ogg")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{DCTERMS}source")),
        vec![Term::from(iri("http://elsewhere.example.net/original"))]
    );
    assert_eq!(
        objects(&g, entry, &format!("{DCTERMS}relation")),
        vec![Term::from(iri("http://elsewhere.example.net/related"))]
    );
    // The feed's xml:lang is the channel's language; an entry's own, when it differs, is its.
    assert_eq!(
        objects(&g, channel, &format!("{DC}language")),
        vec![lit("en")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{DC}language")),
        vec![lit("fr")]
    );
    assert!(objects(
        &g,
        "http://atom.example.org/blog/posts/2",
        &format!("{DC}language")
    )
    .is_empty());
}

#[test]
fn dates_are_xsd_date_times_from_both_formats() {
    let g = graph(&fixture("rss2.xml"), None);
    let a = "http://feeds.example.org/posts/a";
    // RFC 5322 with a zone NAME (PDT, UTC-7), as RSS writers write it.
    assert_eq!(
        objects(&g, a, &format!("{DC}date")),
        vec![date("2026-10-07T17:00:00Z")]
    );
    assert_eq!(
        objects(&g, a, &format!("{DCTERMS}issued")),
        vec![date("2026-10-07T17:00:00Z")]
    );
    // A numeric offset.
    assert_eq!(
        objects(&g, "http://feeds.example.org/posts/b", &format!("{DC}date")),
        vec![date("2026-10-08T07:30:00Z")]
    );
    // Only a pubDate: nothing claims the item was modified.
    assert!(objects(&g, a, &format!("{DCTERMS}modified")).is_empty());

    let g = graph(&fixture("atom.xml"), None);
    let entry = "tag:atom.example.org,2026:entry-1";
    // RFC 3339 with an offset; published and updated kept apart; dc:date the published one.
    assert_eq!(
        objects(&g, entry, &format!("{DCTERMS}issued")),
        vec![date("2026-10-07T17:00:00Z")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{DCTERMS}modified")),
        vec![date("2026-10-09T12:00:00Z")]
    );
    assert_eq!(
        objects(&g, entry, &format!("{DC}date")),
        vec![date("2026-10-07T17:00:00Z")]
    );
    // Only updated: dc:date is it.
    let two = "http://atom.example.org/blog/posts/2";
    assert_eq!(
        objects(&g, two, &format!("{DC}date")),
        vec![date("2026-10-08T00:00:00Z")]
    );
    assert!(objects(&g, two, &format!("{DCTERMS}issued")).is_empty());

    // RSS 1.0's dc:date (W3CDTF).
    let g = graph(&fixture("rss1.xml"), None);
    assert_eq!(
        objects(&g, "http://rdf.example.org/items/1", &format!("{DC}date")),
        vec![date("2026-10-07T08:00:00Z")]
    );
}

#[test]
fn categories_are_nodes_with_term_scheme_and_label() {
    let g = graph(&fixture("rss2.xml"), None);
    let a = "http://feeds.example.org/posts/a";
    let tags = digest("category", &["http://example.org/tags", "rust"]);
    let notes = digest("category", &["", "Notes"]);
    let syndic8 = digest("category", &["Syndic8", "1765"]);
    let mut nodes = [tags.clone(), notes.clone(), syndic8.clone()];
    nodes.sort();
    assert_eq!(
        objects(&g, a, &format!("{DCTERMS}subject")),
        nodes.iter().map(|n| Term::from(iri(n))).collect::<Vec<_>>()
    );
    // dc:subject carries each term, as the Book's queries read it.
    assert_eq!(
        objects(&g, a, &format!("{DC}subject")),
        vec![lit("1765"), lit("Notes"), lit("rust")]
    );
    // With a scheme that is an IRI.
    assert_eq!(
        objects(&g, &tags, &format!("{RDF}value")),
        vec![lit("rust")]
    );
    assert_eq!(
        objects(&g, &tags, &format!("{DCAM}memberOf")),
        vec![Term::from(iri("http://example.org/tags"))]
    );
    // With no scheme.
    assert!(objects(&g, &notes, &format!("{DCAM}memberOf")).is_empty());
    // With a scheme that is not an IRI: a node named by digest, labelled with the text.
    let scheme = digest("scheme", &["Syndic8"]);
    assert_eq!(
        objects(&g, &syndic8, &format!("{DCAM}memberOf")),
        vec![Term::from(iri(&scheme))]
    );
    assert_eq!(
        objects(&g, &scheme, &format!("{RDFS}label")),
        vec![lit("Syndic8")]
    );
    // Atom's label, on the same node an RSS feed names for the same scheme and term.
    let g = graph(&fixture("atom.xml"), None);
    assert_eq!(
        objects(&g, &tags, &format!("{RDFS}label")),
        vec![lit("Rust")]
    );
}

#[test]
fn an_rss2_enclosure_is_carried() {
    let g = graph(&fixture("rss2.xml"), None);
    let mp3 = "http://media.example.org/a.mp3";
    assert_eq!(
        objects(
            &g,
            "http://feeds.example.org/posts/a",
            &format!("{ENC}enclosure")
        ),
        vec![Term::from(iri(mp3))]
    );
    assert_eq!(
        objects(&g, mp3, &format!("{ENC}type")),
        vec![lit("audio/mpeg")]
    );
    assert_eq!(
        objects(&g, mp3, &format!("{ENC}length")),
        vec![Term::from(Literal::new_typed_literal(
            "12345",
            iri("http://www.w3.org/2001/XMLSchema#integer")
        ))]
    );
}

/// The one logical item, as RSS 2.0 and as Atom: the same triples, but for exactly what only
/// Atom carries (the feed's atom:id, the entry's distinct `updated`, a category's label).
#[test]
fn the_same_item_as_rss2_and_atom_lifts_to_the_same_triples() {
    let base = Some("http://same.example.org/feed");
    let rss = graph(&fixture("same-rss2.xml"), base);
    let atom = graph(&fixture("same-atom.xml"), base);
    let only_atom: HashSet<Triple> = atom.difference(&rss).cloned().collect();
    assert!(
        rss.is_subset(&atom),
        "RSS 2.0 only: {:?}",
        rss.difference(&atom).collect::<Vec<_>>()
    );
    let t = |s: &str, p: String, o: Term| Triple::new(iri(s), iri(&p), o);
    let expected: HashSet<Triple> = [
        t(
            "http://same.example.org/feed",
            format!("{DC}identifier"),
            lit("tag:same.example.org,2026:feed"),
        ),
        t(
            "http://same.example.org/posts/one",
            format!("{DCTERMS}modified"),
            date("2026-10-08T08:00:00Z"),
        ),
        t(
            &digest("category", &["http://example.org/tags", "rust"]),
            format!("{RDFS}label"),
            lit("Rust"),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(only_atom, expected);
}

#[test]
fn other_faces_come_through_rdf_transreption() {
    let rep = issue(&fixture("rss1.xml"), &[("as", "application/n-triples")]).unwrap();
    assert!(rep
        .repr_type
        .media_type
        .starts_with("application/n-triples"));
    let nt = parse(&rep.bytes, oxrdfio::RdfFormat::NTriples);
    assert_eq!(nt, graph(&fixture("rss1.xml"), None));
}

#[test]
fn a_body_that_is_not_a_feed_is_refused_naming_the_formats() {
    let json_feed = br#"{"version":"https://jsonfeed.org/version/1.1","title":"x","items":[]}"#;
    let rdf = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="urn:x"/></rdf:RDF>"#;
    for body in [
        &b"<html><body/></html>"[..],
        json_feed,
        b"not xml at all",
        b"<rss version=\"2.0\"><channel>",
        b"",
        rdf,
    ] {
        match issue(body, &[]) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "content");
                assert!(
                    detail.contains("Atom 1.0") && detail.contains("RSS 1.0"),
                    "{detail}"
                );
            }
            other => panic!("{:?}: {other:?}", String::from_utf8_lossy(body)),
        }
    }
    assert!(
        matches!(issue(&fixture("rss2.xml"), &[("base", "not an iri")]), Err(Error::InvalidArgument { name, .. }) if name == "base")
    );
}

#[test]
fn a_body_past_the_size_bound_is_refused() {
    let pad = " ".repeat(ikigai_feed::MAX_FEED_BYTES);
    let body = format!("<rss version=\"2.0\"><channel><title>T</title></channel></rss>{pad}");
    assert!(matches!(
        issue(body.as_bytes(), &[]),
        Err(Error::OverBudget { .. })
    ));
    // At the bound exactly, it is read.
    let body = "<rss version=\"2.0\"><channel><title>T</title></channel></rss>".to_string();
    let body = format!(
        "{body}{}",
        " ".repeat(ikigai_feed::MAX_FEED_BYTES - body.len())
    );
    assert!(issue(body.as_bytes(), &[]).is_ok());
}

/// Registered as a transreptor from each feed type; LOSSY, so planned only with consent, and
/// RDF/XML still goes to ikigai-rdf's lossless edge where both are bound.
#[test]
fn the_lift_is_a_lossy_transreptor_for_the_feed_types() {
    let space = Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_rdf::space()),
    ]);
    let allow = TransreptionPolicy::allow_lossy();
    for from in ["application/rss+xml", "application/atom+xml"] {
        assert!(
            select_transreptor(&space, from, "text/turtle").is_none(),
            "{from}: lossless only"
        );
        let plan = select_transreptor_with(&space, from, "text/turtle", &allow).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].endpoint, "urn:iki:feed:graph");
        assert!(!plan[0].lossless);
    }
    let plan =
        select_transreptor_with(&space, "application/rdf+xml", "text/turtle", &allow).unwrap();
    assert_eq!(plan[0].endpoint, "urn:rdf:transrept");
    // Alone, the lift is the RDF/XML edge too: RSS 1.0 lifts, other RDF/XML is refused.
    let alone = ikigai_feed::space();
    let plan =
        select_transreptor_with(&alone, "application/rdf+xml", "text/turtle", &allow).unwrap();
    assert_eq!(plan[0].endpoint, "urn:iki:feed:graph");
}
