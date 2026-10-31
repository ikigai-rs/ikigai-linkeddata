//! The lift, seen through SPARQL as a feed mapping would (ledger #2674): a `blockquote/@cite`
//! and a `p.cite > a` are selectable; `graph`+`predicate` roots each item's tree at the item;
//! the skolem IRIs are the same on every run; the node bound refuses.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use ikigai_html::{Lift, MAX_HTML_NODES};
use oxrdf::NamedNode;
use std::sync::Arc;

const PREFIXES: &str = r#"PREFIX htm: <https://ikigai-rs.dev/ns/html#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX schema: <https://schema.org/>
PREFIX content: <http://purl.org/rss/1.0/modules/content/>
"#;

/// Every `blockquote/@cite` and every `p.cite > a/@href`, with the root each sits under.
const CITES: &str = r#"
SELECT ?root ?kind ?iri WHERE {
  {
    ?root dcterms:hasPart+ ?q .
    ?q htm:localName "blockquote" ; htm:attribute ?a .
    ?a htm:localName "cite" ; rdf:value ?iri .
    BIND("quote" AS ?kind)
  } UNION {
    ?root dcterms:hasPart+ ?p .
    ?p htm:localName "p" ; htm:attribute ?c ; dcterms:hasPart ?link .
    ?c htm:localName "class" ; rdf:value ?class .
    FILTER(CONTAINS(CONCAT(" ", ?class, " "), " cite "))
    ?link htm:localName "a" ; htm:attribute ?h .
    ?h htm:localName "href" ; rdf:value ?iri .
    BIND("by" AS ?kind)
  }
  ?root a ?rootKind . FILTER(?rootKind IN (htm:Document, htm:Fragment))
}
ORDER BY ?root ?kind
"#;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn request(iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.to_vec())),
    )
}

fn graph(turtle: Vec<u8>) -> FnEndpoint {
    FnEndpoint::new("fixture", move |_| {
        Ok(Representation::new(ReprType::new("text/turtle"), turtle.clone()).cacheable())
    })
}

/// A kernel with the lift, SPARQL, RDF transreption and the named Turtle graphs.
fn kernel(graphs: Vec<(&str, Vec<u8>)>) -> Kernel {
    let space = graphs
        .into_iter()
        .fold(EndpointSpace::new(), |s, (iri, turtle)| {
            s.bind(Exact::new(iri), graph(turtle))
        });
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space),
        Arc::new(ikigai_html::space()),
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_rdf::space()),
    ])))
}

fn issue(kernel: &Kernel, request: Request) -> ikigai_core::Result<Vec<u8>> {
    block_on(kernel.issue(request, &Capability::root())).map(|r| r.bytes)
}

fn select(kernel: &Kernel, query: &str, graphs: &str) -> Vec<String> {
    let query = format!("{PREFIXES}{query}");
    let csv = issue(
        kernel,
        request(
            "urn:sparql:select",
            &[
                ("query", query.as_bytes()),
                ("graph", graphs.as_bytes()),
                ("as", b"text/csv"),
            ],
        ),
    )
    .unwrap();
    String::from_utf8(csv)
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn lift_page() -> Vec<u8> {
    issue(
        &kernel(vec![]),
        request("urn:iki:html:graph", &[("content", &fixture("item.html"))]),
    )
    .unwrap()
}

#[test]
fn a_blockquote_cite_and_a_cite_paragraphs_link_are_selectable() {
    let k = kernel(vec![("urn:test:page", lift_page())]);
    let rows = select(&k, CITES, "urn:test:page");
    let found: Vec<_> = rows
        .iter()
        .map(|r| r.split_once(',').unwrap().1.to_string())
        .collect();
    assert_eq!(
        found,
        [
            "by,https://example.org/author",
            "quote,https://example.org/source"
        ],
        "{rows:?}"
    );
}

#[test]
fn children_are_ordered_text_is_kept_and_comments_dropped() {
    let k = kernel(vec![("urn:test:page", lift_page())]);
    // The cite paragraph's children, in order: a text, the link, a text, the em.
    let rows = select(
        &k,
        r#"SELECT ?pos ?what WHERE {
             ?p htm:localName "p" ; htm:attribute [ htm:localName "class" ] ; dcterms:hasPart ?c .
             ?c schema:position ?pos .
             OPTIONAL { ?c htm:localName ?name }
             OPTIONAL { ?c a htm:Text ; rdf:value ?text }
             BIND(COALESCE(?name, ?text) AS ?what)
           } ORDER BY ?pos"#,
        "urn:test:page",
    );
    assert_eq!(rows, ["1,— ", "2,a", "3,\", \"", "4,em"], "{rows:?}");
    // The document's one child is html; html's are head, a whitespace text, then body.
    let rows = select(
        &k,
        r#"SELECT ?pos ?name WHERE {
             ?d a htm:Document ; dcterms:hasPart ?html .
             ?html htm:localName "html" ; dcterms:hasPart ?c . ?c schema:position ?pos .
             OPTIONAL { ?c htm:localName ?n } BIND(COALESCE(?n, "(text)") AS ?name)
           } ORDER BY ?pos"#,
        "urn:test:page",
    );
    assert_eq!(rows, ["1,head", "2,(text)", "3,body"], "{rows:?}");
    let turtle = String::from_utf8(lift_page()).unwrap();
    assert!(!turtle.contains("a comment"), "comments are not lifted");
}

