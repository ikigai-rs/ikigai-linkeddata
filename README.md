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
| [`ikigai-sniff`](https://crates.io/crates/ikigai-sniff) | **Content-type sniffing and dispatch** — `urn:sniff` classifies opaque (`application/octet-stream`) bytes into a concrete media type by cheap prefix heuristics; `urn:transrept:auto` sniffs piped bytes and dispatches to the matching transreptor chain, so a caller can transrept without knowing the input type. |
| [`ikigai-sparql`](https://crates.io/crates/ikigai-sparql) | **SPARQL** over resolvable graphs — `urn:sparql:{select,ask,construct,describe}` resolve `graph=` sources through the kernel (cacheable, golden-thread-invalidated), run `query=`, and serialize the results. Over a host's shared live store, `urn:sparql:update` adds the one **writing** verb: a transactional SPARQL 1.1 UPDATE, capability-gated on `urn:cap:sparql:update`. |

See each crate's README for its endpoints, arguments, and serialization options.

All three crates pass [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
— the module recipe as one test per crate (`crates/*/tests/conformance.rs`): typed inputs,
every declared RDF face parsed and blank-node-free, vocabulary terms only, cacheability held
to its marking, capabilities enforced as declared. No opt-outs.

## Release notes

### 0.4.0

Streamed bodies through the two transreptors (ledger #2278, from the stream-candidates
survey, ledger #2228); the bytes paths answer as before, and each body path, collected, is
its bytes answer.

- `urn:transrept:auto` (ikigai-sniff) reads `content` in any form (inline, by reference, by
  content id), sniffs only its first `SNIFF_PREFIX` + 1 bytes (64 KiB, new and public), and
  runs the selected plan through the kernel's executor (`Invocation::transrept`) instead of
  a private copy of it that handed every step inline bytes. An incremental step now gets the
  stream uncollected. `sniff` itself never looks past `SNIFF_PREFIX`: an input whose first
  64 KiB are valid UTF-8 is `text/plain` even if later bytes are not (the text decoder
  downstream refuses those). The endpoint declares `over-budget`.
- `urn:rdf:transrept` (ikigai-rdf) declares itself `incremental`. Turtle (N-Triples with it)
  written as N-Triples or N-Quads is parsed as it arrives, with the `<<` nesting bound
  counted on each chunk before the parser sees it (the depth scan is now one incremental
  reading, which `check_turtle_nesting` runs over the whole text). Every other pair collects
  a streamed input under the run's byte budget. Declares `over-budget`.

### 0.3.0

All three crates move together to ikigai-core and ikigai-vocab 0.2.0, the streams
release (ledger #2261). This is a breaking dependency update because core types
appear in their public APIs. Conformance 0.11.0 now runs DATATYPES and FAILURES
by default; all suites require zero skipped checks, with no waivers. SPARQL ASK
preserves parsed query order to avoid a long cancellation gap in an optimized
filtered cross product over the new vocabulary; see the SPARQL budget documentation.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
