//! Who names an item (ledger #2707). An item's IRI is scoped to the feed it was lifted from,
//! so a feed that reuses another feed's `atom:id` names an item of its own and attaches nothing
//! to the other's; and entries that share one id within a feed are one item, the most recently
//! updated, so a feed repeating its id cannot multiply the river's rows.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use std::collections::HashSet;
use std::sync::Arc;

const RSS: &str = "http://purl.org/rss/1.0/";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The Book's river (`tests/river.rs`), verbatim.
const RIVER: &str = r#"PREFIX rss: <http://purl.org/rss/1.0/>
PREFIX dc: <http://purl.org/dc/elements/1.1/>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX content: <http://purl.org/rss/1.0/modules/content/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
# The river: every item of every feed, newest first.
SELECT ?item ?title ?link ?date ?feed ?feedTitle ?summary
WHERE {
  ?item a rss:item ; rss:link ?link ; dcterms:isPartOf ?feed .
  ?feed rss:title ?feedTitle .
  OPTIONAL { ?item rss:title ?t }
  OPTIONAL { ?item dc:date ?date }
  OPTIONAL { ?item rss:description ?summary }
  BIND(COALESCE(?t, "(untitled)") AS ?title)
}
ORDER BY DESC(?date)
"#;

const VICTIM: &str = "https://victim.example.org/feed.xml";
const HOSTILE: &str = "https://hostile.example.net/feed.xml";
const SHARED_ID: &str = "tag:victim.example.org,2026:post-1";

fn request(iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.to_vec())),
    )
}

fn turtle(feed: &str, base: &str) -> Vec<u8> {
    let kernel = Kernel::new(Arc::new(ikigai_feed::space()));
    block_on(kernel.issue(
        request(
            "urn:iki:feed:graph",
            &[("content", feed.as_bytes()), ("base", base.as_bytes())],
        ),
        &Capability::root(),
    ))
    .unwrap()
    .bytes
}

fn graph(feed: &str, base: &str) -> HashSet<Triple> {
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(&turtle(feed, base))
        .map(|q| q.unwrap().into())
        .collect()
}

/// The river over each `(feed, base)` lifted and listed as a `graph=` source, as CSV rows.
fn river(feeds: &[(&str, &str)]) -> Vec<String> {
    let mut graphs = EndpointSpace::new();
    let mut names = Vec::new();
    for (i, (feed, base)) in feeds.iter().enumerate() {
        let body = turtle(feed, base);
        let name = format!("urn:test:feed:{i}");
        graphs = graphs.bind(
            Exact::new(name.clone()),
            FnEndpoint::new("fixture", move |_| {
                Ok(Representation::new(ReprType::new("text/turtle"), body.clone()).cacheable())
            }),
        );
        names.push(name);
    }
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(graphs),
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_rdf::space()),
    ])));
    let csv = block_on(kernel.issue(
        request(
            "urn:sparql:select",
            &[
                ("query", RIVER.as_bytes()),
                ("graph", names.join(",").as_bytes()),
                ("as", b"text/csv"),
            ],
        ),
        &Capability::root(),
    ))
    .unwrap();
    String::from_utf8(csv.bytes)
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn items(g: &HashSet<Triple>) -> Vec<NamedNode> {
    let mut items: Vec<NamedNode> = g
        .iter()
        .filter(|t| {
            t.predicate.as_str() == format!("{RDF}type")
                && t.object == iri(&format!("{RSS}item")).into()
        })
        .map(|t| match &t.subject {
            NamedOrBlankNode::NamedNode(n) => n.clone(),
            other => panic!("blank item {other}"),
        })
        .collect();
    items.sort();
    items
}

fn objects(g: &HashSet<Triple>, s: &NamedNode, p: &str) -> Vec<Term> {
    let mut found: Vec<Term> = g
        .iter()
        .filter(|t| t.subject == s.clone().into() && t.predicate.as_str() == p)
        .map(|t| t.object.clone())
        .collect();
    found.sort_by_key(|t| t.to_string());
    found
}

fn atom(title: &str, entries: &[String]) -> String {
    format!(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><id>tag:{title}</id><title>{title}</title><updated>2026-10-30T00:00:00Z</updated>{}</feed>"#,
        entries.concat()
    )
}

fn entry(id: &str, title: &str, link: &str, updated: &str) -> String {
    format!(
        r#"<entry><id>{id}</id><title>{title}</title><link href="{link}"/><updated>{updated}</updated><summary>{title} summary</summary></entry>"#
    )
}

