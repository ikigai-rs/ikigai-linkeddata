//! A source's own conventions — its kinds of post, its tags — declared as DATA (ledger #2673):
//! one SPARQL CONSTRUCT file per host, read through `urn:iki:feed:mapping:{source}`, and the
//! feed it maps read at `urn:iki:feed:mapped:{feed}`.
//!
//! - [`mappings`] answers `urn:iki:feed:mapping:{source}` by rewriting it to
//!   `urn:file:feed/mappings/{source}.rq` in the ONE file space the host hands it (its
//!   config-home root, mounted with ikigai-fs `cacheable_space`). The rewrite is reported as
//!   the canonical name, so both names share one cache entry and one golden thread: a `Sink`
//!   through either cuts a read through either. Nothing else answers the family (sealed), and
//!   this crate opens no file itself.
//! - [`mapped_space`] binds `urn:iki:feed:graph:{feed}` (the normalized form of the feed at
//!   `{feed}`, fetched and lifted) and `urn:iki:feed:mapped:{feed}` (that graph ∪ what the
//!   host's mapping CONSTRUCTs from it).
use super::{invalid, lift, serialize, MAX_FEED_BYTES};
use async_trait::async_trait;
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Bindings, Description, Endpoint, EndpointSpace, Error,
    FailureClass, Invocation, Iri, ReprType, Representation, Request, Resolution, Scope, Space,
    SpaceEntry, Topology, UriTemplate, Verb,
};
use std::sync::Arc;

/// The family a host's mapping files are read under.
pub const MAPPING_PREFIX: &str = "urn:iki:feed:mapping:";
/// Where `urn:iki:feed:mapping:{source}` lands in the host's file space.
pub const MAPPING_FILE: &str = "urn:file:feed/mappings/{source}.rq";
/// The normalized graph of the feed at `{feed}`.
pub const GRAPH_TEMPLATE: &str = "urn:iki:feed:graph:{feed}";
/// The normalized graph of the feed at `{feed}`, with its host's mapping applied.
pub const MAPPED_TEMPLATE: &str = "urn:iki:feed:mapped:{feed}";
/// The identity claimed by [`mapped_space`].
pub const MAPPED_SPACE_ID: &str = "urn:iki:space:feed:mapped";

const HTTP_GET: &str = "urn:http:get";
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

/// The mapping overlay: `urn:iki:feed:mapping:{source}` → `urn:file:feed/mappings/{source}.rq`
/// in `files`, the host's config-home file space (ikigai-fs `cacheable_space`, so a missing
/// file's `NotFound` and a present file's read both hang from the file's own thread).
///
/// Every verb is forwarded, so a mapping is read and written through the same name: a `Sink`
/// to the alias writes the file and cuts the canonical `urn:file:` thread, exactly as a `Sink`
/// to the `urn:file:` name does. Any other name is a miss here, and so is a `{source}` that is
/// not a lowercased host (`[a-z0-9.-]`, not starting with `.` or `-`), so the family reaches
/// no file outside `feed/mappings/` and only `files` ever answers it.
pub fn mappings(files: Arc<dyn Space>) -> Mappings {
    Mappings { files }
}

/// The space [`mappings`] builds. Anonymous: its doors are whatever file space it was handed.
pub struct Mappings {
    files: Arc<dyn Space>,
}

impl Space for Mappings {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let Some(file) = mapping_file(request.target.as_str()) else {
            return Resolution::Miss;
        };
        let mut rewritten = request.clone();
        rewritten.target = file.clone();
        match self.files.resolve(&rewritten, scope) {
            Resolution::Hit(hit) => Resolution::Hit(hit.with_canonical(file)),
            Resolution::Miss => Resolution::Miss,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // A renaming, so the catalog lists the backing `urn:file:` names, not this family.
        Some(Vec::new())
    }

    fn topology(&self) -> Topology {
        Topology::opaque(None).child(self.files.topology())
    }
}

