# ikigai-harvest: a feed harvester, designed resource-first

A design pass, not code (ledger #2683; scale round 2026-10-30, register entry 11). It follows
the hub's [resource-first method](https://github.com/ikigai-rs/ikigai-gonk-starter/blob/main/docs/resource-design.md):
sets, atoms, compositions, spaces, views, then the economics. Every claim about what exists
today cites the source it was read from. `$R` is the crates.io registry checkout of each crate
at the version named; "cli main", "core main" and "http main" are the GitHub `main` branches on
2026-10-30.

## Why

Today the river is a pure composition over live feeds. `urn:iki:feed:mapped:{feed}` fetches
through `urn:http:get` and lifts on every miss (`crates/ikigai-feed/src/mapped.rs`), and the
Book's river is one SPARQL query over every feed graph (`crates/ikigai-feed/tests/river.rs`).
That one design causes five failures:

| finding | today | cause |
| --- | --- | --- |
| F4 reads drive polling | a fetch happens when a reader misses the cache | nothing fetches except a read |
| F1 one cut recomputes the whole river | any feed's new answer cuts the one river answer | the river is one composite over every feed |
| F6 one failing feed fails the river | a sub-request failure is the river's failure | failure is an error, not a value |
| F5 shared feeds are re-fetched per tenant | the same feed is fetched once per principal and corridor | the cache key is `{request, capability fingerprint, scope-chain fingerprint}` ($R/ikigai-core-0.2.9/src/cache.rs:70-101; the chain hashes every corridor name in order, space.rs:603-612) |
| F11 no history | an item that changes or leaves its feed is gone | nothing is stored |

The principles this design applies:

- Reads compose, writes compute.
- Time is the partition key, and a closed window is immutable.
- A cut's blast radius is a design parameter, so cuts are coalesced in time.
- Shared work runs under one fixed authority and is materialized as atoms.
- Failure is a recorded state.

## The module

**`ikigai-harvest`**, a new crate in this workspace beside `ikigai-feed`, answering
`urn:iki:harvest:*` (space id `urn:iki:space:harvest`). The host runs it as a persistent job
under a fixed named authority, as it already runs four jobs (cli main
`crates/ikigai-embedded/src/lib.rs:5393-5447`; the item cites 5385-5440, and the block has since
moved by a few lines).

It is the ONLY writer of its atoms. Everything a reader touches is either an atom it wrote or a
pure composition over those atoms. No read ever fetches.

**Feeds in scope.** Public feeds only: a URL with no userinfo, fetched with no `Authorization`.
A credentialed or private feed is not shared work. It stays what it is today, a live
composition inside its owner's personal corridor. The subscribe edge refuses any other feed
(below).

## 1. Sets

`{feed}` is the feed's IRI, as in `urn:iki:feed:graph:{feed}`. `{host}` is
`ikigai_feed::source_of(feed)`. `{digest}` is the hex of a kernel `ContentId` (BLAKE3,
$R/ikigai-core-0.2.9/src/content.rs:5-18). `{day}` is a UTC date (`2026-10-30`).

| set | name | one line on what a member IS |
| --- | --- | --- |
| sources | `urn:iki:harvest:sources` | the public feeds the harvester owns |
| subscribe | `urn:iki:harvest:subscribe:{feed}` | the edge that adds a feed to sources |
| feed state | `urn:iki:harvest:state:{feed}` | where fetching that feed stands: validators, last status, next due, change interval, last error |
| feed health | `urn:iki:harvest:health:{feed}` | whether that feed is current, or stale since an instant |
| host pacer | `urn:iki:harvest:host:{host}` | when that host may next be asked, and why |
| robots | `urn:iki:harvest:robots:{host}` | what the host's `robots.txt` says to this agent: crawl delay, disallowed paths |
| body | `urn:iki:harvest:body:{digest}` | one fetched feed document's bytes, exactly as received |
| lifted body | `urn:iki:harvest:graph:{digest}` | that body in ikigai-feed's form, with its host's mapping applied |
| item | `urn:iki:harvest:item:{digest}` | one version of one item: its triples, named by their digest |
| latest | `urn:iki:harvest:latest:{item}` | which item version is current for item IRI `{item}`, and when each was first seen |
| pending | `urn:iki:harvest:pending` | the window entries seen since the open window was last written |
| day window | `urn:iki:harvest:window:{day}` | the item versions first seen on that day, with their feeds |
| day view | `urn:iki:harvest:view:day:{day}` | the river of that day: every item version the window lists, as one graph |
| days | `urn:iki:harvest:days` | which days exist, which are sealed, and the current open day |
| river | `urn:iki:harvest:view:river` | the shared river: the newest version of every item from the last N days |
| health view | `urn:iki:harvest:view:health` | every source's health, as one graph |
| segment | `urn:iki:harvest:segment:{day}` | a sealed day's item atoms packed into one file with an index |
| tick | `urn:iki:harvest:tick` | the job's door: one bounded harvesting pass |
| fetch | `urn:iki:harvest:fetch:{feed}` | the edge that harvests one feed now (the tick's step, and an operator's) |
| personal river | `urn:iki:river` | a principal's river: the shared river with their overlay |

The questions people ask are sets too. "Which sources are stale?" is the health view. "What
was in the river on the 12th?" is `urn:iki:harvest:view:day:2026-10-12`. "What did this item
say before it changed?" is the history in `latest:{item}`.

## 2. Atoms

Only these hold state, and only the harvester writes them (the job's authority is the only one
holding their write capability):

| atom | written | lifetime | storage |
| --- | --- | --- | --- |
| `sources` | on subscribe or unsubscribe | mutable | one Turtle file |
| `state:{feed}` | on every fetch attempt, including a 304 | mutable, small | one file per feed |
| `health:{feed}` | **only on a transition** (current → stale, stale → current) | mutable, rare | one file per feed |
| `host:{host}` | when a slot is taken, or on a `RetryAfter` | mutable, small | one file per host |
| `body:{digest}` | once, when a fetch returns bytes not seen before | **immutable** | one file per digest |
| `item:{digest}` | once, when a lift yields an item version not seen before | **immutable** | one file per digest; later, a segment |
| `latest:{item}` | when an item's current version changes | mutable, append-mostly | one file per item IRI digest |
| `pending` | on every fetch that yields new item versions | mutable, read by no view | one file |
| `window:{day}` | open day: **at most once per coalescing interval**; sealed day: never again | immutable once sealed | one file per day |
| `view:day:{day}`, `view:river`, `view:health` | with the window (river, day view) or on a health transition (health view) | materialized; day views immutable once sealed | one file each |
| `days` | on a new day and on a seal | mutable, daily | one file |
| `segment:{day}` | once, when a sealed day is compacted | immutable | one file per day |

The materialized views are listed as atoms because the harvester writes them. Each is ALSO
defined as a composition (section 3), and the harvester writes exactly what that composition
answers. The composition is the specification, and the atom is a cache that outlives the
process and is shared across principals. A test resolves both and compares them.

All atoms are files under a harvest data root, mounted with ikigai-fs `cacheable_space`, as
`ikigai-feed`'s mappings already are. A read is cacheable and hangs from the file's own
thread. A write through the kernel cuts exactly that thread, and the write is staged and
published atomically ($R/ikigai-fs-0.3.0/src/lib.rs:96-101, 230-236, 275-292). Each set
rewrites onto its file, as `urn:iki:feed:mapping:{source}` rewrites onto
`urn:file:feed/mappings/{source}.rq`, so one name serves reads and writes. ikigai-store is not
used: today every write invalidates every read through three store-wide threads
($R/ikigai-store-0.3.1/src/endpoints.rs:265-284, 439-441). That is the opposite of the blast
radius this design needs.

### Feed state, `urn:iki:harvest:state:{feed}`

```turtle
<urn:iki:harvest:state:{feed}> h:feed <{feed}> ;
    h:etag "\"abc\"" ; h:lastModified "Thu, 29 Oct 2026 10:00:00 GMT" ;
    h:lastAttempt "2026-10-30T10:00:00Z"^^xsd:dateTime ;
    h:lastStatus h:notModified ;           # h:changed | h:notModified | h:unchanged | h:failed
    h:lastBody <urn:iki:harvest:body:{digest}> ;
    h:lastChange "2026-10-29T22:00:00Z"^^xsd:dateTime ;
    h:changeInterval "PT6H"^^xsd:duration ; # the estimate
    h:nextDue "2026-10-30T13:00:00Z"^^xsd:dateTime ;
    h:failures 0 ;
    h:lastError [ ... ] .                  # class, message, at; absent after a success
```

`h:` is a small vocabulary the crate mints, `https://ikigai-rs.dev/ns/harvest#`, as ikigai-html
mints its own. The terms are listed in "Vocabulary" below.

- **Conditional GET.** The fetch passes `If-None-Match` and `If-Modified-Since` from this atom
  in `headers=`. This holds today. A caller's own conditional request answers a 304 as an empty
  body with `http-status=304` on its type, and never cached (ikigai-http 0.5.0 `src/lib.rs:917-930`).
  On a 200, the new validators are read from the answer's `origin_validator()` (lib.rs:1348-1362).
  That is kernel-local, Rust-only metadata (`$R/ikigai-core-0.2.9/src/repr.rs:226-235`), which
  is enough because the harvester is Rust.
- **The kernel's own revalidation can also answer.** When the kernel holds a stale cached entry
  for the same request, ikigai-http replaces the caller's validators with the stored ones and
  serves the stored body on a 304 (lib.rs:790-805, 891-915). So "not modified" can arrive as a
  full body. The fetch therefore also compares the body's digest with `h:lastBody`. An equal
  digest is `h:unchanged`: the state is written, and nothing else is.
- **Change interval.** An exponentially weighted mean of the intervals between fetches that
  changed, clamped to `[min_interval, max_interval]` (host config; default 15 min to 24 h).
  `nextDue` = the attempt time + half the estimate, never earlier than the host pacer allows, and
  never earlier than the response's own freshness lifetime. A failure sets `nextDue` by backoff
  instead (section "Failure").

### Host pacer, `urn:iki:harvest:host:{host}`

`h:nextSlot` (the earliest instant this host may be asked), `h:delay` (the spacing between
requests), `h:notBefore` with `h:reason` (set from a `Retry-After`), and `h:robots` (the robots
answer it was seeded from).

- `h:delay` = max(the host floor in config, default 1 s; robots `Crawl-delay`).
- A `RetryAfter` error sets `h:notBefore` = now + its delay. ikigai-http turns a 429, or a 503
  with a usable `Retry-After`, into `RetryAfter { delay }` (ikigai-http 0.5.0
  `src/status.rs:19-40, 109-183`). A `RetryAfter` with no delay backs off by the host floor ×
  2^n.
- The pacer is per HOST, not per feed: ten feeds on one host share one slot.
- Today's rate limiter cannot do this. `ikigai-throttle`'s `RateLimit` keys on the request's
  target IRI only. For `urn:http:get` that is one window for every host, its rules are fixed at
  construction, and it refuses rather than waits ($R/ikigai-throttle-0.3.1/src/lib.rs:205-254,
  357-416). So the pacer is data in this atom, read by the tick.

### Item atoms and the latest pointer

- **Splitting a lifted body.** The body's lift (`graph:{digest}`) is split into one subgraph per
  `rss:item` subject:
  - the item's own triples;
  - the triples of the nodes it owns (its category nodes, its link and enclosure URLs);
  - its `dcterms:isPartOf` link to the channel.

  The split is serialized as canonical N-Triples (sorted lines; the form is blank-node-free by
  construction, so sorting is canonical). The item version is named by that text's
  `ContentId`. The channel is split the same way into a channel version (`item:{digest}`, with
  `a rss:channel`), so a renamed feed is history too.
- **Write-once.** A Sink to an existing `item:{digest}` with the same bytes is a no-op success.
  Different bytes are impossible by construction, and are refused with `Conflict`.
- **Latest.** `latest:{item}` lists `(digest, first seen, feed)` in order, newest first. Only a
  NEW digest appends. An item that disappears from its feed keeps its latest pointer: the
  harvester never deletes, and "gone from the feed" is a state (`h:absentSince` on the pointer),
  not an absence.

### Windows

- **The partition key is the instant an item version was FIRST SEEN by the harvester, in UTC.**
  It is not the item's `dc:date`. A publisher writes `dc:date`, so a backdated post would
  rewrite a sealed day, and a future-dated one would open a day early. The first-seen instant
  only moves forward, so sealed windows really are immutable. The item's own date stays in its
  triples, and views sort by it.
- **Open window.** Every fetch that yields new versions appends their entries to `pending`,
  which no view reads, so that write cuts nothing a reader holds. When `pending` is non-empty
  and the open window was last written at least one **coalescing interval** ago (config;
  default 5 min), the tick does three things:
  1. folds `pending` into `window:{today}`;
  2. writes `view:day:{today}` and `view:river` from it;
  3. empties `pending`.

  That is one rewrite and one cut per interval, however many feeds changed in it. The cut's
  blast radius is today's window, today's day view and the river, and nothing older.
- **Seal.** On the first tick after `{day}` ends (plus a grace period for fetches in flight;
  default 10 min), the tick does four things:
  1. folds what `pending` holds for `{day}`;
  2. writes the final `window:{day}` and `view:day:{day}`;
  3. records the day as sealed in `days`;
  4. never writes either file again. A Sink to a sealed day's window or view is refused with
     `Conflict`.

  The sealed day's reads are cacheable from their own file threads, which nothing ever cuts
  again. Its answers are served `Expiry::Never`.
- **Compaction.** A sealed day's item atoms are packed into `segment:{day}`: their N-Triples
  concatenated, plus an index of digest → (offset, length). After that,
  `item:{digest}` is answered from the segment the latest pointer names, and the per-digest files
  can go. This keeps the file count proportional to days, not items. It is the last step to
  build (see "Order"), and only if file count is measured to matter before then.

## 3. Compositions

For every set that is not an atom, from most preferred to least:

| set | kind | how |
| --- | --- | --- |
| `state:{feed}`, `health:{feed}`, `host:{host}`, `pending`, `days`, `sources` | alias rule | an exact rewrite onto its file under the harvest root (the ikigai-feed mappings pattern), reported as the canonical name |
| `body:{digest}`, `item:{digest}`, `latest:{item}`, `window:{day}`, `view:*` | alias rule | the same rewrite onto files. A sealed day's `item:{digest}` rewrites into its segment through a tiny pure function (offset lookup) |
| `robots:{host}` | generic composer + tiny pure function | `urn:http:get url=https://{host}/robots.txt` (the response's own HTTP freshness; at least `max_age=86400`) piped to a pure parser that keeps the group matching the harvester's user agent: `Crawl-delay` and `Disallow`. A 404 is "no rules". A failure is "no rules, retry tomorrow" and is recorded in the host atom, never fatal |
| `graph:{digest}` | generic composer | `urn:rdf:union content=<urn:iki:feed:graph content=<urn:iki:harvest:body:{digest}> base={feed}> with=<urn:sparql:construct graph=… query=<urn:iki:feed:mapping:{host}>>`: today's mapped feed, applied to a stored body instead of a live fetch. Pure over an immutable atom, so cacheable forever |
| `view:day:{day}` (its specification) | generic composer | `urn:sparql:construct` (`CONSTRUCT WHERE { ?s ?p ?o }`) with `graph=` the `item:{digest}` names `window:{day}` lists: a list of names (`urn:rdf:union` is binary, `content` ∪ `with`, so a many-graph union is the query's `graph=` list) |
| `view:river` (its specification) | generic composer | SPARQL CONSTRUCT over `view:day:{d}` for the last N days (config; default 7), keeping, per item IRI, the version `latest:{item}` names. Sealed days are cache hits, so this is the cost of today plus a union |
| `view:health` (its specification) | generic composer | the same identity CONSTRUCT with `graph=` every `health:{feed}` in `sources` |
| `urn:iki:river` | generic composer, in a corridor | the Book's river query, unchanged, over `graph=urn:iki:harvest:view:river,urn:iki:harvest:view:health,<the principal's subscriptions>`, filtered to the principal's subscriptions, with each row's source health joined (section 5) |

Two pieces are code, and why:

- **The tick and the fetch edge** (`urn:iki:harvest:tick`, `urn:iki:harvest:fetch:{feed}`). They
  are the writer: conditional request, digest comparison, change-interval arithmetic, pacer
  decisions, coalescing and sealing are decisions over time with writes in between. They cannot
  be one composition, because a composition is a pure read. Each step they take is a kernel
  request to a name above (Source the state, Source `urn:http:get`, Sink the body, Source
  `graph:{digest}`, Sink items, Sink the state), so every piece of work except the decisions is
  addressable and testable alone. A stored script could hold the decisions later. Today it
  cannot: ETag and Last-Modified reach Rust only (`origin_validator()`), and ikigai-script has no
  timer trigger ($R/ikigai-script-0.4.0/README.md:539-546). The precedent for a timer-fired
  writer that composes, diffs and sinks is ikigai-view's `urn:view:derive:tick`
  ($R/ikigai-view-0.2.6/src/lib.rs:605-967).
- **The item split.** It is a tiny pure function over `graph:{digest}` (a partition of a graph
  by subject, plus a digest). A SPARQL CONSTRUCT per item could select one item's triples, but
  not enumerate the items and name each by its own digest in one answer.

## 4. Spaces

- **Module internals.** `ikigai-harvest`'s space, beside `ikigai_feed::space()`,
  `ikigai_feed::mappings(files)`, the harvest file root, `ikigai_sparql::space()`,
  `ikigai_rdf::space()` and `urn:http:get`.
- **The job's authority: fixed and named, not root.** The host gains a fifth job,
  `HostJob::Harvest`, firing `urn:iki:harvest:tick` with `Verb::Source` on
  `Schedule::Every(tick_every)` (default 30 s). Unlike `Derive` and `Drain`, which keep root as
  "a stated limit" (cli main `lib.rs:5857-5885`), the harvester's reach is enumerable:
  - `urn:cap:net:*` (http's grammar; $R/ikigai-http-0.5.0/src/lib.rs:80-165, with `-` denies
    for private ranges if the host wants them);
  - write on the harvest file root;
  - read on the feed mappings.

  Every shared read happens under this ONE capability in ONE (empty) corridor chain, so each
  feed is fetched, lifted and split once, under one cache key. That is the F5 fix, made
  without kernel delegation, which is deliberately not built (core main
  `docs/design/sub-request-authority.md`).
- **Personal space.** A principal's corridor binds `urn:iki:river`, and their subscriptions
  (`urn:personal:feeds:subscriptions`, a list of feed IRIs) and read marks, over the shared
  names (`Scope::with_named`, $R/ikigai-core-0.2.9/src/space.rs:220-246). Their read is cached
  under their own key, because the corridor name and capability are in it. That entry is an
  atom read plus a filter, and the work under the shared key is not repeated.
- **Temporal space.** The river "as of" an instant is the as-of corridor the host already has
  (`Scope::with_named_at`, space.rs:248-345; cli main `lib.rs:718-791`). It binds
  `urn:iki:harvest:view:river` as of T to the union of the sealed day views up to T's day. For a
  past day those are immutable, so the answer is `Expiry::Never`, a pure function of the
  corridor's name.
- **Tenant.** A tenant is a corridor exactly like a principal. Tenants share the harvester and
  its atoms. A tenant that must not share a source list runs its own harvester under its own
  authority and root, and that is a deployment choice, not a mechanism.
- **Sealed names.** Only the harvester's file root answers `urn:iki:harvest:*`, and only the job
  authority can write it. No personal corridor may bind a `urn:iki:harvest:` name: a corridor
  overlays `urn:iki:river`, never the shared atoms.

## 5. Views and failure

**Failure is a state.** A fetch that fails, whether with `RetryAfter`, `Unavailable`,
`Timeout`, `NotFound`, `Refused`, a lift's `InvalidArgument` or `OverBudget`, does four things:

1. writes `state:{feed}`: `h:lastStatus h:failed`, `h:lastError` with its class and message,
   `h:failures` + 1, and `h:nextDue` by backoff (`min(max_interval, interval × 2^failures)`,
   or the `Retry-After`);
2. writes `health:{feed}` **only if** it was current: `h:staleSince` = the last success;
3. writes nothing else. The last good body, its items, the windows and the views stay as
   they are, so no view a reader holds is cut;
4. on `RetryAfter`, also writes the host pacer.

A success after failures clears `h:lastError` and writes `health:{feed}` back to current. That
is again a transition, and the only kind of write the health view sees. A `NotFound` or `Gone`
past a threshold (config; default 7 days of failures) marks the source `h:retired` in
`sources`, and it is fetched weekly after that.

**"Stale since T" on that source only.** The river view does not read feed state (it changes on
every fetch). The personal river joins `view:health`, which changes only on transitions, so each
row of a failing source carries `stale since T` while the rest of the river is untouched and
uncut. One failing feed never fails the river (F6), because the river reads atoms that exist,
not fetches that can fail.

**Faces.**

- Every atom and view: Turtle, with the other RDF faces through ikigai-rdf.
- `view:river` and `urn:iki:river`: also HTML through a template resource,
  `urn:iki:harvest:template:river`, as the tic-tac-toe views compose theirs.
- `view:health`: also `text/plain`, one line per source (`ok`, or `stale since T: <error class>`).
- `urn:iki:harvest:subscribe:{feed}`: a Sink. Unsubscribe is a Delete on the same name.

## Economics

**Cacheability.**

| answer | expiry | hangs from |
| --- | --- | --- |
| `body`, `item`, `graph:{digest}` | never changes | its own file thread, never cut. `graph:{digest}` is pure over the body and the host's mapping file, so editing a mapping re-lifts exactly that host's stored bodies |
| sealed `window`, `view:day` | never changes | own thread, never cut |
| open `window:{today}`, `view:day:{today}`, `view:river` | cut at most once per coalescing interval | own thread |
| `health:*`, `view:health` | cut on transitions only | own thread |
| `state:*`, `host:*`, `pending` | cut on every attempt | own thread; **no view reads them**, so these cuts reach no reader |
| `urn:iki:river` | per principal | `view:river`, `view:health`, the principal's subscriptions |

No volatile source is joined into an expensive cached composite. The two volatile atom
families, state and pacer, are read only by the tick.

**Cost of a cut, before and after.**

- **Before:** any feed's new answer cut the one river, and the next reader paid for every
  feed's lift and the whole query.
- **After:** one cut per coalescing interval. The next reader of the river reads one atom, and
  the harvester paid for today's union once.

**Errors, as part of each contract.**

| request | refusal |
| --- | --- |
| subscribe to a feed with userinfo, a non-http(s) scheme, or a host the job's net capability denies | `InvalidArgument` on `feed` |
| Sink to a sealed day; Sink to an existing `item:{digest}` with different bytes | `Conflict` |
| any write by a caller other than the job authority | `Denied` |
| an unknown digest, an unharvested feed's state, a day before the first | `NotFound` |
| a source that is failing | **no refusal**: its rows carry `stale since T` |

## What exists and what is new

| needed | status | where |
| --- | --- | --- |
| persistent host job, fixed interval, bare IRI, recorded capability | **exists** | `ikigai_time::JobRegistry::schedule_persistent` ($R/ikigai-time-0.5.0/src/lib.rs:487-501; a fire issues the bare IRI with no arguments, 690-710). `Schedule` has only `Every` (118-134), which is enough: per-feed due times are data in state atoms |
| named job authority | **exists, extended** | `HostJob` / `host_job_authority` (cli main `lib.rs:5844-5885`). **New:** a `HostJob::Harvest` variant with a scoped capability (ikigai-cli) |
| conditional GET, a 304 as metadata | **exists** | ikigai-http 0.5.0 `src/lib.rs:917-930`; validators via `origin_validator()` (1348-1362) |
| `Retry-After` as data | **exists** | `Error::RetryAfter { delay }` (ikigai-http `src/status.rs`) |
| failure caching of 5xx/429 (so a hammered failure is not refetched) | **exists on http main** | http main `src/lib.rs:226-231, 782-792` (not in the 0.5.0 on crates.io) |
| robots.txt, `Crawl-delay` | **new, in the module** | ikigai-http has none |
| per-host pacing | **new, in the module** (atoms) | ikigai-throttle keys by target IRI only |
| an HTTP timeout | **new, in the host: a gap** | the host's ureq transport sets none (cli main `lib.rs:2326-2329`), so one hung server stalls a tick. The harvester needs a transport timeout before it runs unattended |
| lift, mapping, union, CONSTRUCT | **exists** | `urn:iki:feed:graph` (`content`, `base`), `urn:iki:feed:mapping:{source}`, `urn:rdf:union`, `urn:sparql:construct` |
| durable atoms with per-name threads and atomic writes | **exists** | ikigai-fs `cacheable_space` |
| content digests | **exists** | `ContentId` (BLAKE3) |
| golden threads, auto-cut on Sink | **exists** | $R/ikigai-core-0.2.9/src/kernel.rs:5085-5091 |
| coalescing cuts in time | **not in the kernel; designed in the module** | the kernel cuts each write at once and never merges cuts (listen.rs:44-51, 175-211). The module coalesces by writing the open window at most once per interval, which needs nothing new |
| named corridors (personal, tenant), temporal corridor | **exist** | `Scope::with_named`, `Scope::with_named_at` |
| corridor templates (a personal corridor built per request from values) | **not built** (core main `docs/design/space-declarations.md`, ledger #632) | not needed: the host binds `urn:iki:river` per principal as it binds personal spaces today |
| sealing (refuse writes to a closed window) | **new, in the module** | core's "seal" is sealed name prefixes, not data |
| segments, compaction | **new, in the module** | nothing in core or ikigai-store. The nearest relative is ikigai-layers' append-only channel |
| exporting sealed windows across processes (thread-free `Never` entries) | **design only** | core main `docs/design/cache-ejection.md`. Sealed windows are the natural first customer; not needed for this module |

**Gaps this pass found**, for the hub to weigh:

1. No transport timeout in the host.
2. Response validators reach Rust only, which blocks a scripted harvester.
3. ikigai-script has no timer trigger.
4. `RateLimit` cannot key by argument (host).

None blocks the module, and 1 must ship with it.

## Order to build

Each step is shippable and tested alone. The test named on each step is the one that proves
its finding fixed.

0. **Host (ikigai-cli):** `HostJob::Harvest` with its scoped capability, and a transport timeout
   (`http.timeout`). Test: the job's capability is not root, and a server that never answers
   times out.
1. **Fetch and state:** `sources`, `subscribe`, `state:{feed}`, `health:{feed}`, `body:{digest}`,
   `graph:{digest}` and the fetch edge, with failure as state. Tests against a local server:
   - a 304 writes the state and cuts nothing else;
   - an unchanged body writes only the state;
   - a failing feed leaves the last body and flips health once (F6);
   - two principals reading one feed cause one fetch (F5).
2. **Pacer and tick:** `host:{host}`, `robots:{host}`, the tick selecting due feeds within a
   per-tick budget, and the persistent job. Tests:
   - two feeds on one host are spaced by the crawl delay;
   - a 429 with `Retry-After` holds the host;
   - a read never fetches (F4).
3. **Items:** the split, `item:{digest}`, `latest:{item}`. Tests:
   - an edited item is a new version, and the old one is still readable (F11);
   - one item in RSS and Atom is one digest.
4. **Windows and views:** `pending`, the open `window:{today}`, coalescing, the materialized
   `view:day:{today}`, `view:river`, `view:health`, and `urn:iki:river` in a personal corridor.
   Tests:
   - N feed changes inside one interval are one cut (F1);
   - each materialized atom equals its composition;
   - a failing source shows `stale since T` and no other row changes.
5. **Seal and segments:** day rollover, sealing, `Conflict` on a sealed write, and compaction
   into `segment:{day}`. Tests:
   - a sealed day reads the same before and after compaction;
   - a sealed day is never cut again.

**Later, and only on measurement:**

- sharding the open window (by host or by hour) if a coalesced rewrite of today is measured too
  slow;
- a store index (ikigai-store, once it has per-graph threads) if the file count or the river's
  union is measured too slow;
- clustering harvesters across machines if one host's fetch budget is measured insufficient.

## Vocabulary

`https://ikigai-rs.dev/ns/harvest#` (`h:`), minted by the crate with a published Turtle
definition, as ikigai-html mints its own:

- **properties:** `feed`, `etag`, `lastModified`, `lastAttempt`, `lastStatus`, `lastBody`,
  `lastChange`, `changeInterval`, `nextDue`, `failures`, `lastError`, `staleSince`, `retired`,
  `absentSince`, `nextSlot`, `delay`, `notBefore`, `reason`, `robots`, `firstSeen`, `version`,
  `sealed`;
- **statuses:** `changed`, `notModified`, `unchanged`, `failed`.

Where a published term fits, it is used instead: `dcterms:isPartOf`, `prov:generatedAtTime`,
`dcterms:hasVersion`, `dcterms:replaces`.
