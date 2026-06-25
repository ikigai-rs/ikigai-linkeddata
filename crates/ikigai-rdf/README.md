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
`<scheme://…>` IRI → Turtle/N-Triples; a leading XML element → RDF/XML; otherwise Turtle),
so an explicit input format isn't needed for the common cases.

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
let kernel = Kernel::builder()
    .mount(ikigai_rdf::space())
    .build();
```

## Caching

Transreption is a pure function of its input bytes, so its output is *as cacheable as its
input*. The result is marked `.cacheable()` and the kernel folds in the expiry of whatever
was piped in: a stable source (e.g. `urn:kernel:catalog`) yields a cacheable result, while
a live fetch with no `Cache-Control` yields an uncacheable one. Cacheability flows down the
pipe rather than being asserted unconditionally.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
