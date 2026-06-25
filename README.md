# ikigai-linkeddata

The **Linked Data toolkit** for [ikigai](https://crates.io/crates/ikigai-core) — a
Resource-Oriented Computing (ROC) resolution kernel in Rust. This workspace ships the RDF
and SPARQL modules as ordinary ikigai resources: mount their `space()` into any kernel
(the CLI's embedded space, the in-browser kernel) and you have content negotiation and
query as resolvable, cacheable URIs.

Both crates build on Oxigraph's pure-Rust crates with **no rocksdb**, so everything is
client-side and **wasm-ready** — the browser can transrept a fetched graph or run a SPARQL
query without a server. And because they are just resources, they are **composable through
the resource model**: a SPARQL `CONSTRUCT` can be piped straight into `urn:rdf:transrept`
to render the constructed graph as an HTML table.

## Crates

| Crate | What it does |
| --- | --- |
| [`ikigai-rdf`](https://crates.io/crates/ikigai-rdf) | RDF **transreption** — `urn:rdf:transrept` re-serializes an RDF graph between syntaxes (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD) or renders it as an HTML table. Input syntax is sniffed. |
| [`ikigai-sparql`](https://crates.io/crates/ikigai-sparql) | **SPARQL** over resolvable graphs — `urn:sparql:{select,ask,construct,describe}` resolve `graph=` sources through the kernel (cacheable, golden-thread-invalidated), run `query=`, and serialize the results. |

See each crate's README for its endpoints, arguments, and serialization options.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