#[test]
fn a_feed_cannot_claim_another_feeds_item_by_reusing_its_id() {
    let victim = atom(
        "Victim",
        &[entry(
            SHARED_ID,
            "Real headline",
            "https://victim.example.org/posts/1",
            "2026-10-29T09:00:00Z",
        )],
    );
    let hostile = atom(
        "Hostile",
        &[entry(
            SHARED_ID,
            "Click here",
            "https://hostile.example.net/phish",
            "2099-01-01T00:00:00Z",
        )],
    );
    let v = graph(&victim, VICTIM);
    let h = graph(&hostile, HOSTILE);
    let (v_items, h_items) = (items(&v), items(&h));
    assert_eq!((v_items.len(), h_items.len()), (1, 1));
    // Two items, not one: the id names neither, and each item's facts are its own feed's.
    assert_ne!(v_items, h_items);
    let union: HashSet<Triple> = v.union(&h).cloned().collect();
    let victim_item = &v_items[0];
    assert_eq!(
        objects(&union, victim_item, &format!("{RSS}link")),
        vec![Term::from(oxrdf::Literal::new_simple_literal(
            "https://victim.example.org/posts/1"
        ))]
    );
    assert_eq!(
        objects(&union, victim_item, &format!("{RSS}title")).len(),
        1
    );
    assert_eq!(
        objects(&union, victim_item, &format!("{DCTERMS}isPartOf")),
        vec![Term::from(iri(VICTIM))]
    );
    // The id is kept, as a property of each item, not as its name.
    for (g, item) in [(&v, &v_items[0]), (&h, &h_items[0])] {
        assert_eq!(
            objects(g, item, &format!("{DC}identifier")),
            vec![Term::from(oxrdf::Literal::new_simple_literal(SHARED_ID))]
        );
    }
    // The river shows each headline once, under its own feed and link.
    let rows = river(&[(&victim, VICTIM), (&hostile, HOSTILE)]);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    let victim_row = rows
        .iter()
        .find(|r| r.contains("Real headline"))
        .expect("the victim's headline");
    assert!(
        victim_row.contains("https://victim.example.org/posts/1") && victim_row.contains(VICTIM),
        "{victim_row}"
    );
    assert!(!victim_row.contains("hostile"), "{victim_row}");
}

/// The same holds for an RSS 2.0 permalink guid and for a link: neither names an item outside
/// its feed.
#[test]
fn a_reused_guid_or_link_names_an_item_of_the_reusing_feed() {
    let rss = |title: &str, guid: &str| {
        format!(
            r#"<rss version="2.0"><channel><title>{title}</title><item><title>{title} item</title><link>https://victim.example.org/posts/1</link><guid isPermaLink="true">{guid}</guid></item><item><title>{title} linked</title><link>https://victim.example.org/posts/2</link></item></channel></rss>"#
        )
    };
    let v = graph(&rss("Victim", "https://victim.example.org/posts/1"), VICTIM);
    let h = graph(
        &rss("Hostile", "https://victim.example.org/posts/1"),
        HOSTILE,
    );
    let (v_items, h_items) = (items(&v), items(&h));
    assert_eq!((v_items.len(), h_items.len()), (2, 2));
    assert!(v_items.iter().all(|i| !h_items.contains(i)));
}

#[test]
fn entries_repeating_one_id_are_one_item_the_latest_updated() {
    // Thirty entries share one id, each with its own title, link and date; the latest is in the
    // middle, so neither the first nor the last entry wins by position.
    let entries: Vec<String> = (0..30)
        .map(|i| {
            let day = if i == 17 { 28 } else { 1 + (i % 20) };
            entry(
                SHARED_ID,
                &format!("Version {i}"),
                &format!("https://victim.example.org/posts/1?v={i}"),
                &format!("2026-10-{day:02}T00:00:00Z"),
            )
        })
        .collect();
    let feed = atom("Repeats", &entries);
    let g = graph(&feed, VICTIM);
    let only = items(&g);
    assert_eq!(only.len(), 1, "{only:?}");
    assert_eq!(
        objects(&g, &only[0], &format!("{RSS}title")),
        vec![Term::from(oxrdf::Literal::new_simple_literal("Version 17"))]
    );
    assert_eq!(objects(&g, &only[0], &format!("{RSS}link")).len(), 1);
    assert_eq!(objects(&g, &only[0], &format!("{DC}date")).len(), 1);
    // One row in the river, not thousands, and the river is answered, not refused.
    let rows = river(&[(&feed, VICTIM)]);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(rows[0].contains("Version 17"), "{rows:#?}");
    // The collapse is deterministic: the same bytes lift to the same Turtle.
    assert_eq!(turtle(&feed, VICTIM), turtle(&feed, VICTIM));
}

#[test]
fn on_equal_dates_the_first_entry_in_document_order_wins() {
    let entries: Vec<String> = ["First", "Second", "Third"]
        .iter()
        .map(|t| {
            entry(
                SHARED_ID,
                t,
                &format!("https://victim.example.org/{t}"),
                "2026-10-01T00:00:00Z",
            )
        })
        .collect();
    let g = graph(&atom("Ties", &entries), VICTIM);
    let only = items(&g);
    assert_eq!(only.len(), 1);
    assert_eq!(
        objects(&g, &only[0], &format!("{RSS}title")),
        vec![Term::from(oxrdf::Literal::new_simple_literal("First"))]
    );
}

#[test]
fn rss_items_repeating_a_guid_are_one_item() {
    let items_xml: String = (0..10)
        .map(|i| {
            format!(
                r#"<item><title>Copy {i}</title><link>https://victim.example.org/posts/{i}</link><guid>https://victim.example.org/posts/1</guid><pubDate>Wed, 0{} Oct 2026 10:00:00 GMT</pubDate></item>"#,
                1 + i % 9
            )
        })
        .collect();
    let feed =
        format!(r#"<rss version="2.0"><channel><title>Repeats</title>{items_xml}</channel></rss>"#);
    let g = graph(&feed, VICTIM);
    let only = items(&g);
    assert_eq!(only.len(), 1, "{only:?}");
    // 1 + i % 9 is 9 for i = 8 only: Copy 8 is the latest.
    assert_eq!(
        objects(&g, &only[0], &format!("{RSS}title")),
        vec![Term::from(oxrdf::Literal::new_simple_literal("Copy 8"))]
    );
}
