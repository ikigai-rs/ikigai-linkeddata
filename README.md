# ikigai-linkeddata

The **Linked Data toolkit** for [ikigai](https://crates.io/crates/ikigai-core) — a
Resource-Oriented Computing (ROC) resolution kernel in Rust. This workspace ships the RDF,
SPARQL, reasoning, feed- and HTML-lifting modules as ordinary ikigai resources: mount their `space()` into any kernel
(the CLI's embedded space, the in-browser kernel) and you have content negotiation and
query as resolvable, cacheable URIs.

The RDF and SPARQL crates build on Oxigraph's pure-Rust crates with **no rocksdb**, so everything is
client-side and **wasm-ready** — the browser can transrept a fetched graph or run a SPARQL
query without a server. And because they are just resources, they are **composable through
the resource model**: a SPARQL `CONSTRUCT` can be piped straight into `urn:rdf:transrept`
to render the constructed graph as an HTML table.

## Crates

| Crate | What it does |
| --- | --- |
| [`ikigai-rdf`](https://crates.io/crates/ikigai-rdf) | RDF **transreption** — `urn:rdf:transrept` re-serializes an RDF graph between syntaxes (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD) or renders it as an HTML table. Input syntax is sniffed. |
| [`ikigai-feed`](crates/ikigai-feed) | **Feed normalization**: `urn:iki:feed:graph` lifts RSS 2.0, RSS 1.0 and Atom (parsed by feed-rs) into one skolemized RDF form in RSS 1.0's vocabulary with Dublin Core. Items are named by their own IRI, dates are `xsd:dateTime`, and categories are nodes. The same lift is a (lossy) transreptor from the feed media types. Per-source mappings are data: one SPARQL CONSTRUCT file per host at `urn:iki:feed:mapping:{source}`, applied at `urn:iki:feed:mapped:{feed}` over the form and its lifted HTML (`urn:iki:feed:html:{feed}`). |
| [`ikigai-html`](crates/ikigai-html) | **HTML structure**: `urn:iki:html:graph` lifts HTML (parsed by html5ever's tree builder) into skolemized structural triples (element, attribute, child order, text), from one document or from every HTML literal under a predicate in a graph, each tree rooted at its subject. Mints a small vocabulary, `https://ikigai-rs.dev/ns/html#`. |
| [`ikigai-reason`](crates/ikigai-reason) | **OWL 2 RL reasoning** through reasonable 0.4.4 — `urn:reason:closure` returns input plus entailments; `urn:reason:inferred` returns only new triples. Bounded, cacheable, and invalidated through input graphs' golden threads. |
| [`ikigai-sniff`](https://crates.io/crates/ikigai-sniff) | **Content-type sniffing and dispatch** — `urn:sniff` classifies opaque (`application/octet-stream`) bytes into a concrete media type by cheap prefix heuristics; `urn:transrept:auto` sniffs piped bytes and dispatches to the matching transreptor chain, so a caller can transrept without knowing the input type. |
| [`ikigai-sparql`](https://crates.io/crates/ikigai-sparql) | **SPARQL** over resolvable graphs — `urn:sparql:{select,ask,construct,describe}` resolve `graph=` sources through the kernel (cacheable, golden-thread-invalidated), run `query=`, and serialize the results. Over a host's shared live store, `urn:sparql:update` adds the one **writing** verb: a transactional SPARQL 1.1 UPDATE, capability-gated on `urn:cap:sparql:update`. |

See each crate's README for its endpoints, arguments, and serialization options.

All six crates pass [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
— the module recipe as one test per crate (`crates/*/tests/conformance.rs`): typed inputs,
every declared RDF face parsed and blank-node-free, vocabulary terms only, cacheability held
to its marking, capabilities enforced as declared. No opt-outs.

## Designs

- [ikigai-harvest](docs/design/feed-harvester.md): a feed harvester run as a persistent host job
  under a fixed authority: fetch state, a per-host pacer, immutable item atoms, failure as
  state, and per-day windows with materialized views. A resource-first design pass, not yet
  built (ledger #2683).

## OWL 2 RL reasoning

Mount `ikigai_reason::space()` beside `ikigai_rdf::space()` and the host's graph
sources. Resolve `urn:reason:closure graph=urn:my:data,urn:my:vocabulary` to get
input plus entailments, or `urn:reason:inferred` with the same arguments for
only new triples. `profile=owl-rl` is the default. Turtle is the default face;
the other RDF faces and HTML reuse RDF transreption. Nothing changes the input.

Reasoning admits at most 1,000 input triples and returns at most 100,000 closure
triples by default; hosts configure both with `space_with_limits`. Both limits
refuse rather than truncate. See the [crate README](crates/ikigai-reason/README.md)
for the cache contract, errors, bounds, wasm, and RDF 1.2 feature integration.

## Release notes

### 0.3.2

`bindings=` on the four query forms of ikigai-sparql (ledger #2517, named requests L1): a
JSON object of variable → term, in ikigai-store's shape, substituted into the PARSED query,
so a parameterized request needs no string splice into SPARQL. A binding the query does not
project is refused naming `bindings`, never ignored; `urn:sparql:update` refuses the argument.
Additive; ikigai-rdf and ikigai-sniff move with the workspace unchanged.

### 0.3.1

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