/// The file `urn:iki:feed:mapping:{source}` names, if `name` is one with a well-formed source.
fn mapping_file(name: &str) -> Option<Iri> {
    let source = name.strip_prefix(MAPPING_PREFIX)?;
    is_source(source)
        .then(|| Iri::parse(MAPPING_FILE.replace("{source}", source)).ok())
        .flatten()
}

/// A source key: a lowercased host name.
fn is_source(source: &str) -> bool {
    !source.is_empty()
        && !source.starts_with(['.', '-'])
        && source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// The source key of a feed IRI: its host, lowercased (`http://Notes.Example.org:8080/feed`
/// → `notes.example.org`). `None` when the IRI has no host a source key can spell.
pub fn source_of(feed: &str) -> Option<String> {
    let (_, rest) = feed.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next()?.to_ascii_lowercase();
    is_source(&host).then_some(host)
}

/// `urn:iki:feed:graph:{feed}` and `urn:iki:feed:mapped:{feed}`.
///
/// `urn:iki:feed:mapped` is a small function here for now. Once core's reference templates
/// (ledger #2671) and the engine's names (ledger #2619) ship, it can become a named request:
/// `urn:rdf:union content=<urn:iki:feed:graph:{feed}> with=<urn:sparql:construct
/// graph=<urn:iki:feed:graph:{feed}> query=<urn:iki:feed:mapping:{source}>>`, falling back on
/// the mapping's own `NotFound`.
pub fn mapped_space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(template(GRAPH_TEMPLATE), FeedAt)
        .bind(template(MAPPED_TEMPLATE), MappedFeed)
        .named(space_iri("feed:mapped"))
}

fn template(t: &str) -> UriTemplate {
    UriTemplate::parse(t).expect("the feed templates are valid")
}

fn feed_iri(bindings: &Bindings) -> Result<Iri, Error> {
    let feed = bindings
        .get("feed")
        .ok_or_else(|| Error::MissingArgument("feed".into()))?;
    Iri::parse(feed).map_err(|e| invalid("feed", format!("not an IRI: {e}")))
}

fn feed_input() -> ArgSpec {
    ArgSpec::new("feed")
        .summary("the feed's own IRI (an http(s) feed is fetched through urn:http:get)")
        .class(XSD_ANY_URI)
        .binding()
}

fn turtle(bytes: Vec<u8>) -> Representation {
    Representation::new(
        ReprType::new("text/turtle").with_param("charset", "utf-8"),
        bytes,
    )
    .cacheable()
}

/// `urn:iki:feed:graph:{feed}`: the feed fetched and lifted, `{feed}` its base. Cacheable,
/// hanging on the fetch's thread.
struct FeedAt;

#[async_trait]
impl Endpoint for FeedAt {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let feed = feed_iri(inv.bindings)?;
        let fetched =
            if feed.as_str().starts_with("http://") || feed.as_str().starts_with("https://") {
                let get = Request::new(Verb::Source, Iri::parse(HTTP_GET).unwrap())
                    .with_arg("url", ArgRef::Inline(feed.as_str().as_bytes().to_vec()));
                inv.issue(get).await?
            } else {
                inv.source(&feed).await?
            };
        if fetched.bytes.len() > MAX_FEED_BYTES {
            return Err(Error::OverBudget {
                detail: format!("feed exceeds {MAX_FEED_BYTES} bytes"),
            });
        }
        Ok(turtle(serialize(lift(
            &fetched.bytes,
            Some(feed.as_str()),
        )?)?))
    }

    fn name(&self) -> &str {
        "feed-graph-at"
    }

    fn describe(&self) -> Description {
        Description::new("feed-graph-at")
            .title("Feed graph, by feed IRI")
            .summary("The feed at {feed}, fetched and lifted into the one graph form")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(feed_input())
            .output("text/turtle")
            .fails(FailureClass::OverBudget)
    }
}

