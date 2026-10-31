//! Per-source mappings as data (ledger #2673): `urn:iki:feed:mapping:{source}` is a file under
//! the host's config-home root, served by ikigai-fs `cacheable_space`, and
//! `urn:iki:feed:mapped:{feed}` is the normalized graph ∪ what that file's CONSTRUCT adds.
//!
//! The host here is the test: a temporary root mounted as a host mounts its config home, a
//! counting `urn:http:get` standing in for the network, and two synthetic hosts, `a.example`
//! and `b.example`, whose feeds carry no real site's content.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Iri, Kernel, ReprType,
    Representation, Request, Space, Verb,
};
use oxrdf::Triple;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const A: &str = "http://a.example/feed.xml";
const B: &str = "http://b.example/feed.xml";

fn feed(host: &str) -> String {
    format!(
        r#"<rss version="2.0"><channel><title>{host}</title><link>http://{host}/</link><description>d</description>
<item><title>Quoting Somebody on something</title><link>http://{host}/1</link><pubDate>Wed, 07 Oct 2026 10:00:00 GMT</pubDate></item>
<item><title>A note</title><link>http://{host}/2</link><pubDate>Thu, 08 Oct 2026 10:00:00 GMT</pubDate></item>
</channel></rss>"#
    )
}

/// A mapping that names the kind of a `Quoting …` post: the synthetic fixture the item asks for.
const QUOTES: &str = r#"PREFIX rss: <http://purl.org/rss/1.0/>
PREFIX dcterms: <http://purl.org/dc/terms/>
CONSTRUCT { ?item dcterms:type <urn:example:kind:quotation> }
WHERE { ?item a rss:item ; rss:title ?title . FILTER(STRSTARTS(?title, "Quoting ")) }
"#;

/// Another, so an edit is visible: every item becomes a note.
const NOTES: &str = r#"PREFIX rss: <http://purl.org/rss/1.0/>
PREFIX dcterms: <http://purl.org/dc/terms/>
CONSTRUCT { ?item dcterms:type <urn:example:kind:note> }
WHERE { ?item a rss:item }
"#;

const DCTERMS_TYPE: &str = "http://purl.org/dc/terms/type";

struct Host {
    kernel: Kernel,
    root: PathBuf,
    fetches: Arc<AtomicUsize>,
}

impl Drop for Host {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn temp_root(label: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "ikigai-feed-mapped-{}-{label}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(root.join("feed/mappings")).unwrap();
    root
}

/// A host: its config-home root under `cacheable_space`, mounted once at `urn:file:` and once
/// through [`ikigai_feed::mappings`], beside the feed, SPARQL and RDF modules.
fn host(label: &str) -> Host {
    let root = temp_root(label);
    let files: Arc<dyn Space> = Arc::new(ikigai_fs::cacheable_space(&root));
    let fetches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    let http = EndpointSpace::new().bind(
        Exact::new("urn:http:get"),
        FnEndpoint::new("fixture-get", move |inv| {
            counted.fetch_add(1, Ordering::SeqCst);
            let url = inv.inline_str("url")?;
            let host = ikigai_feed::source_of(url).unwrap();
            if host == "a.example" || host == "b.example" {
                Ok(Representation::new(
                    ReprType::new("application/rss+xml"),
                    feed(&host).into_bytes(),
                )
                .cacheable())
            } else {
                Err(Error::NotFound(url.to_string()))
            }
        }),
    );
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_feed::mapped_space()),
        Arc::new(ikigai_feed::mappings(Arc::clone(&files))),
        files,
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_rdf::space()),
        Arc::new(http),
    ])));
    Host {
        kernel,
        root,
        fetches,
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter()
        .fold(Request::new(verb, Iri::parse(iri).unwrap()), |r, (k, v)| {
            r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()))
        })
}

fn mapped(feed: &str) -> Request {
    request(Verb::Source, &format!("urn:iki:feed:mapped:{feed}"), &[])
}

fn normalized(feed: &str) -> Request {
    request(Verb::Source, &format!("urn:iki:feed:graph:{feed}"), &[])
}

impl Host {
    fn issue(&self, request: Request) -> ikigai_core::Result<Representation> {
        block_on(self.kernel.issue(request, &Capability::root()))
    }

    fn graph(&self, request: Request) -> HashSet<Triple> {
        parse(&self.issue(request).unwrap().bytes)
    }

    fn cached(&self, request: &Request) -> bool {
        self.kernel.is_cached(request, &Capability::root())
    }

    fn write(&self, name: &str, text: &str) {
        self.issue(request(Verb::Sink, name, &[("content", text)]))
            .unwrap();
    }
}

