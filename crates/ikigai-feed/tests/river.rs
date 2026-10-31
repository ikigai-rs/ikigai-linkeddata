//! The Book's river query (ikigai-tutorial `examples/rss/ws/queries/river.rq`, verbatim) over
//! the form lifted from RSS 2.0, RSS 1.0 and Atom at once: the queries written against the
//! XSLT's RSS 1.0 shape keep working on it, whichever syntax a feed came in.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::sync::Arc;

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

fn request(iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.to_vec())),
    )
}

#[test]
fn the_books_river_query_reads_every_syntax() {
    let feeds = Kernel::new(Arc::new(ikigai_feed::space()));
    let mut graphs = EndpointSpace::new();
    for name in ["rss2", "rss1", "atom"] {
        let bytes = std::fs::read(format!(
            "{}/tests/fixtures/{name}.xml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let turtle = block_on(feeds.issue(
            request("urn:iki:feed:graph", &[("content", &bytes)]),
            &Capability::root(),
        ))
        .unwrap()
        .bytes;
        graphs = graphs.bind(
            Exact::new(format!("urn:test:{name}")),
            FnEndpoint::new("fixture", move |_| {
                Ok(Representation::new(ReprType::new("text/turtle"), turtle.clone()).cacheable())
            }),
        );
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
                ("graph", b"urn:test:rss2,urn:test:rss1,urn:test:atom"),
                ("as", b"text/csv"),
            ],
        ),
        &Capability::root(),
    ))
    .unwrap();
    let csv = String::from_utf8(csv.bytes).unwrap();
    let rows: Vec<&str> = csv.lines().skip(1).collect();
    // Every item with a link, from all three syntaxes: six of RSS 2.0's seven (one has no
    // link, which the river requires), both of RSS 1.0's, both of Atom's.
    assert_eq!(rows.len(), 10, "{csv}");
    // Items are named within their feed (ledger #2707), so a row is found by its link.
    for expected in [
        "http://feeds.example.org/posts/a.html",
        "http://rdf.example.org/items/1.html",
        "http://atom.example.org/blog/posts/1",
    ] {
        assert!(
            rows.iter().any(|r| row_link(r) == expected),
            "{expected}: {csv}"
        );
    }
    // Newest first across syntaxes, on dc:date: RSS 2.0's 07:30Z on the 8th (written +0200)
    // before Atom's midnight on the 8th, before the three items of the 7th.
    assert_eq!(
        row_link(rows[0]),
        "http://feeds.example.org/posts/b",
        "{csv}"
    );
    assert_eq!(
        row_link(rows[1]),
        "http://atom.example.org/blog/posts/2",
        "{csv}"
    );
    // Every item is named by digest within its feed, none by an IRI a feed wrote.
    assert!(rows.iter().all(|r| r.starts_with("nih:sha-256;")), "{csv}");
}

/// A row's `?link`: the first `http` field after the item (no fixture title holds one).
fn row_link(row: &str) -> &str {
    row.split(',')
        .skip(1)
        .find(|f| f.starts_with("http"))
        .unwrap_or("")
}