#[test]
fn foreign_elements_and_attributes_carry_their_namespace() {
    let k = kernel(vec![("urn:test:page", lift_page())]);
    let rows = select(
        &k,
        r#"SELECT ?ens ?ans ?href WHERE {
             ?e htm:localName "a" ; htm:namespace ?ens ; htm:attribute ?h .
             ?h htm:localName "href" ; htm:namespace ?ans ; rdf:value ?href .
           }"#,
        "urn:test:page",
    );
    assert_eq!(
        rows,
        ["http://www.w3.org/2000/svg,http://www.w3.org/1999/xlink,https://example.org/figure"]
    );
}

fn lift_feed(k: &Kernel, predicate: &str) -> Vec<u8> {
    issue(
        k,
        request(
            "urn:iki:html:graph",
            &[
                ("graph", b"urn:test:feed"),
                ("predicate", predicate.as_bytes()),
            ],
        ),
    )
    .unwrap()
}

const ENCODED: &str = "http://purl.org/rss/1.0/modules/content/encoded";

#[test]
fn graph_and_predicate_root_each_items_tree_at_the_item() {
    let feed = fixture("feed.ttl");
    let html = lift_feed(&kernel(vec![("urn:test:feed", feed.clone())]), ENCODED);
    let k = kernel(vec![("urn:test:feed", feed), ("urn:test:html", html)]);
    let rows = select(
        &k,
        &format!(
            r#"SELECT ?item ?kind ?iri WHERE {{
                 ?item a <http://purl.org/rss/1.0/item> ; htm:tree ?root .
                 ?root htm:property content:encoded .
                 {{ {} }}
               }} ORDER BY ?item ?kind"#,
            // The CITES pattern, bound to the item's root.
            CITES
                .split_once("WHERE {")
                .unwrap()
                .1
                .rsplit_once('}')
                .unwrap()
                .0
        ),
        "urn:test:feed,urn:test:html",
    );
    assert_eq!(
        rows,
        [
            "https://example.org/posts/1,by,https://example.org/by/1",
            "https://example.org/posts/1,quote,https://example.org/quoted/1",
            "https://example.org/posts/2,by,https://example.org/by/2",
            "https://example.org/posts/2,quote,https://example.org/quoted/2",
        ],
        "{rows:?}"
    );
    // A fragment is its own top-level nodes: no html or body wrapper between item and quote.
    let rows = select(
        &k,
        r#"SELECT ?item ?first WHERE {
             ?item htm:tree ?root . ?root a htm:Fragment ; dcterms:hasPart ?c .
             ?c schema:position 1 ; htm:localName ?first .
           } ORDER BY ?item"#,
        "urn:test:html",
    );
    assert_eq!(
        rows,
        [
            "https://example.org/posts/1,blockquote",
            "https://example.org/posts/2,blockquote"
        ]
    );
    // Only the named predicate is lifted: the description's <b> is not there.
    let rows = select(
        &k,
        r#"SELECT ?e WHERE { ?e htm:localName "b" }"#,
        "urn:test:html",
    );
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
fn trees_of_two_predicates_on_one_item_stay_apart() {
    let feed = fixture("feed.ttl");
    let k = kernel(vec![("urn:test:feed", feed.clone())]);
    let encoded = lift_feed(&k, ENCODED);
    let description = lift_feed(&k, "http://purl.org/rss/1.0/description");
    let k = kernel(vec![
        ("urn:test:encoded", encoded),
        ("urn:test:description", description),
    ]);
    let rows = select(
        &k,
        r#"SELECT ?property (COUNT(?root) AS ?n) WHERE {
             <https://example.org/posts/1> htm:tree ?root . ?root htm:property ?property .
           } GROUP BY ?property ORDER BY ?property"#,
        "urn:test:encoded,urn:test:description",
    );
    assert_eq!(
        rows,
        [
            "http://purl.org/rss/1.0/description,1",
            "http://purl.org/rss/1.0/modules/content/encoded,1"
        ]
    );
}

