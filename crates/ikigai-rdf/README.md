# ikigai-rdf

An RDF **transreption** module for the [ikigai-core](https://crates.io/crates/ikigai-core)
resolution kernel. It binds a single resource — `urn:rdf:transrept` — that takes an RDF
document and re-serializes it into another syntax chosen by content negotiation.

Transreption is NetKernel's term for the lossless transformation between representations
of the same resource. Pipe an RDF graph in (in any common syntax — the input is sniffed),
pick an output with `as=`, and the kernel hands back the re-serialized bytes. Built on
Oxigraph's **pure** parser/serializer crates ([`oxrdfio`](https://crates.io/crates/oxrdfio)
0.2 / [`oxrdf`](https://crates.io/crates/oxrdf) 0.3) — no store, no rocksdb — so it
compiles to `wasm32` and the browser can transrept a fetched graph **client-side**.

## Features

The default `transrept` feature provides `space()` and `SPACE_ID`, with all existing
input/output syntaxes. It enables optional `oxrdfio` and the `rdfxml` scan feature.
Upstream oxrdfio 0.2 has no per-format switches, so full transreption still includes
all its parsers.

For Turtle parsing helpers, canonicalization and Turtle/JSON depth checks only:

```toml
ikigai-rdf = { version = "0.3.1", default-features = false }
```

`subclass_axioms` parses directly with `oxttl`. The depth checks themselves do not
parse RDF. This configuration includes no oxrdfio, oxjsonld, oxrdfxml or quick-xml.
Add `features = ["rdfxml"]` for `check_rdfxml_nesting` and quick-xml alone.
`rdf-12` enables triple-term handling in oxrdf/oxttl and forwards to oxrdfio only
when transreption is enabled; it does not activate the full format graph.
Cargo features are additive: another dependency enabling `transrept` restores that
graph. See [the standalone wasm consumer](tests/wasm-consumer/README.md) for the
regression check and before/after measurement.

## Endpoint

| Resource | Arg | Meaning |
| --- | --- | --- |
| `urn:rdf:transrept` | `content` | the RDF document to transrept — usually piped in (e.g. from `urn:http:get`) |
| | `src` | instead of `content`: the IRI of an RDF resource to transrept, resolved through the kernel under the caller's capability (an `http(s)://` URL via `urn:http:get`) |
| | `as` | target representation (default `text/turtle`) |

The input syntax is sniffed from its opening tokens (`{`/`[` → JSON-LD; a leading
`<scheme:…>` IRI → Turtle/N-Triples; a leading XML element → RDF/XML; otherwise Turtle),
so an explicit input format isn't needed for the common cases. A `src=` resource is
sniffed the same way. Give `content` or `src`, exactly one: both together is refused as
an `InvalidArgument`. A `src=` read is a sub-request of the transreption, so the answer is
exactly as cacheable as the source and carries its golden threads: cutting the source's
thread invalidates the transrepted answer too. Turtle and XML both open
with `<`, and the IRI is recognized by its shape — it closes with no whitespace, quote or
`<` inside it, leads with a URI scheme, and is not a bare XML QName like `<rdf:RDF>` — so
an authority-less scheme (`<urn:…>`, `<mailto:…>`, `<did:…>`) sniffs as Turtle.

### `as` targets

`text/turtle` (default), `application/n-triples`, `application/n-quads`,
`application/trig`, `application/rdf+xml`, `application/ld+json`, or `text/html` for a
human-readable subject/predicate/object table over the parsed triples. Short aliases
(`ttl`, `nt`, `trig`, `jsonld`, …) are accepted too.

## Usage

```shell
# Fetch a graph and re-serialize it to N-Triples, all client-side:
source urn:http:get url=https://example.org/thing | urn:rdf:transrept as=application/n-triples

# Render any graph as an HTML table:
source urn:http:get url=https://example.org/thing | urn:rdf:transrept as=text/html

# Or name the resource instead of piping it, read under your own capability:
source urn:rdf:transrept src=urn:org:agenda:week as=text/html
```

Mount it in any kernel — the CLI's embedded space, the in-browser kernel:

```rust
use ikigai_core::{Fallback, Kernel, Space};
use std::sync::Arc;

let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![
    Arc::new(my_space) as Arc<dyn Space>,
    Arc::new(ikigai_rdf::space()) as Arc<dyn Space>,
]));
let kernel = Kernel::new(root);
```

Since 0.2.0, `space()` names itself `urn:iki:space:rdf` (`ikigai_rdf::SPACE_ID`).

## Bounded nesting

With RDF 1.2 on (`oxrdfio/rdf-12`, which any crate in a build can turn on for all of them),
the RDF library copies a nested triple term recursively, and a stack overflow aborts the whole
host. So every door here reads caller RDF for triple-term depth before parsing it, and
refuses past `MAX_TURTLE_NESTING` (64) as an `InvalidArgument` naming the argument that
carried it (`content` or `with`): `<<` nesting in Turtle, N-Triples, N-Quads and TriG,
nested `rdf:parseType` elements in RDF/XML (all but `Resource`, `Literal` and `Collection`,
which never build a triple term). The scans are exported as `check_turtle_nesting` and
`check_rdfxml_nesting`. N-Triples and N-Quads are read with Turtle's lexer, which is safe for a
caller that stops at the parser's first error, as every door here does: oxttl resumes at the
next line after an error, so a caller that reads on past one needs a scan of its own. RDF/XML
must be UTF-8, as oxrdfxml requires; a UTF-16 document fails in the parser at its first element.

JSON-LD has no triple terms, but its expander recurses once per nested node object (a debug
build aborted at 29 levels on a 2 MiB thread, a release one at about 1,000), and re-reads a
node's content once per level. So `urn:rdf:transrept` refuses JSON-LD that nests objects and
arrays deeper than `MAX_JSON_NESTING` (64; the W3C JSON-LD test suite's deepest document nests
10), and parses what it admits on a thread of its own (`JSON_LD_STACK`, 16 MiB), so a debug
build answers what a release one does. On wasm the parse runs inline and the bound alone
applies. The scan is exported as `check_json_nesting`.

Nothing else these syntaxes nest recurses, so nothing else is bounded: blank-node property
lists, collections and RDF 1.2 annotation blocks in Turtle, and striped element nesting, XML
literals and `rdf:parseType="Resource"` in RDF/XML, are each answered at every door 32,000 levels deep (8,000 for RDF/XML)
on a 2 MiB thread in a debug build (`tests/structural_depth.rs`, ledger #1326). ⚠ That
is about the stack, not time: oxrdfxml walks its whole stack of open elements once per element,
so striped RDF/XML costs the square of its depth (8,000 levels, 330 KB, in 1.8 s of a debug
build).

## Caching

Transreption is a pure function of its input bytes, so its output is *as cacheable as its
input*. The result is marked `.cacheable()` and the kernel folds in the expiry of whatever
was piped in: a stable source (e.g. `urn:kernel:catalog`) yields a cacheable result, while
a live fetch with no `Cache-Control` yields an uncacheable one. Cacheability flows down the
pipe rather than being asserted unconditionally.

`urn:rdf:union` and `urn:rdf:diff` are cacheable on the same terms: a set operation over
the piped graph and `with=`, inheriting the `with=` resolution's expiry when that named a
resource rather than inline Turtle.

Cacheable also means **the same bytes when it is recomputed** (ledger #1566). Union and diff
write their set sorted by N-Triples text: an `oxrdf::Graph` iterates in an order its
per-graph hashing picks afresh every time. And every face relabels blank nodes in document
order (`_:b0`, `_:b1`, …), because the parsers label an anonymous node (`[ … ]`, a
collection, a JSON-LD node without `@id`, an RDF/XML node without `rdf:about`) at random.
Transreption keeps the document's order. Both tools are public:
[`BlankNodeLabels`](src/canonical.rs) and [`canonical_triples`](src/canonical.rs).

**RDF 1.2: turn on the `rdf-12` feature** (ledger #1575). It turns on the parsers' RDF 1.2
support (`oxrdf/rdf-12`, `oxrdfio/rdf-12`) and relabels the blank nodes INSIDE triple terms
(`<<( [] ex:p ex:o )>>`) in the same document order. ⚠ Feature unification does not carry it: a
build where another crate turns `oxrdf/rdf-12` on (rudof does) has triple terms, and without this
feature their blank nodes pass through untouched. That is worse than a random label: a node
written both outside and inside a triple term (`_:x ex:p <<( _:x ex:q ex:r )>>`) is split in two.
A host that parses RDF 1.2 turns this on; one with `ikigai-sparql` in it turns on
`ikigai-sparql/rdf-12`, which carries this one and the oxigraph feature it needs.

## Conformance

Passes [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
(`tests/conformance.rs`): every input typed, every RDF face the transreptor declares —
all six syntaxes — resolved, parsed and free of blank nodes, every term defined, and all
three endpoints held to their `.cacheable()` marking as pure functions of their inputs.
No opt-outs.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