fn parse(bytes: &[u8]) -> HashSet<Triple> {
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(bytes)
        .map(|q| q.unwrap().into())
        .collect()
}

/// The `dcterms:type` triples of a graph, as `(item, kind)`.
fn kinds(g: &HashSet<Triple>) -> Vec<(String, String)> {
    let mut found: Vec<_> = g
        .iter()
        .filter(|t| t.predicate.as_str() == DCTERMS_TYPE)
        .map(|t| (t.subject.to_string(), t.object.to_string()))
        .collect();
    found.sort();
    found
}

#[test]
fn a_feed_with_no_mapping_reads_as_its_normalized_graph_unchanged() {
    let host = host("unmapped");
    let form = host.graph(normalized(A));
    assert!(!form.is_empty());
    assert_eq!(host.graph(mapped(A)), form);
    // And that graph is `urn:iki:feed:graph` of the fetched bytes, based at the feed's IRI.
    let direct = host.graph(
        request(Verb::Source, "urn:iki:feed:graph", &[("base", A)])
            .with_arg("content", ArgRef::Inline(feed("a.example").into_bytes())),
    );
    assert_eq!(form, direct);
    // The fallback is cacheable: it hangs from the fetch and the missing file, nothing else.
    assert!(host.cached(&mapped(A)));
}

#[test]
fn creating_a_mapping_through_a_sink_maps_the_next_read_with_no_restart() {
    let host = host("create");
    assert!(kinds(&host.graph(mapped(A))).is_empty());
    assert!(host.cached(&mapped(A)));
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    // The missing file's NotFound was the fallback's dependency; the write cut it.
    assert!(
        !host.cached(&mapped(A)),
        "creating the file re-maps its host"
    );
    // A `Quoting …` title gains its kind; the other item does not.
    assert_eq!(
        kinds(&host.graph(mapped(A))),
        [(
            "<http://a.example/1>".to_string(),
            "<urn:example:kind:quotation>".to_string()
        )]
    );
}

#[test]
fn editing_one_hosts_mapping_recomputes_that_host_and_leaves_the_other_cached() {
    let host = host("edit");
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    host.write("urn:file:feed/mappings/b.example.rq", QUOTES);
    let b_before = host.graph(mapped(B));
    assert_eq!(kinds(&host.graph(mapped(A))).len(), 1);
    assert!(host.cached(&mapped(A)) && host.cached(&mapped(B)));
    let fetches = host.fetches.load(Ordering::SeqCst);

    host.write("urn:file:feed/mappings/a.example.rq", NOTES);
    assert!(!host.cached(&mapped(A)), "A's edit cut A's mapped feed");
    assert!(host.cached(&mapped(B)), "and only A's");
    assert!(
        host.cached(&normalized(A)),
        "the normalized graph never read the file"
    );
    assert_eq!(
        kinds(&host.graph(mapped(A)))
            .into_iter()
            .map(|(_, kind)| kind)
            .collect::<Vec<_>>(),
        ["<urn:example:kind:note>", "<urn:example:kind:note>"]
    );
    assert_eq!(host.graph(mapped(B)), b_before);
    // Re-mapping A refetched nothing: the feed's read is its own entry, on the fetch's thread.
    assert_eq!(host.fetches.load(Ordering::SeqCst), fetches);
}

#[test]
fn the_diff_of_mapped_against_normalized_is_exactly_the_mappings_additions() {
    // The lint a mapping author runs: what does my file add to this feed?
    let host = host("diff");
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    let added = host.issue(
        request(
            Verb::Source,
            "urn:rdf:diff",
            &[("with", &format!("urn:iki:feed:graph:{A}"))],
        )
        .with_arg(
            "content",
            ArgRef::Reference(Iri::parse(format!("urn:iki:feed:mapped:{A}")).unwrap()),
        ),
    );
    let added = parse(&added.unwrap().bytes);
    let constructed = host.graph(
        request(
            Verb::Source,
            "urn:sparql:construct",
            &[("graph", &format!("urn:iki:feed:graph:{A}"))],
        )
        .with_arg(
            "query",
            ArgRef::Reference(Iri::parse("urn:iki:feed:mapping:a.example").unwrap()),
        ),
    );
    assert_eq!(added.len(), 1);
    assert_eq!(added, constructed);
    let removed = host.issue(
        request(
            Verb::Source,
            "urn:rdf:diff",
            &[
                ("with", &format!("urn:iki:feed:graph:{A}")),
                ("mode", "removed"),
            ],
        )
        .with_arg(
            "content",
            ArgRef::Reference(Iri::parse(format!("urn:iki:feed:mapped:{A}")).unwrap()),
        ),
    );
    assert!(
        parse(&removed.unwrap().bytes).is_empty(),
        "a mapping only adds"
    );
}

