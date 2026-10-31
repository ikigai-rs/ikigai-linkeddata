# ikigai-feed

RSS 2.0, RSS 1.0 and Atom lifted into **one graph form**, so everything downstream (the river
queries, a category → SKOS mapping, a site with no feed) reads one shape whichever syntax a
feed was written in. The lift is an ordinary ikigai resource: pure, cacheable, wasm-ready.

```text
source urn:http:get url=https://example.org/feed.xml | urn:iki:feed:graph base=https://example.org/feed.xml
```

Parsing is [feed-rs](https://crates.io/crates/feed-rs) 3 (MIT; Atom, RSS 0.9x/1.0/2.0 into one
model, both date formats). This crate is the **mapping** from feed-rs's model to the form,
plus a bounding pre-scan (below) that also recovers what feed-rs's model drops and the form
needs.

## The resource

`urn:iki:feed:graph` (`Source`, `Meta`):

| argument | | |
| --- | --- | --- |
| `content` | required, read as a body | the feed's bytes, usually piped in from a fetch |
| `base` | optional, an IRI | the feed's own IRI: names the channel, resolves relative links (beneath any `xml:base`), and seeds the digest IRI of an item with no IRI of its own |
| `as` | optional | the RDF face: `text/turtle` (default), `application/n-triples`, `application/n-quads`, `application/trig`, `application/rdf+xml`, `application/ld+json`, `text/html` |

Turtle is written here with oxrdfio's serializer, in one triple order. Every other face is
[ikigai-rdf](../ikigai-rdf)'s transreption of that Turtle, so mount `ikigai_rdf::space()` beside
`ikigai_feed::space()` to serve them.

The answer is `.cacheable()`: the lift is a pure function of its input, so it is as cacheable
as the piped feed and hangs on the fetch's golden thread. There are no atoms and nothing else
is read.

**Transreption.** The same endpoint is declared a transreptor from `application/rss+xml`,
`application/atom+xml` and `application/rdf+xml` to each RDF face. It is declared **lossy**:
the form keeps its fields and drops the rest (iTunes, podcast and MediaRSS detail, unknown
extension elements), and ikigai-core reserves "lossless" for the same resource in another
form. A plan therefore routes a fetched feed through it when the caller consents to a lossy step
(`lossy=allow`). With the consent, `application/rdf+xml` still goes to ikigai-rdf's lossless
edge when that space is bound, because the kernel always prefers a lossless plan. RSS 1.0 is
RDF already. When only this space is bound, RDF/XML that has no RSS 1.0 channel is refused.

## The form

