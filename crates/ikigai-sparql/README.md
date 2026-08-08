# ikigai-sparql

A SPARQL query module for the [ikigai-core](https://crates.io/crates/ikigai-core)
resolution kernel. It binds the four SPARQL verbs as resources — `urn:sparql:select`,
`urn:sparql:ask`, `urn:sparql:construct`, `urn:sparql:describe` — that run a `query=` over
one or more `graph=` sources **resolved through the kernel**.

A graph can be any resolvable resource: a remote document via `urn:httpGet`, a file, a
store's named graph. Federation is just listing graphs — `graph=` takes a comma- or
space-separated list, each loaded as a named graph (named by its URI) with the query's
default graph set to their union, so simple queries span every source and
`GRAPH <uri> { … }` addresses one. Built on [`oxigraph`](https://crates.io/crates/oxigraph)
0.5's in-memory store (no rocksdb), so it runs natively and in the browser.

## Endpoints

| Resource | Result shape |
| --- | --- |
| `urn:sparql:select` | solution bindings |
| `urn:sparql:ask` | boolean |
| `urn:sparql:construct` | RDF graph |
| `urn:sparql:describe` | RDF graph |

All four resolve identically — the query form determines the result shape — but they are
distinct, discoverable IRIs. Each accepts the same arguments:

| Arg | Meaning |
| --- | --- |
| `query` | the SPARQL query |
| `graph` | one or more graph source IRIs, comma- or space-separated; each resolved **through the kernel** and loaded as a named graph |
| `as` | result representation (see below) |

### `as` representations

- **SELECT / ASK** → `application/sparql-results+json` (default), `+xml`, `text/csv`,
  `text/tab-separated-values` (aliases `json`/`xml`/`csv`/`tsv`).
- **CONSTRUCT / DESCRIBE** → RDF: `text/turtle` (default), `application/n-triples`, …
  This composes with [`ikigai-rdf`](https://crates.io/crates/ikigai-rdf)'s
  `urn:rdf:transrept` for an HTML-table view of the constructed graph.

## Usage

```shell
# SELECT over a remote graph (fetched + cached through the kernel):
source urn:sparql:select query="SELECT ?name WHERE { ?s <http://ex/name> ?name }" \
    graph=https://example.org/people.ttl

# Federate two graphs and emit CSV:
source urn:sparql:select query="SELECT ?name WHERE { ?s <http://ex/name> ?name }" \
    graph="https://a.example/g.ttl, https://b.example/g.ttl" as=text/csv

# CONSTRUCT, then transrept the result to an HTML table:
source urn:sparql:construct query="CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }" \
    graph=https://example.org/people.ttl | urn:rdf:transrept as=text/html
```

```rust
use ikigai_core::{Fallback, Kernel, Space};
use std::sync::Arc;

let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![
    Arc::new(my_space) as Arc<dyn Space>,
    Arc::new(ikigai_sparql::space()) as Arc<dyn Space>,
]));
let kernel = Kernel::new(root);
```

## Shared-store variant

`space_with_store(store: Arc<ikigai_sparql::Store>)` binds the same four IRIs over a
**caller-owned live store** — the seam that lets a host expose the RDF its other modules
write (an explanation archive, annotation graphs) as one SPARQL-able shared graph. Use
the re-exported `ikigai_sparql::Store` (= `oxigraph::store::Store`) so every module's
`Arc<Store>` is the same type.

```rust
use ikigai_core::Kernel;
use ikigai_sparql::{space_with_store, Store};
use std::sync::Arc;

let store = Arc::new(Store::new()?);           // the host hands clones of this Arc
let kernel = Kernel::new(Arc::new(space_with_store(Arc::clone(&store))));
// …other modules write through `store`; urn:sparql:* queries see it live.
```

The contract differs deliberately from `space()`:

- **The caller owns the store's contents and lifecycle.** Nothing is auto-loaded — not
  even the ikigai vocabulary; call `ikigai_sparql::load_vocabulary(&store)` if schema
  joins are wanted.
- **No `graph=` argument** (loading kernel-resolved sources would permanently mutate the
  shared store — per-query federation remains `space()`'s job). Passing it is an error.
- **Results are uncacheable**: writes go through the raw store handle, which carries no
  golden thread, so a cached result could never be invalidated.
- The query's default graph is the union of all graphs in the store (named graphs are
  visible to plain queries; `GRAPH <uri> { … }` addresses one) — same as `space()`.

## Caching

The result is `.cacheable()` and — because each graph is resolved with `inv.source` — it
depends on every source's **golden thread**. Re-running the same query is a cache hit, and
a change to any underlying graph auto-invalidates the cached result. An `http(s)://` graph
is fetched via `urn:httpGet`, so its own cache policy propagates into the query result;
`urn:`/`file:` graphs resolve directly.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
