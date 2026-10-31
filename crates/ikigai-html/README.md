# ikigai-html

HTML lifted into **structural RDF** (elements, attributes, child order, text) so a SPARQL
query can see inside it: a feed mapping can select a `blockquote/@cite` or a `p.cite > a`
from an item's content with graph patterns, not a REGEX over an HTML literal. The lift is
an ordinary ikigai resource: pure, cacheable, wasm-ready.

```text
source urn:http:get url=https://example.org/page.html | urn:iki:html:graph
source urn:iki:html:graph graph=urn:my:feed predicate=http://purl.org/rss/1.0/modules/content/encoded
```

Parsing is [html5ever](https://crates.io/crates/html5ever) 0.40 (MIT OR Apache-2.0), the
WHATWG tree builder Servo uses. The triples are the tree a browser would build: implied tags,
the adoption agency, foster parenting and foreign (SVG, MathML) content are all html5ever's.
This crate is the arena that tree is built into, and the walk from it to triples.

## The resource

`urn:iki:html:graph` (`Source`, `Meta`). Give `content`, or `graph` with `predicate`:

| argument | | |
| --- | --- | --- |
| `content` | read as a body | one HTML document's bytes (UTF-8; invalid sequences are replaced) |
| `graph` | an IRI | an RDF graph, resolved through the kernel (`http(s)` through `urn:http:get`); every literal under `predicate` in it is lifted as an HTML **fragment** |
| `predicate` | an IRI, with `graph` | the predicate whose literals are HTML: `content:encoded`, `rss:description`, … |
| `as` | optional | the RDF face: `text/turtle` (default), `application/n-triples`, `application/n-quads`, `application/trig`, `application/rdf+xml`, `application/ld+json`, `text/html` |

The `graph` form is one request per feed: each item's tree hangs on the item, so there are no
per-item names to resolve. The answer holds the trees and the `htm:tree` links, not the
graph's own triples: query both together (`urn:sparql:select graph=urn:my:feed,urn:my:html`).
A literal on a blank subject is refused (its tree would hang on nothing nameable). Literals of
any datatype (`rdf:HTML`, a plain string) are read by their lexical form; IRIs under the
predicate are not HTML and are passed over.

Turtle is written here in one triple order; every other face is [ikigai-rdf](../ikigai-rdf)'s
transreption of it, so mount `ikigai_rdf::space()` beside `ikigai_html::space()` to serve them.
The answer is `.cacheable()`: a function of the bytes it is handed, or of the graph it
resolves, whose golden thread it inherits.

Not declared a transreptor from `text/html`: a structural lift is not "the same resource in
another form", and a plan should not reach it unasked.

## The form

```turtle
@prefix htm:     <https://ikigai-rs.dev/ns/html#> .
@prefix rdf:     <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix schema:  <https://schema.org/> .

<https://example.org/posts/1> htm:tree <nih:sha-256;…f> .
<nih:sha-256;…f> a htm:Fragment ; htm:property content:encoded ; dcterms:hasPart <nih:sha-256;…q> .
<nih:sha-256;…q> a htm:Element ; schema:position 1 ; htm:localName "blockquote" ;
    htm:attribute <nih:sha-256;…c> ; dcterms:hasPart <nih:sha-256;…t> .
<nih:sha-256;…c> a htm:Attribute ; htm:localName "cite" ; rdf:value "https://example.org/quoted" .
<nih:sha-256;…t> a htm:Text ; schema:position 1 ; rdf:value "first" .
```

A `p class="cite"` holding a link, selected:

```sparql
SELECT ?item ?href WHERE {
  ?item htm:tree ?root . ?root dcterms:hasPart+ ?p .
  ?p htm:localName "p" ; htm:attribute [ htm:localName "class" ; rdf:value ?class ] ;
     dcterms:hasPart ?a .
  FILTER(CONTAINS(CONCAT(" ", ?class, " "), " cite "))
  ?a htm:localName "a" ; htm:attribute [ htm:localName "href" ; rdf:value ?href ] .
}
```

- **Roots.** A document is an `htm:Document` whose child is the `html` element. A fragment is
  parsed as the content of a `<body>` (the WHATWG fragment algorithm), and its top-level nodes
  are the `htm:Fragment`'s own children: no `html` or `body` wrapper sits between item and
  content.
- **Children** are elements and texts, each linked by `dcterms:hasPart` from its parent and
  numbered by `schema:position` from 1 in document order. Adjacent text is one node, as in
  the DOM; whitespace-only text is kept. Comments, processing instructions and the doctype
  are not lifted. A `<template>`'s contents are its children.
- **Names** are html5ever's: lowercase for HTML, the SVG/MathML case for foreign content.
  `htm:namespace` is written on an element outside the HTML namespace and on an attribute in
  one (`xlink:href`); otherwise it is absent.

### Reused and minted terms

Published terms first:

| term | carries |
| --- | --- |
| `rdf:type` | the node's kind |
| `rdf:value` | a text node's text; an attribute's value |
| `dcterms:hasPart` | parent → child |
| `schema:position` | a child's 1-based position among its parent's lifted children |

Minted, because nothing published says them (`src/vocabulary.ttl`, served at
<https://ikigai-rs.dev/ns/html>):

| term | |
| --- | --- |
| `htm:Document`, `htm:Fragment`, `htm:Element`, `htm:Text`, `htm:Attribute` | the kinds of node |
| `htm:localName` | an element's or attribute's local name |
| `htm:namespace` | its namespace IRI, when not the default |
| `htm:attribute` | element → attribute |
| `htm:tree` | a resource → the fragment lifted from an HTML literal it holds |
| `htm:property` | that fragment → the predicate the literal was under |

`ik:order` (core's position) was not reused: core's shapes tie it to `ik:forkOf`. `rdf:_n`
would order children without a minted term but cannot be selected without a string filter on
the predicate; `dcterms:hasPart` with `schema:position` keeps "the children of" and "the
second child of" both one pattern.

### Skolem IRIs

Every node is an IRI; there are no blank nodes. Each is an RFC 6920 digest IRI
(`nih:sha-256;<hex>`), SHA-256 over a kind and parts, each part length-prefixed:

- a document: `document` and its text;
- a fragment: `fragment`, its subject, its predicate and its text;
- an element or text: `node`, its root, and its path of positions (`1.3.2`);
- an attribute: `attribute`, its root, its element's path, its namespace and local name.

So the same HTML under the same subject and predicate lifts to the same IRIs on every run,
and a changed item lifts to a new tree. A test pins one IRI.

## Bounds

Both refuse with `OverBudget`, never truncate:

| bound | | |
| --- | --- | --- |
| `MAX_HTML_DEPTH` | 256 | element nesting: a document's `html` is depth 1, a fragment's top-level elements are. Counted with an explicit stack after the parse, **before** the tree is walked |
| `MAX_HTML_NODES` | 100,000 | nodes the lift mints in one request (roots, elements, attributes, texts), across all of a graph's literals |
| `MAX_HTML_BYTES` | 8 MiB | the document, or a graph's literals together |
| `MAX_GRAPH_BYTES` | 8 MiB | the RDF graph the `graph` form reads (depth-scanned by ikigai-rdf before it is parsed) |

The tree builder's own scope checks walk its stack of open elements, so unchecked nesting is
quadratic before any later bound would run (700,000 nested `<div>`s ran past two minutes in a
release build). The input is therefore fed to html5ever in 4 KiB chunks, and the parse stops
between chunks once the tree is twice as deep or twice as large as the lift admits. That is a
cost guard, not the answer's bound: the exact counts above decide. Nothing in the crate
recurses over the tree, and the arena (a `Vec`) drops without recursion.
`tests/depth.rs` runs the deep cases in a child process on a 2 MiB thread.

## wasm

Pure Rust (html5ever, oxrdf, sha2): builds for `wasm32-unknown-unknown`.

## Conformance

`tests/conformance.rs` runs [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
over the space with no opt-outs; VOCABULARY holds the `htm:` terms to `VOCABULARY`.