#[test]
fn skolem_iris_are_stable_and_there_are_no_blank_nodes() {
    let once = lift_page();
    let again = lift_page();
    assert_eq!(once, again, "the same document lifts to the same bytes");
    let turtle = String::from_utf8(once).unwrap();
    assert!(!turtle.contains("_:"), "no blank nodes:\n{turtle}");
    assert!(!turtle.contains('['), "no anonymous nodes:\n{turtle}");

    // Pinned, so a change to how nodes are named (which breaks every stored lift) is seen:
    // the fragment root is a digest of subject, predicate and HTML, and every node below it a
    // digest of the root and its path.
    let mut lift = Lift::new();
    let root = lift
        .fragment(
            &NamedNode::new("https://example.org/posts/1").unwrap(),
            &NamedNode::new(ENCODED).unwrap(),
            "<p>x</p>",
        )
        .unwrap();
    assert_eq!(
        root.as_str(),
        "nih:sha-256;d6b0b9c539cf9066704b5e47f63b81cced59f7425a2852cae81523c186637bf9"
    );
    let triples = lift.into_triples();
    let subjects: Vec<_> = triples.iter().map(|t| t.subject.to_string()).collect();
    assert!(
        subjects
            .iter()
            .all(|s| s.starts_with("<nih:sha-256;") || s == "<https://example.org/posts/1>"),
        "{subjects:?}"
    );
}

#[test]
fn the_node_bound_refuses_and_admits_at_it() {
    let item = NamedNode::new("https://example.org/i").unwrap();
    let predicate = NamedNode::new(ENCODED).unwrap();
    // A fragment of n empty elements mints n + 1 nodes (the root and each element).
    let n = |count: usize| "<i></i>".repeat(count);
    let at = Lift::new().fragment(&item, &predicate, &n(MAX_HTML_NODES - 1));
    assert!(at.is_ok(), "{at:?}");
    let past = Lift::new().fragment(&item, &predicate, &n(MAX_HTML_NODES));
    assert!(
        matches!(&past, Err(Error::OverBudget { detail }) if detail.contains("100000")),
        "{past:?}"
    );
    // Attributes and texts count, and the bound is the whole request's, across literals.
    let mut lift = Lift::with_bounds(16, 6);
    lift.fragment(&item, &predicate, "<a href=x>t</a>").unwrap(); // root, a, href, text
    let second = lift.fragment(&item, &ikigai_iri("urn:p:2"), "<b>u</b>"); // 3 more: 7 > 6
    assert!(
        matches!(second, Err(Error::OverBudget { .. })),
        "{second:?}"
    );
}

#[test]
fn the_node_bound_refuses_through_the_door() {
    let k = kernel(vec![]);
    let html = format!("<p>{}</p>", "<i></i>".repeat(MAX_HTML_NODES));
    let refused = issue(
        &k,
        request("urn:iki:html:graph", &[("content", html.as_bytes())]),
    );
    assert!(
        matches!(refused, Err(Error::OverBudget { .. })),
        "{refused:?}"
    );
}

