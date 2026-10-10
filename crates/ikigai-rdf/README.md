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

## Endpoint

| Resource | Arg | Meaning |
| --- | --- | --- |
| `urn:rdf:transrept` | `content` | the RDF document to transrept — usually piped in (e.g. from `urn:httpGet`) |
| | `as` | target representation (default `text/turtle`) |

The input syntax is sniffed from its opening tokens (`{`/`[` → JSON-LD; a leading
`<scheme:…>` IRI → Turtle/N-Triples; a leading XML element → RDF/XML; otherwise Turtle),
so an explicit input format isn't needed for the common cases. Turtle and XML both open
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
source urn:httpGet url=https://example.org/thing | urn:rdf:transrept as=application/n-triples

# Render any graph as an HTML table:
source urn:httpGet url=https://example.org/thing | urn:rdf:transrept as=text/html
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
nested `rdf:parseType` elements in RDF/XML. The scans are exported as
`check_turtle_nesting` and `check_rdfxml_nesting`.

## Caching

Transreption is a pure function of its input bytes, so its output is *as cacheable as its
input*. The result is marked `.cacheable()` and the kernel folds in the expiry of whatever
was piped in: a stable source (e.g. `urn:kernel:catalog`) yields a cacheable result, while
a live fetch with no `Cache-Control` yields an uncacheable one. Cacheability flows down the
pipe rather than being asserted unconditionally.

`urn:rdf:union` and `urn:rdf:diff` are cacheable on the same terms: a set operation over
the piped graph and `with=`, inheriting the `with=` resolution's expiry when that named a
resource rather than inline Turtle.

## Conformance

Passes [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
(`tests/conformance.rs`): every input typed, every RDF face the transreptor declares —
all six syntaxes — resolved, parsed and free of blank nodes, every term defined, and all
three endpoints held to their `.cacheable()` marking as pure functions of their inputs.
No opt-outs.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
