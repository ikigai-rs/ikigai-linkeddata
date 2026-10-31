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

/// A kernel serving the three fixtures' lifted form as `urn:test:{rss2,rss1,atom}`, beside the
/// SPARQL doors.
fn kernel() -> Kernel {
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
        // Atom's again under a second name, for a merge of two sources holding the same triples.
        let names = if name == "atom" {
            &["atom", "atom-again"][..]
        } else {
            &[name][..]
        };
        for name in names {
            let turtle = turtle.clone();
            graphs = graphs.bind(
                Exact::new(format!("urn:test:{name}")),
                FnEndpoint::new("fixture", move |_| {
                    Ok(
                        Representation::new(ReprType::new("text/turtle"), turtle.clone())
                            .cacheable(),
                    )
                }),
            );
        }
    }
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(graphs),
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_rdf::space()),
    ])))
}

/// `query` over the sources `graph` lists, as CSV.
fn select(query: &str, graph: &str) -> String {
    let csv = block_on(kernel().issue(
        request(
            "urn:sparql:select",
            &[
                ("query", query.as_bytes()),
                ("graph", graph.as_bytes()),
                ("as", b"text/csv"),
            ],
        ),
        &Capability::root(),
    ))
    .unwrap();
    String::from_utf8(csv.bytes).unwrap()
}

#[test]
fn the_books_river_query_reads_every_syntax() {
    let csv = select(RIVER, "urn:test:rss2,urn:test:rss1,urn:test:atom");
    let rows: Vec<&str> = csv.lines().skip(1).collect();
    // Every item with a link, from all three syntaxes: six of RSS 2.0's seven (one has no
    // link, which the river requires), both of RSS 1.0's, both of Atom's.
    assert_eq!(rows.len(), 10, "{csv}");
    for expected in [
        "http://feeds.example.org/posts/a,",
        "http://rdf.example.org/items/1,",
        "tag:atom.example.org,2026:entry-1,",
    ] {
        assert!(
            rows.iter().any(|r| r.starts_with(expected)),
            "{expected}: {csv}"
        );
    }
    // Newest first across syntaxes, on dc:date: RSS 2.0's 07:30Z on the 8th (written +0200)
    // before Atom's midnight on the 8th, before the three items of the 7th.
    assert!(
        rows[0].starts_with("http://feeds.example.org/posts/b,"),
        "{csv}"
    );
    assert!(
        rows[1].starts_with("http://atom.example.org/blog/posts/2,"),
        "{csv}"
    );
}

/// A feed listed twice is the same feed (ledger #2715): the river and a COUNT over the merge are
/// what they are with each feed listed once, not a row per listing.
#[test]
fn a_feed_listed_twice_changes_neither_the_river_nor_a_count() {
    let once = "urn:test:rss2,urn:test:rss1,urn:test:atom";
    let twice = "urn:test:rss2,urn:test:rss1,urn:test:atom,urn:test:atom urn:test:rss2";
    assert_eq!(select(RIVER, twice), select(RIVER, once));
    let count = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }";
    assert_eq!(select(count, twice), select(count, once));
    let items = "PREFIX rss: <http://purl.org/rss/1.0/> \
                 SELECT (COUNT(*) AS ?n) WHERE { ?item a rss:item }";
    assert_eq!(select(items, twice), select(items, once));
    // The same feed under two names is two sources holding the same triples: their merge holds
    // each once, so the river is the one feed's.
    assert_eq!(
        select(RIVER, "urn:test:atom,urn:test:atom-again"),
        select(RIVER, "urn:test:atom")
    );
}