#[test]
fn a_shallow_fragment_past_a_small_depth_bound_refuses() {
    let item = NamedNode::new("https://example.org/i").unwrap();
    let ok = Lift::with_bounds(3, 100).fragment(&item, &item, "<div><p><b>x</b></p></div>");
    assert!(ok.is_ok(), "{ok:?}");
    let deep =
        Lift::with_bounds(3, 100).fragment(&item, &item, "<div><p><b><i>x</i></b></p></div>");
    assert!(
        matches!(&deep, Err(Error::OverBudget { detail }) if detail.contains("4 deep")),
        "{deep:?}"
    );
}

#[test]
fn arguments_are_checked() {
    let k = kernel(vec![("urn:test:feed", fixture("feed.ttl"))]);
    let both = issue(
        &k,
        request(
            "urn:iki:html:graph",
            &[("content", b"<p>"), ("graph", b"urn:test:feed")],
        ),
    );
    assert!(
        matches!(both, Err(Error::InvalidArgument { .. })),
        "{both:?}"
    );
    let no_predicate = issue(
        &k,
        request("urn:iki:html:graph", &[("graph", b"urn:test:feed")]),
    );
    assert!(no_predicate.is_err(), "{no_predicate:?}");
    let blank = kernel(vec![(
        "urn:test:blank",
        br#"[] <urn:p> "<p>x</p>" ."#.to_vec(),
    )]);
    let refused = issue(
        &blank,
        request(
            "urn:iki:html:graph",
            &[("graph", b"urn:test:blank"), ("predicate", b"urn:p")],
        ),
    );
    assert!(
        matches!(&refused, Err(Error::InvalidArgument { detail, .. }) if detail.contains("blank")),
        "{refused:?}"
    );
}

fn ikigai_iri(iri: &str) -> NamedNode {
    NamedNode::new(iri).unwrap()
}

/// The tree builder's harder paths go through the arena's moves (`append_before_sibling`,
/// `reparent_children`, `remove_from_parent`): each lifts, and to the tree a browser builds.
#[test]
fn misnested_html_lifts_to_the_browsers_tree() {
    let item = NamedNode::new("https://example.org/i").unwrap();
    let tree = |html: &str| {
        let mut lift = Lift::new();
        lift.fragment(&item, &item, html).unwrap();
        lift.into_triples().len()
    };
    // The adoption agency: <b>1<p>2</b>3</p> is <b>1</b><p><b>2</b>3</p>, the p moved out of
    // the b and a clone of the b made inside it.
    let k = {
        let mut lift = Lift::new();
        lift.fragment(&item, &item, "<b>1<p>2</b>3</p>").unwrap();
        let turtle = lift
            .into_triples()
            .iter()
            .map(|t| format!("{t} .\n"))
            .collect::<String>();
        kernel(vec![("urn:test:aaa", turtle.into_bytes())])
    };
    let rows = select(
        &k,
        r#"SELECT ?path WHERE {
             ?f a htm:Fragment ; dcterms:hasPart ?top . ?top schema:position ?i ; htm:localName ?n .
             OPTIONAL { ?top dcterms:hasPart ?c . ?c schema:position ?j .
                        OPTIONAL { ?c htm:localName ?cn } OPTIONAL { ?c rdf:value ?cv } }
             BIND(CONCAT(STR(?i), ":", ?n, "/", STR(?j), ":", COALESCE(?cn, ?cv)) AS ?path)
           } ORDER BY ?path"#,
        "urn:test:aaa",
    );
    assert_eq!(rows, ["1:b/1:1", "2:p/1:b", "2:p/2:3"], "{rows:?}");
    for html in [
        "<table><tr><td>a</td>stray<td>b</td></tr></table>",
        "<template><p>inside</p></template>",
        "<a href=1><div><a href=2>x</a></div></a>",
        "<p><i><b><s>a</p>b",
        "<select><option>a<option>b</select>",
        "<math><annotation-xml encoding=text/html><p>x</p></annotation-xml></math>",
        "</p></br><frameset>",
        "\u{0}<p>\u{0}</p>",
        "",
    ] {
        tree(html);
    }
}
