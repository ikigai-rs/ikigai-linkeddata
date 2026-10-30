# ikigai-feed

RSS 2.0, RSS 1.0 and Atom lifted into **one graph form**, so everything downstream (the river
queries, a category → SKOS mapping, a site with no feed) reads one shape whichever syntax a
feed was written in. The lift is an ordinary ikigai resource: pure, cacheable, wasm-ready.

```text
source urn:http:get url=https://example.org/feed.xml | urn:iki:feed:graph base=https://example.org/feed.xml
```

Parsing is [feed-rs](https://crates.io/crates/feed-rs) 3 (MIT; Atom, RSS 0.9x/1.0/2.0 into one
model, both date formats). This crate is the **mapping** from feed-rs's model to the form,
plus a bounding pre-scan (below) that also recovers the three things feed-rs's model drops and
the form needs.

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
`rdf:value` and `rdfs:label`. ikigai-feed mints no term. `ikigai_feed::VOCABULARY` restates
the published definitions of the terms that ikigai-vocab does not already know.

```turtle
@prefix rss:     <http://purl.org/rss/1.0/> .
@prefix content: <http://purl.org/rss/1.0/modules/content/> .
@prefix enc:     <http://purl.oclc.org/net/rss_2.0/enc#> .
@prefix dc:      <http://purl.org/dc/elements/1.1/> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix dcam:    <http://purl.org/dc/dcam/> .
@prefix rdf:     <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs:    <http://www.w3.org/2000/01/rdf-schema#> .
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
| category term | `category` | | `category/@term` | `dc:subject` (literal) and `dcterms:subject` → category node |
| enclosure | `enclosure`, `media:content` | | `link rel="enclosure"` | `enc:enclosure` → its URL; `enc:type`, `enc:length` (`xsd:integer`) on it |
| via | | | `link rel="via"` | `dcterms:source` |
| related | | | `link rel="related"` | `dcterms:relation` |

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
| `scheme` | the scheme's text | a scheme that is not an absolute IRI |

### Dates

Every date is an `xsd:dateTime` in UTC (`2026-10-07T17:00:00Z`). feed-rs parses RFC 3339
(Atom, RSS 1.0's W3CDTF) and RFC 5322 (RSS 2.0), including the zone names RSS writers use
(`GMT`, `UT`, `EST`/`EDT`, `CST`/`CDT`, `MST`/`MDT`, `PST`/`PDT`). The written offset is
normalized away: the instant is kept, the zone it was written in is not. Published and updated
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
digest of its text, with `rdfs:label` the text. This mapping does not create SKOS concepts. A
separate mapping resource can turn these nodes into SKOS concepts (ledger #2623).

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

The pre-scan reads attributes only, never text. It recovers three things that feed-rs's model
drops:

- RSS 2.0 `guid/@isPermaLink`, which the IRI rule needs.
- RSS 1.0 `rdf:about` on the channel and on each item.
- `xml:lang` (the Atom feed's, or an entry's own).

It identifies entries the way feed-rs does, by the root's local name, with entries as children
of the root or of its channel. If the two counts ever disagree, the scan's findings are not
used.

Still lost, and worth having back later:

- The offset a date was written in. Only the instant survives.
- RSS 1.0 `dc:subject` on items. feed-rs reads no categories in RSS 1.0.
- `atom:source`.
- Atom `content/@src` (content stored outside the feed).
- Every alternate link after the first.
- `hreflang` and `title` on links.
- Category hierarchies (feed-rs's `subcategories`, from iTunes).
- Extension namespaces outside feed-rs's model.

## wasm

Builds for `wasm32-unknown-unknown` like its siblings. feed-rs compiles a v4 UUID generator in.
This crate replaces that generator and never calls it, but on wasm uuid still needs its `js`
randomness feature, which the crate turns on.

## License

MIT OR Apache-2.0.