#[test]
fn a_mapping_is_read_and_written_through_one_name() {
    // The alias reports its rewrite, so the kernel keys the cache and the auto-cut on the
    // `urn:file:` name whichever spelling a request used.
    let host = host("one-name");
    let alias = || request(Verb::Source, "urn:iki:feed:mapping:a.example", &[]);
    let file = || request(Verb::Source, "urn:file:feed/mappings/a.example.rq", &[]);

    // Written through the file name, read through the alias: the alias's NotFound is cut.
    assert!(matches!(host.issue(alias()), Err(Error::NotFound(_))));
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    assert_eq!(host.issue(alias()).unwrap().bytes, QUOTES.as_bytes());
    assert!(
        host.cached(&alias()) && host.cached(&file()),
        "one entry, both names"
    );

    // Written through the alias, read through either: both see the edit.
    host.write("urn:iki:feed:mapping:a.example", NOTES);
    assert!(!host.cached(&alias()) && !host.cached(&file()));
    assert_eq!(host.issue(file()).unwrap().bytes, NOTES.as_bytes());
    assert_eq!(host.issue(alias()).unwrap().bytes, NOTES.as_bytes());
    assert_eq!(
        std::fs::read_to_string(host.root.join("feed/mappings/a.example.rq")).unwrap(),
        NOTES
    );
    // And a mapped feed read before the alias write is re-mapped by it.
    let _ = host.graph(mapped(A));
    host.write("urn:iki:feed:mapping:a.example", QUOTES);
    assert!(!host.cached(&mapped(A)));
}

#[test]
fn a_broken_mapping_fails_the_mapped_feed_rather_than_falling_back() {
    // Only the mapping file's OWN NotFound falls back. A mapping that is there and wrong is
    // the author's error, and a missing feed is the composite's: both surface.
    let host = host("broken");
    host.write("urn:file:feed/mappings/a.example.rq", "CONSTRUCT {");
    assert!(host.issue(mapped(A)).is_err());
    host.write("urn:file:feed/mappings/c.example.rq", QUOTES);
    assert!(matches!(
        host.issue(mapped("http://c.example/feed.xml")),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn nothing_but_the_hosts_file_root_answers_the_mapping_family() {
    let host = host("sealed");
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    // Only a lowercased host is a source; nothing else under the family reaches a file.
    for name in [
        "urn:iki:feed:mapping:A.example",
        "urn:iki:feed:mapping:a.example/../b",
        "urn:iki:feed:mapping:",
    ] {
        assert!(
            matches!(
                host.issue(request(Verb::Source, name, &[])),
                Err(Error::Unresolved(_))
            ),
            "{name}"
        );
    }
    // The overlay answers the family alone: it does not open the rest of the file root.
    let only = Kernel::new(Arc::new(ikigai_feed::mappings(Arc::new(
        ikigai_fs::cacheable_space(&host.root),
    ))));
    let file = request(Verb::Source, "urn:file:feed/mappings/a.example.rq", &[]);
    assert!(matches!(
        block_on(only.issue(file, &Capability::root())),
        Err(Error::Unresolved(_))
    ));
    let alias = request(Verb::Source, "urn:iki:feed:mapping:a.example", &[]);
    assert!(block_on(only.issue(alias, &Capability::root())).is_ok());
    // And the feed module binds none of it: without the host's root the family is unresolved.
    let bare = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_feed::mapped_space()),
    ])));
    let alias = request(Verb::Source, "urn:iki:feed:mapping:a.example", &[]);
    assert!(matches!(
        block_on(bare.issue(alias, &Capability::root())),
        Err(Error::Unresolved(_))
    ));
}

#[test]
fn a_mapping_the_caller_may_not_read_is_refused_not_skipped() {
    // A denial is not an absence: a caller kept out of the mappings does not get the
    // unmapped graph passed off as the mapped one.
    let host = host("denied");
    host.write("urn:file:feed/mappings/a.example.rq", QUOTES);
    let root = host.root.to_str().unwrap();
    let kept_out = Capability::scoped([
        format!("urn:cap:fs:read:{root}"),
        format!("urn:cap:fs:read:-{root}/feed/mappings"),
    ]);
    assert!(matches!(
        block_on(host.kernel.issue(mapped(A), &kept_out)),
        Err(Error::Denied(_))
    ));
}