/// `urn:iki:feed:mapped:{feed}`: `urn:iki:feed:graph:{feed}` ∪ the CONSTRUCT at
/// `urn:iki:feed:mapping:{source}` run over it; on the mapping's OWN `NotFound`, the
/// normalized graph alone.
struct MappedFeed;

#[async_trait]
impl Endpoint for MappedFeed {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let feed = feed_iri(inv.bindings)?;
        let source = source_of(feed.as_str())
            .ok_or_else(|| invalid("feed", "the feed IRI has no host to key a mapping by"))?;
        let normalized = Iri::parse(GRAPH_TEMPLATE.replace("{feed}", feed.as_str()))
            .map_err(|e| invalid("feed", e.to_string()))?;
        let mapping = Iri::parse(format!("{MAPPING_PREFIX}{source}")).unwrap();
        // ★ Fall back only on the ATOM's own NotFound (field guide, invalidation trap 1): the
        // mapping is read here, by itself, so the NotFound is the file's and the fallback hangs
        // from the file's thread, which creating `{source}.rq` cuts. A NotFound from the
        // CONSTRUCT or the union below is a composite's, and propagates.
        match inv.source(&mapping).await {
            Ok(_) => {}
            Err(Error::NotFound(_)) => return inv.source(&normalized).await,
            Err(e) => return Err(e),
        }
        // The query by reference: the kernel settles it from the (cached) mapping read.
        let construct = Request::new(Verb::Source, Iri::parse("urn:sparql:construct").unwrap())
            .with_arg(
                "graph",
                ArgRef::Inline(normalized.as_str().as_bytes().to_vec()),
            )
            .with_arg("query", ArgRef::Reference(mapping))
            .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec()));
        let added = inv.issue(construct).await?;
        let union = Request::new(Verb::Source, Iri::parse("urn:rdf:union").unwrap())
            .with_arg("content", ArgRef::Inline(added.bytes))
            .with_arg(
                "with",
                ArgRef::Inline(normalized.as_str().as_bytes().to_vec()),
            );
        Ok(turtle(inv.issue(union).await?.bytes))
    }

    fn name(&self) -> &str {
        "feed-mapped"
    }

    fn describe(&self) -> Description {
        Description::new("feed-mapped")
            .title("Mapped feed")
            .summary(
                "The feed at {feed} in the one graph form, plus what its host's mapping \
                 (urn:iki:feed:mapping:{source}, one SPARQL CONSTRUCT) adds; the form alone \
                 when the host has no mapping",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(feed_input())
            .output("text/turtle")
            // The mapping is a file: reading it takes the file module's read grant, which
            // its ACL then scopes to `feed/mappings/`.
            .requires("urn:cap:fs:read:*")
            .fails(FailureClass::OverBudget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_the_feeds_host_lowercased() {
        assert_eq!(
            source_of("https://Notes.Example.org:8443/feed.xml").as_deref(),
            Some("notes.example.org")
        );
        assert_eq!(
            source_of("http://u@a.example/x").as_deref(),
            Some("a.example")
        );
        assert_eq!(source_of("urn:example:feed"), None);
        assert_eq!(source_of("http://[::1]/feed"), None);
    }

    #[test]
    fn only_a_host_names_a_mapping_file() {
        assert_eq!(
            mapping_file("urn:iki:feed:mapping:a.example")
                .unwrap()
                .as_str(),
            "urn:file:feed/mappings/a.example.rq"
        );
        for bad in [
            "urn:iki:feed:mapping:",
            "urn:iki:feed:mapping:../x",
            "urn:iki:feed:mapping:a/b",
            "urn:iki:feed:mapping:A.example",
            "urn:iki:feed:mapping:.x",
            "urn:file:feed/mappings/a.example.rq",
        ] {
            assert!(mapping_file(bad).is_none(), "{bad}");
        }
    }
}