One vocabulary, all published: RSS 1.0 (`rss:`) and its content (`content:`) and enclosure
(`enc:`) modules, Dublin Core (`dc:`, `dcterms:`) and DCMI's abstract model (`dcam:`), with
`rdf:value`, `rdfs:label` and `skos:broader`, and the IANA link relations (`iana:`) that
[RFC 4287](https://www.rfc-editor.org/rfc/rfc4287#section-4.2.7.2) names a registered `rel` by.
ikigai-feed mints no term. `ikigai_feed::VOCABULARY` restates the published definitions of the
terms that ikigai-vocab does not already know, `ikigai_feed::RELATIONS` among them.

```turtle
@prefix rss:     <http://purl.org/rss/1.0/> .
@prefix content: <http://purl.org/rss/1.0/modules/content/> .
@prefix enc:     <http://purl.oclc.org/net/rss_2.0/enc#> .
@prefix dc:      <http://purl.org/dc/elements/1.1/> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix dcam:    <http://purl.org/dc/dcam/> .
@prefix rdf:     <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
@prefix skos:    <http://www.w3.org/2004/02/skos/core#> .
@prefix iana:    <http://www.iana.org/assignments/relation/> .
@prefix xsd:     <http://www.w3.org/2001/XMLSchema#> .
```

| field | RSS 2.0 | RSS 1.0 | Atom | predicate |
| --- | --- | --- | --- | --- |
| channel | `channel` | `channel` | `feed` | `a rss:channel` |
| item | `item` | `item` | `entry` | `a rss:item` |
| item's channel | | | | `dcterms:isPartOf` → channel |
| title | `title` | `title` | `title` | `rss:title` |
| link (literal) | `link` | `link` | first `link` with `rel="alternate"` or no `rel` | `rss:link` |
| summary | `description` | `description` | `summary` (item), `subtitle` (channel) | `rss:description` |
| full content | `content:encoded` | `content:encoded` | `content` | `content:encoded` |
| source identifier, as written | `guid` | | `id` | `dc:identifier` |
| author | `author`, `dc:creator` | `dc:creator` | `author/name` (else `email`) | `dc:creator` |
| the date | `pubDate` | `dc:date` | `published`, else `updated` | `dc:date` (`xsd:dateTime`) |
| published | `pubDate` | `dc:date` | `published` | `dcterms:issued` (`xsd:dateTime`) |
| updated | `atom:updated`, `lastBuildDate` (channel) | | `updated` | `dcterms:modified` (`xsd:dateTime`), only when it differs from published |
| language | `language` (channel) | `dc:language` | `xml:lang` | `dc:language`; on an item only when it differs from the channel's |
| rights | `copyright` | `dc:rights` | `rights` | `dc:rights` |
| category term | `category` | `dc:subject` | `category/@term` | `dc:subject` (literal) and `dcterms:subject` → category node |
| subcategory | `itunes:category` nested (channel) | | | its own category node, `skos:broader` → the parent's node; also a subject (below) |
| enclosure | `enclosure`, `media:content` | | `link rel="enclosure"` | `enc:enclosure` → its URL; `enc:type`, `enc:length` (`xsd:integer`) on it |
| via | | | `link rel="via"` | `dcterms:source` |
| related | | | `link rel="related"` | `dcterms:relation` |
| every alternate link | `link` | `link` | `link rel="alternate"` or no `rel` (each, not only the first) | `iana:alternate` → its URL |
| comments | `comments` | | `wfw:commentRss` | `iana:replies` → its URL |
| other links | `atom:link` (any `rel`) | | `link` with any other `rel` | `iana:{rel}` → its URL for a rel in `RELATIONS` (`self`, `license`, `hub`, `edit`, `next`, …); `dcterms:relation` for any other (an extension IRI, an unregistered name) |
| a link's `type`, `hreflang`, `title` | | | on `link` | on the link's URL: `dcterms:format`, `dcterms:language`, `dcterms:title` (an enclosure's type stays `enc:type`) |
| content stored outside the feed | | | `content/@src` | `dcterms:hasFormat` → the src; `dcterms:format` its `type` |
| the feed an item was copied from | `source/@url` | | `source` (its `link rel="self"`, else its `id`, else its alternate link) | `dcterms:source` → that IRI |

`dc:date`, `dc:subject` and the literal `rss:link` are the shape the
[ikigai Book's](https://github.com/ikigai-rs/ikigai-tutorial) RSS example writes. `tests/river.rs`
runs its river query, verbatim, over the form lifted from all three syntaxes.

Nothing in the form needs a feed-only field. A channel is "the document the items came from",
and it is named by `base` when one is given, so a site with no feed can write the same form.

### Item IRIs (no blank nodes anywhere)

The first that applies:

1. **The entry's own IRI**: Atom's `atom:id`, or RSS 1.0's `rdf:about`, when it is an absolute IRI.
2. **An RSS `guid`** with `isPermaLink="true"`, or with no `isPermaLink` when the guid is an
   absolute IRI. A guid with `isPermaLink="false"` never names the item, even when it looks
   like an IRI.
3. **The item's link** (as resolved against `xml:base` and `base`), when absolute.
4. **A digest IRI** over the channel IRI, the title and the date (below).

The channel's IRI is the first of: `base`; `atom:link rel="self"`; the Atom feed's `id`; RSS
1.0's channel `rdf:about`; the channel's link; a digest of its title.

**Digest IRIs** are [RFC 6920](https://www.rfc-editor.org/rfc/rfc6920)'s `nih:` form:
`nih:sha-256;` plus the lowercase hex SHA-256 of a kind and its parts, each one followed by
U+0000 (no XML text can contain U+0000):

| kind | parts | names |
| --- | --- | --- |
| `item` | channel IRI, title, date (`xsd:dateTime` lexical form, or empty) | an item with no IRI of its own |
| `channel` | title | a channel with no IRI of its own |
| `category` | scheme (as written, or empty), term | a category node |
| `subcategory` | the parent's node IRI, term | a subcategory node (so one term under two parents is two nodes) |
| `scheme` | the scheme's text | a scheme that is not an absolute IRI |

### Dates

Every date is an `xsd:dateTime` in UTC (`2026-10-07T17:00:00Z`). feed-rs parses RFC 3339
(Atom, RSS 1.0's W3CDTF) and RFC 5322 (RSS 2.0), including the zone names RSS writers use
(`GMT`, `UT`, `EST`/`EDT`, `CST`/`CDT`, `MST`/`MDT`, `PST`/`PDT`). The written offset is
normalized away: the instant is kept, the zone it was written in is not (on purpose; see
"Date offsets" below). Published and updated
dates stay separate when the source gives both. feed-rs copies an RSS `pubDate` into "updated"
when nothing updated the item, so `dcterms:modified` is written only when it differs from
`dcterms:issued`. A date that does not parse is dropped.

### Categories

Each category is a node, named by digest of scheme and term, so the same category from two
feeds, or from RSS and Atom, is one node:

```turtle
<item> dc:subject "rust" ;                         # the term, for the Book's queries
       dcterms:subject <nih:sha-256;…> .
<nih:sha-256;…> rdf:value "rust" ;                # term
                rdfs:label "Rust" ;               # label (Atom), when present
                dcam:memberOf <http://example.org/tags> .  # scheme / RSS domain, when present
```

A scheme that is not an absolute IRI (an RSS `domain="Syndic8"`) becomes a node named by
digest of its text, with `rdfs:label` the text.

**Hierarchies.** A subcategory (feed-rs's `subcategories`: iTunes nests `itunes:category`) is
a node of its own, named by digest of its parent's node and its term, with `skos:broader` its
parent's node, so the Topics-style closure (`skos:broaderTransitive`, or the path
`skos:broader+`) runs over feed categories:

```turtle
<channel> dc:subject "Technology" , "Podcasting" ;
          dcterms:subject <nih:…technology> , <nih:…podcasting> .
<nih:…podcasting> rdf:value "Podcasting" ; skos:broader <nih:…technology> .
```

**The parents are subjects too.** Every level is a `dc:subject` / `dcterms:subject` of the
item or channel: iTunes lists a show in the parent category as well as the child, and the
parent was already a subject before hierarchies were read, so leaving it out would have removed
triples. RSS 2.0's slash-separated `category` text (`a/b/c`) is not split: the spec leaves the
convention to processors, and `TCP/IP` is a tag, not a path. RSS 1.0's `dc:subject` is a
category with no scheme. The lift writes `skos:broader` and no `skos:Concept` typing. A separate
mapping resource can make these nodes full SKOS concepts (ledger #2623).

### Worked example

```xml
<rss version="2.0">
  <channel>
    <title>Example Notes</title>
    <link>https://notes.example.org/</link>
    <language>en</language>
    <item>
      <title>Hello, form</title>
      <link>https://notes.example.org/posts/hello</link>
      <guid isPermaLink="false">post-0001</guid>
      <description>A first post.</description>
      <pubDate>Wed, 07 Oct 2026 10:00:00 PDT</pubDate>
      <category domain="https://notes.example.org/tags">rdf</category>
      <enclosure url="https://notes.example.org/hello.mp3" length="4242" type="audio/mpeg"/>
    </item>
  </channel>
</rss>
```

`urn:iki:feed:graph base=https://notes.example.org/feed.xml` (prefixes as above; the guid is not
a permalink, so the link names the item):

```turtle
<https://notes.example.org/feed.xml> a rss:channel ;
    rss:title "Example Notes" ;
    rss:link "https://notes.example.org/" ;
    dc:language "en" .
<https://notes.example.org/posts/hello> a rss:item ;
    dcterms:isPartOf <https://notes.example.org/feed.xml> ;
    rss:title "Hello, form" ;
    rss:link "https://notes.example.org/posts/hello" ;
    rss:description "A first post." ;
    dc:identifier "post-0001" ;
    dc:date "2026-10-07T17:00:00Z"^^xsd:dateTime ;
    dcterms:issued "2026-10-07T17:00:00Z"^^xsd:dateTime ;
    dc:subject "rdf" ;
    dcterms:subject <nih:sha-256;6b99679bfd1e667b89646da70ffdbbf251ce342a652a9f8c2eca03a0ec9ddde3> ;
    enc:enclosure <https://notes.example.org/hello.mp3> .
<https://notes.example.org/hello.mp3> enc:type "audio/mpeg" ; enc:length 4242 .
<nih:sha-256;6b99679bfd1e667b89646da70ffdbbf251ce342a652a9f8c2eca03a0ec9ddde3> rdf:value "rdf" ;
    dcam:memberOf <https://notes.example.org/tags> .
```

`cargo run -p ikigai-feed --example lift -- FEED.xml [BASE]` prints any feed's form.

## Per-source mappings

A source's own conventions (its kinds of post, its tags) are declared as **data**, one SPARQL
CONSTRUCT file per host, never as code per site (ledger #2673).

| set | name | kind | how |
| --- | --- | --- | --- |
| source | `{source}` | key | the feed IRI's host, lowercased (`notes.example.org`). One mapping per host. `ikigai_feed::source_of` computes it |
| source mapping | `urn:iki:feed:mapping:{source}` | atom (a file) | an exact rewrite onto `urn:file:feed/mappings/{source}.rq` under the config-home root the host mounts with ikigai-fs `cacheable_space`. The file is ONE SPARQL CONSTRUCT (UNION for several facets). There is no mapping vocabulary |
| normalized feed | `urn:iki:feed:graph:{feed}` | composition | the feed at `{feed}` fetched (`urn:http:get url={feed}` for http(s), else a `Source` of `{feed}`) and lifted with `base={feed}`: `urn:iki:feed:graph` by feed IRI, so a query can name it as `graph=` |
| mapped feed | `urn:iki:feed:mapped:{feed}` | composition | `urn:rdf:union` of `urn:iki:feed:graph:{feed}` and `urn:sparql:construct graph=<urn:iki:feed:graph:{feed}> query=<urn:iki:feed:mapping:{source}>`, the query passed BY REFERENCE. On the mapping file's own `NotFound`: the normalized graph alone |

A host mounts its config-home root once and hands the same space to the mapping overlay:

```rust
let files: Arc<dyn Space> = Arc::new(ikigai_fs::cacheable_space(config_home));
let root = Fallback::new(vec![
    Arc::new(ikigai_feed::space()),
    Arc::new(ikigai_feed::mapped_space()),       // urn:iki:feed:graph:{feed}, urn:iki:feed:mapped:{feed}
    Arc::new(ikigai_feed::mappings(Arc::clone(&files))), // urn:iki:feed:mapping:{source}
    files,                                       // urn:file:{path}
    Arc::new(ikigai_sparql::space()),
    Arc::new(ikigai_rdf::space()),
    Arc::new(http),                              // urn:http:get
]);
```

**An example mapping**, `feed/mappings/notes.example.org.rq` under the config home:

```sparql
PREFIX rss: <http://purl.org/rss/1.0/>
PREFIX dcterms: <http://purl.org/dc/terms/>
# Posts titled "Quoting …" are quotations on this site.
CONSTRUCT { ?item dcterms:type <https://notes.example.org/kinds#quotation> }
WHERE { ?item a rss:item ; rss:title ?title . FILTER(STRSTARTS(?title, "Quoting ")) }
```

**Rules, each held by a test in `tests/mapped.rs`:**

- **Sealed.** Only the file space the host hands `mappings` answers `urn:iki:feed:mapping:*`.
  The overlay answers that family alone, and only for a `{source}` that is a lowercased host
  (`[a-z0-9.-]`, not starting with `.` or `-`), so the family reaches no file outside
  `feed/mappings/`. Any other name is a miss. This crate opens no file itself: unlike
  ikigai-markdown's named-mapping server, it never touches `std::fs`.
- **Falls back only on the mapping's own `NotFound`.** The mapped feed reads the mapping atom
  by itself first. Its `NotFound` belongs to the file and hangs from the file's thread, so a
  `Sink` that creates `{source}.rq` cuts the fallback, and the next read is mapped with no
  restart. A mapping that is there but wrong fails the mapped feed. So does a failure of the
  CONSTRUCT or the union, and so does a refused read of the mapping (`Denied` is not an
  absence).
- **One name for reads and writes.** The overlay reports its rewrite as the canonical name, so
  the kernel keys the cache entry and the auto-cut on `urn:file:feed/mappings/{source}.rq`,
  whichever spelling a request used. Measured: a `Sink` through `urn:file:` cuts a read through
  `urn:iki:feed:mapping:`, and a `Sink` through `urn:iki:feed:mapping:` writes the file and cuts
  both names. Either spelling is safe for a writer.
- **Caching.** The mapped feed is a pure function of the feed's bytes and the mapping's text.
  It is cacheable and hangs on the fetch's thread and the mapping file's thread. Editing one
  host's file re-maps exactly that host's feeds. Another host's stay cached, and so does every
  normalized graph, because none of them read the file.
- **The lint a mapping author runs:** what the mapping adds to a feed is
  `urn:rdf:diff content=<urn:iki:feed:mapped:{feed}> with=urn:iki:feed:graph:{feed}`, and it is
  exactly the CONSTRUCT's output. `mode=removed` is always empty, because a mapping only adds.

`urn:iki:feed:mapped` is a small function in this crate for now. Once core's reference templates
(ledger #2671) and the engine's names (ledger #2619) ship, it can become a named request.

## Bounds and errors

- **Size:** at most `MAX_FEED_BYTES` (8 MiB), read under a memory pool of that size, so a
  streamed body is refused as it arrives. Past it: `OverBudget`.
- **Depth:** before feed-rs sees the bytes, an *iterative* quick-xml scan refuses elements
  nested deeper than `MAX_FEED_DEPTH` (64; an Atom entry's fields sit at depth 3, RSS 2.0's at
  4). feed-rs itself was measured parsing 65,000 nested levels on a 2 MiB thread without
  crashing (`tests/depth.rs`, child processes). The bound applies anyway, because request
  input is bounded before any parse.
- **Not a feed:** `InvalidArgument` on `content`, naming what is accepted: RSS 0.9x/2.0
  (`<rss>`), RSS 1.0 (`<rdf:RDF>` with an RSS 1.0 channel) or Atom 1.0 (`<feed>`). JSON Feed is
  not accepted. A `base` that is not an IRI is `InvalidArgument` on `base`.

## What feed-rs loses, and what is recovered

Each loss sorted by where its data is (ledger #2672):

| loss | where the data is | recovered by |
| --- | --- | --- |
| every link after the first alternate; `hreflang`, `title` on links | in feed-rs's model (`Entry::links`, `Link::href_lang`, `Link::title`) | the lift |
| Atom `content/@src` | in feed-rs's model (`Content::src`) | the lift. feed-rs leaves the href as written, with no `xml:base` applied, so a relative src is dropped, not guessed at. For RSS, feed-rs puts `content:encoded`'s `xml:base` in the same field, so the field is read for Atom only |
| category hierarchies | in feed-rs's model (`Category::subcategories`) | the lift |
| `atom:source`, RSS 2.0 `source/@url` | dropped by feed-rs (`Entry::source` exists but no parser fills it) | the pre-scan: the source's `link` attributes and the text of its `atom:id` |
| RSS 1.0 `dc:subject` | element text feed-rs does not model | the pre-scan (below) |
| the offset a date was written in | feed-rs's model holds `DateTime<Utc>` | not recovered, on purpose (below) |
| extension namespaces | outside feed-rs's model | not recovered: the raw feed stays addressable on its golden thread |

The pre-scan reads attributes, and the text of exactly two elements at fixed places: RSS 1.0's
`dc:subject` (a child of the channel or an item) and `atom:source/atom:id`. It recovers:

- RSS 2.0 `guid/@isPermaLink`, which the IRI rule needs.
- RSS 1.0 `rdf:about` on the channel and on each item.
- `xml:lang` (the Atom feed's, or an entry's own).
- An entry's source feed: Atom's `atom:source` (its `link rel="self"`, its `atom:id`, its
  alternate link) and RSS 2.0's `source/@url`. Only an absolute IRI is used.
- RSS 1.0 `dc:subject`, trimmed, with entities and CDATA resolved. Empty text is no subject.

RSS 1.0 `dc:subject` is read in the pre-scan, not by an upstream feed-rs change, because the
read is a dozen lines at one fixed place, interprets nothing (the text is the term, as written)
and ships now. The right long-term home is still feed-rs: a patch to its RSS 1.0 parser
reading `dc:subject` into `Entry::categories`, after which this read can go.

It identifies entries the way feed-rs does, by the root's local name, with entries as children
of the root or of its channel. If the two counts ever disagree, the scan's findings are not
used. Its kept text is bounded by the feed's own size bound, and its depth by `MAX_FEED_DEPTH`.

### Date offsets

The written offset stays normalized away, and `dc:date`, `dcterms:issued` and
`dcterms:modified` stay in UTC:

- **One instant is one term.** `"2026-10-07T10:00:00-07:00"^^xsd:dateTime` and
  `"2026-10-07T17:00:00Z"^^xsd:dateTime` are equal values but different RDF terms. SPARQL
  orders and compares them as instants, but set operations are by term: the union of two graphs,
  `urn:rdf:diff` (the mapping lint), and the property that one item written as RSS 2.0 and as
  Atom lifts to the same triples. UTC makes the same instant the same term everywhere.
- **Digest IRIs hang on it.** An item with no IRI of its own is named by digest over its date's
  lexical form. Keeping offsets would rename every such item that is not written in UTC.
- **Recovering it means reparsing.** feed-rs's model holds `DateTime<Utc>`. Getting the offset
  back means either an upstream API change, or the pre-scan reparsing every date, which would
  reimplement feed-rs's lenient RFC 5322 parsing.

The form does not change, so there is nothing to note for the next release.

Still lost, and worth having back later:

- The offset a date was written in. Only the instant survives (above).
- A link relation outside `RELATIONS` (an extension IRI, an unregistered name): the link is
  kept, as `dcterms:relation`, but its relation is not.
- A relative Atom `content/@src` (feed-rs leaves `xml:base` unapplied to it).
- iTunes, podcast and MediaRSS detail that feed-rs already models (`MediaObject`'s duration,
  thumbnails, credits, transcripts, season and episode): mappable in the lift like the
  above, once published terms are chosen for them.
- Extension namespaces outside feed-rs's model. The raw feed stays addressable on its
  golden thread, and the form is declared lossy.

## wasm

Builds for `wasm32-unknown-unknown` like its siblings. feed-rs compiles a v4 UUID generator in.
This crate replaces that generator and never calls it, but on wasm uuid still needs its `js`
randomness feature, which the crate turns on.

## License

MIT OR Apache-2.0.
