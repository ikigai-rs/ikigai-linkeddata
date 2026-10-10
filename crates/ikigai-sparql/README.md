# ikigai-sparql

A SPARQL query module for the [ikigai-core](https://crates.io/crates/ikigai-core)
resolution kernel. It binds the four SPARQL verbs as resources — `urn:sparql:select`,
`urn:sparql:ask`, `urn:sparql:construct`, `urn:sparql:describe` — that run a `query=` over
one or more `graph=` sources **resolved through the kernel**.

A graph can be any resolvable resource: a remote document via `urn:httpGet`, a file, a
store's named graph. Federation is just listing graphs — `graph=` takes a comma- or
space-separated list, each loaded as a named graph (named by its URI) with the query's
default graph set to their union, so simple queries span every source and
`GRAPH <uri> { … }` addresses one. A query that states its own dataset gets exactly that
dataset instead: `FROM <uri>` makes one source the default graph, and `FROM NAMED` limits what
`GRAPH` can reach (alone, it leaves the default graph empty, as SPARQL 1.1 says). Built on [`oxigraph`](https://crates.io/crates/oxigraph)
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
| `budget` | optional: milliseconds; can only **lower** the space's time budget (see [Time budget](#time-budget)) |
| `max_rows` | optional: rows (solutions, or triples); can only **lower** the space's answer bound (see [Answer size](#answer-size)) |
| `max_bytes` | optional: serialized bytes; can only **lower** the space's answer bound |

`budget`, `max_rows` and `max_bytes` are read only as inline values: one given by reference is
refused, never ignored (see [Bounds come inline](#bounds-come-inline-or-not-at-all)).

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

Since 0.2.0, `space()` names itself `urn:iki:space:sparql` (`ikigai_sparql::SPACE_ID`); every `space_with_*`
constructor stays anonymous, for the host to name.

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
  visible to plain queries; `GRAPH <uri> { … }` addresses one) — same as `space()` — unless
  the query states a dataset: `FROM` / `FROM NAMED` are honored, never replaced by the union. An
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
| `budget` | optional: milliseconds; can only lower the space's time budget |

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
| the SPARQL thread | 16 MiB + 512 bytes per byte of text (a debug build: + 64 MiB, and 2 KiB a byte) | everything else that recurses, run on a stack sized for it |

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

- **The thread is sized for the build** (ledger #1025, after ikigai-store's #1003). An
  unoptimized build spends up to ~70× the stack per level, and through 0.2.0 a debug host
  aborted on a `1*1*…` chain of ~405 terms, well inside the algebra bound of the
  [time budget](#time-budget) below. A build with `debug_assertions` now adds 64 KiB for each
  of the 1,024 algebra nodes that bound admits, and takes 2 KiB a byte in place of 512 (an
  `IN` list is one node however long, and recurses once a member), so a debug and a release
  host admit and answer the same queries. `limits::sparql_stack_size` is the one formula.
  ⚠ It keys on `debug_assertions`, the only compile-time signal there is: a release profile
  that turns optimization off without turning debug assertions on is the one it undersizes.
- **On wasm there are no threads**, so only the two bounds apply.
- **It does not bound time.** That is the [time budget](#time-budget)'s job, below.
- **The bounds are constants, not configuration.** They protect the process, not a policy.
  A host with its own SPARQL face over oxigraph can apply the same bound with
  `ikigai_sparql::limits::check_sparql`, and must parse on `limits::on_sparql_stack`.

`src/limits.rs` is a copy of `ikigai-store`'s (commit `6abf030`, ledger #915; re-synced at
`73e9ad0`, ledger #1025), kept byte-for-byte except where marked, until a shared crate
replaces both.
`tests/sparql_nesting.rs` reproduces the abort in a child process on a 2 MiB thread at all
nine doors (it fails, with the child killed by `SIGABRT`, against 0.1.10);
`tests/sparql_stack_measure.rs` re-measures the stack each shape costs when oxigraph moves.

### Bounds on `graph=` sources

The RDF parsers that load a `graph=` source recurse too: oxrdf copies a nested RDF 1.2 triple
term once per level (Turtle, TriG, N-Triples, N-Quads, and RDF/XML's `rdf:parseType="Triple"`),
and oxjsonld's expander recurses once per nested node object. Through 0.2.0, a source of about
3,000 nested triple terms aborted the host through any of the four query forms, and so did
JSON-LD 64 node objects deep in a debug build (ledger #1043). Now each source is scanned with
[ikigai-rdf](https://crates.io/crates/ikigai-rdf)'s depth scan for the syntax it will be parsed
as, and one nested deeper than 64 (`MAX_TURTLE_NESTING`, or `MAX_JSON_NESTING` in JSON levels)
is refused as an `InvalidArgument` on `graph` that names the source, before it is loaded. The
sources that pass are loaded on a thread of 16 MiB, enough for JSON-LD at the bound in a debug
build (~4.5 MiB). `tests/sparql_graph_depth.rs` reproduces both aborts in a child process on a
2 MiB thread.

## Time budget

Through 0.1.11 nothing bounded how long a query ran, and oxigraph is slow well before it is
deep. Measured with oxigraph 0.5.11 in a release build: a 1,000-step property path (3 KB)
took 19 s of a core, 200 triple patterns 7 s, a 5,000-step path more than ten minutes, and a
50-byte cross product of the always-loaded vocabulary (`SELECT * { ?a ?b ?c . ?d ?e ?f . ?g
?h ?i }`) runs until it is killed. Through an anonymous `urn:sparql:select` door, one request
could pin a core for as long as it liked.

Every evaluation at all nine doors now runs under a **time budget**, in two layers:

| bound | value | what it stops |
| --- | --- | --- |
| the budget | 5 s by default (`budget::DEFAULT_BUDGET`), set by the host | evaluation and serialization, stopped at the deadline and refused as a `timeout` |
| `budget::MAX_JOIN_OPERANDS` | 32 | a join with more triple patterns, path steps and nested groups, refused after parsing, before planning |
| `budget::MAX_ALGEBRA_NODES` | 1024 | a query or update with more operators (patterns, `OPTIONAL`/`UNION`/`FILTER`/`BIND`, path and expression operators) |

- **The deadline** is oxigraph's `CancellationToken`, cancelled by a watchdog thread when the
  budget expires; the crate's own serialization loop checks it once per row or triple. A query
  that crosses the deadline is refused with `Error::Timeout` naming the budget — **never a
  partial answer** — and the evaluation thread has returned before the caller hears it, so
  the core is free (the tests measure the process's CPU while idle afterwards).
- **The algebra bounds exist because oxigraph's planner cannot be cancelled.** The token is
  checked only where the evaluator touches the dataset, and the planner touches none: its join
  ordering is about cubic in one join's operands, and `OPTIONAL` and operator chains are
  quadratic. A 1,000-step path cancelled after 0.5 s returned 18 s later. So the shapes the
  planner is slow in are refused by size, at once, with an `InvalidArgument` naming the
  bound. Constants cost nothing: `VALUES` rows, constant `IN (…)` members and `INSERT DATA`
  quads are not counted, and neither are variables, IRIs or literals. Inside both bounds the
  slowest plan measured is ~125 ms.
- **The evidence for the numbers.** The largest query the ecosystem runs (a survey of every
  repo) joins 11 patterns and has about 60 operators. The heaviest by data, the reading room's
  book `CONSTRUCT` over its 4.4 MB Zotero export (65,607 quads in, 11,445 triples out),
  evaluates and serializes in 18 ms. The default budget is over 250 times that.
  `tests/sparql_time_measure.rs` re-measures every number here when oxigraph moves.

### How a host sets it

The caller can only make the budget smaller; the host decides how large it may be.

```rust
use std::time::Duration;

// The ceiling: no evaluation in this space runs longer.
let space = ikigai_sparql::space_with_budget(Duration::from_secs(2));
let shared = ikigai_sparql::space_with_store_and_budget(store, Duration::from_secs(2));
// `space()` and `space_with_store()` take budget::DEFAULT_BUDGET (5 s).
```

A request may carry `budget=<milliseconds>`, and the budget that applies is the **smaller** of
that and the ceiling: asking for an hour gets the ceiling. So a host whose anonymous door
shares a kernel with trusted callers **stamps** a small `budget=` (1,000 ms is still 50 times
the heaviest legitimate query) on every request that door forwards, overwriting any the
caller sent. A budget that is not a positive whole number of milliseconds is refused.

### Updates

A `urn:sparql:update` the deadline stops was cancelled **inside its transaction**, which is
never committed: the store is exactly as it was, every operation of a `;`-separated update
included, and the refusal says `nothing was applied`. An update that finishes after its
deadline anyway (in a part the token does not reach) has been committed, and is reported as
applied — never as a timeout for a write that happened.

### What the budget does not cover

- **Planning, inside the algebra bounds**: up to ~125 ms measured, before the first check.
- **Work between two dataset touches in an operator that consumes its input before yielding**
  — an aggregate, `ORDER BY`, the build side of a join. oxigraph's join iterators combine
  in-memory tuples without checking the token, so a `COUNT(*)` over a four-way cross product of
  470 triples ran 4 s past its cancellation. The same product WITHOUT the aggregate stops at
  the deadline, because its rows reach the crate's serializer. Closing this needs oxigraph to
  check the token in those loops.
- **The size of an answer.** The budget bounds how long rows are produced, not how many.
  That is the [answer bound](#answer-size)'s job, below.
- **Resolving `graph=` sources**, which are other endpoints' requests under their own
  policies; the budget starts when parsing starts.
- **wasm**, which has no threads: no watchdog runs there, and only the algebra bounds apply.

## Answer size

Through 0.1.12 nothing bounded how LARGE an answer was. A deadline bounds how long rows are
produced, not how many: a 939-byte `VALUES` cross product (three lists of 100, no data needed)
answered with **275 MB of JSON in 680 ms** (release build), well inside the 5 s budget, and
every byte was held in memory before the first one left. ikigai-cms-web measured the same
shape answering 272 MB through its anonymous door.

Every answer is now bounded in size, and **refused, never truncated**, past the bound:

| bound | default | ceiling | counts |
| --- | --- | --- | --- |
| rows (`budget::DEFAULT_MAX_ROWS`) | 100,000 | 10,000,000 (`CEILING_MAX_ROWS`) | solutions of a `SELECT`; triples of a `CONSTRUCT` or `DESCRIBE` |
| bytes (`budget::DEFAULT_MAX_BYTES`) | 16 MiB | 1 GiB (`CEILING_MAX_BYTES`) | the serialized answer, every form |

`ASK` is exempt: its answer is one boolean. Both bounds are counted **while the answer is
serialized** — the row past the bound is refused before it is written, and every write the
serializer makes goes through a writer that refuses the one past the byte bound — so an
answer never grows in memory past its bound. The same 939-byte query is now refused in
127 ms. The refusal is `InvalidArgument` on `query`, the kind the algebra bounds use:

```text
invalid argument `query`: the answer exceeds 100000 rows; add LIMIT, narrow the query, or ask
the host for more. It was refused, not truncated: no part of it was sent (ledger #970). A
request's `max_rows=` can only lower this bound
```

(`triples` in place of `rows` for a graph answer; `bytes` and `max_bytes=` for the byte bound.)
The heaviest legitimate answer found in the ecosystem is 11,445 triples. A caller that wants
more pages with `LIMIT` and `OFFSET`.

### How a host raises it

```rust
use ikigai_sparql::budget::{AnswerBound, DEFAULT_BUDGET};

// A trusted door: a million rows and 256 MiB. AnswerBound::new refuses more than the ceiling.
let bound = AnswerBound::new(1_000_000, 256 << 20)?;
let space = ikigai_sparql::space_with_bounds(DEFAULT_BUDGET, bound);
let shared = ikigai_sparql::space_with_store_and_bounds(store, DEFAULT_BUDGET, bound);
// Every other constructor takes AnswerBound::DEFAULT.
```

A request's `max_rows=` and `max_bytes=` apply when they are **smaller**, exactly like
`budget=`: a host's anonymous door stamps small ones, and a caller asking for more gets the
space's bound.

### Not covered

What oxigraph materializes before the first row reaches the serializer: an `ORDER BY`, a
`GROUP BY` or a `DISTINCT` consumes its whole input first, and that is bounded only by the
time budget.

## Bounds come inline, or not at all

Through 0.1.12 a `budget=` given **by reference** (or as bytes that are not UTF-8) was
silently ignored, and the space's ceiling applied. That was a bypass: a host door that stamps a
small budget only when the caller sent none saw one present and stamped nothing. Now
`budget=`, `max_rows=` and `max_bytes=` given any way but inline text are refused with
`InvalidArgument` naming the argument (`budget::inline_bound`).

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
