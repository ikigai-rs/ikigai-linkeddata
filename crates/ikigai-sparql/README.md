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

`space_with_store(store: Arc<ikigai_sparql::Store>)` binds the same four IRIs — **plus
`urn:sparql:update`** — over a **caller-owned live store**: the seam that lets a host
expose the RDF its other modules write (an explanation archive, annotation graphs) as one
SPARQL-able, and now writable, shared graph. Use the re-exported `ikigai_sparql::Store`
(= `oxigraph::store::Store`) so every module's `Arc<Store>` is the same type.

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
- **No `graph=` argument** on any of the five (loading kernel-resolved sources would
  permanently mutate the shared store — per-query federation remains `space()`'s job).
  Passing it is an error.
- **Results are uncacheable** — see [Writing: `urn:sparql:update`](#writing-urnsparqlupdate)
  for why an update's golden thread does not yet change that.
- A *query*'s default graph is the union of all graphs in the store (named graphs are
  visible to plain queries; `GRAPH <uri> { … }` addresses one) — same as `space()`. An
  *update* gets plain SPARQL 1.1 semantics instead; see below.

## Writing: `urn:sparql:update`

`urn:sparql:update` is a `Verb::Sink` bound **only** by `space_with_store`. It applies a
SPARQL 1.1 UPDATE — `INSERT DATA`, `DELETE … INSERT … WHERE`, `CLEAR`, `DROP`, … — to the
shared store, and it is the one writing verb in `urn:sparql:*`. Before it existed the
ecosystem could query its graphs from anywhere and could only *change* them through
bespoke Rust; a namespace rename shipped as a one-shot binary. That whole transform is now
one query (the crate tests it, over a store shaped like the real one).

```shell
# the verbatim tail of a `sink` is its content, so the bare form works:
sink urn:sparql:update INSERT DATA { <urn:x> <urn:p> "v" }

# named, when the update shares a line with other arguments:
sink urn:sparql:update update='DELETE WHERE { <urn:x> ?p ?o }'

# or pipe the update text in from anywhere it can be resolved:
source urn:fs:file path=migrate.ru | urn:sparql:update
```

| Arg | Meaning |
| --- | --- |
| `update` | the SPARQL UPDATE text |
| `content` | the same text, piped — one of the two must be present |

- **Capability-gated on `urn:cap:sparql:update`**, declared and therefore enforced by the
  kernel before the endpoint runs. ⚠ **That scope is coarse and it is the keys to the
  store**: `DROP ALL`, `CLEAR ALL` and a bare `DELETE WHERE { ?s ?p ?o }` all empty it.
  This is a deliberate v1 choice, not an omission — a partial gate that refuses `DROP ALL`
  while admitting `DELETE WHERE { ?s ?p ?o }` denies a spelling, not an act. Graph-level
  write policy belongs to the host that owns the store.
- **Atomic**: parse first, then run every operation in ONE transaction, committed only if
  all succeed. A `;`-separated update that fails halfway leaves the store untouched.
- **Plain SPARQL dataset semantics, NOT the query endpoints' union.** The default graph is
  the store's real default graph; named graphs need `GRAPH ?g`. ⚠ So a `SELECT` can see a
  quad that the identically-worded `DELETE` will not remove. Unioning an update's `WHERE`
  while its templates still target the real default graph would match everything and write
  nothing, silently — a documented asymmetry beats a silent no-op.
- **`graph=` is refused**, not ignored.
- ⚠ **`LOAD <url>` is unavailable** — deliberately. It would need oxigraph's `http-client`,
  whose fetch never passes through `urn:httpGet`, so it is invisible to the kernel and
  ungated by `urn:cap:net:*`; enabling it would quietly upgrade `urn:cap:sparql:update`
  into "arbitrary outbound HTTP", a far larger grant than the scope's name claims (and it
  would drag a TLS stack into a crate that must keep compiling to wasm32). Pull remote
  graphs in through the kernel instead.

### Does an update cut a golden thread?

**Yes — `urn:sparql:update`, the thread named after the endpoint**, cut automatically by
the kernel on a successful mutating verb. A cacheable representation declaring
`depends_on(ikigai_sparql::UPDATE_THREAD)` recomputes on its next read; the crate proves
this end to end. It is the first write to these stores the kernel can see at all.

**But the shared-store query endpoints still do not take it, and stay uncacheable.** The
cut is correct, not complete: the raw `Arc<Store>` handle remains a writer the kernel
cannot see, so a reader depending on that thread would be invalidated on kernel writes and
silently *not* on raw ones — "always fresh" would become "fresh until someone writes the
other way, then stale with no bound and no signal". A thread that is right on some writes
and wrong on others is worse than no thread. Note what the objection is *not*: bluntness. A
store-wide cut invalidating every derivation at once would be perfectly acceptable. The
missing piece is coverage, and the remaining raw writers live in other crates. The flip is
one line the day every writer to a store goes through the kernel, or a freshness watcher
cuts the same thread on any change.

## Caching

The result is `.cacheable()` and — because each graph is resolved with `inv.source` — it
depends on every source's **golden thread**. Re-running the same query is a cache hit, and
a change to any underlying graph auto-invalidates the cached result. An `http(s)://` graph
is fetched via `urn:httpGet`, so its own cache policy propagates into the query result;
`urn:`/`file:` graphs resolve directly.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
