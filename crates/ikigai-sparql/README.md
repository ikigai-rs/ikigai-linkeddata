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

Each list is what that form actually serves, and an `as=` outside it is **refused** rather
than substituted — including one the *other* form serves (`as=text/turtle` on a SELECT).
The error names the target and the formats that form accepts. Omitting `as=` takes the
default; giving one that cannot be honored is an error, never a quiet fallback.

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

## Bounds on caller SPARQL

Oxigraph's SPARQL parser and evaluator are **recursive**, so a query's shape is a claim on
the stack of whatever thread runs it, and running out is not an error a caller gets back:
Rust aborts the **whole process** on a stack overflow, on any thread. Through 0.1.10, one
query of about 2 KB — `SELECT * WHERE { FILTER(((…1…))) }` with ~1,000 parentheses —
aborted any release-built host that ran it, through any of the nine doors that parse caller
text (the four query forms in each space, and `urn:sparql:update`), and a host that exposes
`urn:sparql:select` to anonymous visitors exposes that to anyone.

Two layers now stand in front of the parser, at all nine doors:

| bound | value | what it stops |
| --- | --- | --- |
| `limits::MAX_SPARQL_BYTES` | 1 MiB | any query or update larger, refused before parsing |
| `limits::MAX_SPARQL_NESTING` | 64 | brackets `(` `{` `[` `<<`, and runs of `!`, nested deeper, refused before parsing |
| the SPARQL thread | 16 MiB + 512 bytes per byte of text | everything else that recurses, run on a stack sized for it |

**The bounds refuse, never truncate**: the refusal is an `InvalidArgument` on `query` (or
`update`/`content`) that names the bound, and a query is refused before any `graph=` source
is resolved. Real queries nest about 5 deep; 64 is an order of magnitude of headroom, and
the largest legitimate queries (`VALUES` lists, `INSERT DATA`) are flat and do not recurse
at all, which is why the byte bound can be generous.

**Why a thread as well.** Nesting is not the only recursion. On a 2 MiB thread (a tokio
worker's), a release build aborted on a 2,500-term `1||1…` chain, 5,000 `*1` or `+1` terms,
2,500 `FILTER`s, 2,000 triple patterns or a 5,000-step property path, none of them nested.
A generated query can really have those shapes, so nothing lexical may refuse them. The
parse, the evaluation and the serialization therefore run on a thread whose stack is
reserved in proportion to the text: an ordinary query's thread is ~16 MiB of address space,
touched only as deep as it recurses.

**The scan follows every reading.** Counting brackets while skipping strings, IRIs and
comments is wrong in a way an attacker can use: in an expression, `?a<'> ) '` is a
less-than followed by a string, and a scan that skipped `<'>` as an IRI never sees the
string open. So at each `<` where a less-than is possible, the scan follows both readings
and refuses on the deeper (`src/limits.rs` has the argument).

⚠ What this does **not** do, plainly:

- **It is a release-build guarantee for the thread.** A debug build spends ~20–50× the stack
  per level, so a long enough operator chain can still overflow a debug host. The nesting
  bound holds in both.
- **On wasm there are no threads**, so only the two bounds apply.
- **It does not bound time.** The evaluator is quadratic in an operator chain, and worse on
  some shapes: on the per-query dataset, a 5,000-step property path (15 KB) ran for more
  than ten minutes of a core and 2,000 triple patterns for more than a minute, without
  finishing. A host that runs untrusted queries needs its own budget.
- **The bounds are constants, not configuration.** They protect the process, not a policy.
  A host with its own SPARQL face over oxigraph can apply the same bound with
  `ikigai_sparql::limits::check_sparql`, and must parse on `limits::on_sparql_stack`.

`src/limits.rs` is a copy of `ikigai-store`'s (commit `6abf030`, ledger #915), kept
byte-for-byte except where marked, until a shared crate replaces both.
`tests/sparql_nesting.rs` reproduces the abort in a child process on a 2 MiB thread at all
nine doors (it fails, with the child killed by `SIGABRT`, against 0.1.10);
`tests/sparql_stack_measure.rs` re-measures the stack each shape costs when oxigraph moves.

## Conformance

Passes [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
(`tests/conformance.rs`), once per space: every input typed; CONSTRUCT and DESCRIBE
declare their six RDF faces (and SELECT/ASK their four result formats) so each face is
resolved, parsed and read for blank nodes and undefined terms — over the always-loaded
vocabulary, which is therefore checked on every walk; `space()`'s forms held to their
`.cacheable()` marking as pure functions of query and vocabulary; `urn:sparql:update`
refused without its capability and proven to read piped `content` by the write landing.
No opt-outs.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
