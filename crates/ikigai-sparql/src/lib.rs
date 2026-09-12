//! `ikigai-sparql` — SPARQL query as ikigai resources.
//!
//! `urn:sparql:select` / `:ask` / `:describe` / `:construct` run a `query=<sparql>` over
//! one or more `graph=<uri>` **sources resolved through the kernel** — a graph can be any
//! resolvable resource (a remote document via `urn:httpGet`, a file, a store's named
//! graph). Federation is just listing graphs: `graph=` takes a comma/space-separated list,
//! each loaded as a named graph (named by its URI) with the query's default graph set to
//! their union — so simple queries span all of them and `GRAPH <uri> { … }` addresses one.
//!
//! The **ikigai vocabulary** (`urn:ikigai:vocab` — the `ns#` ontology: `ik:Transreptor
//! rdfs:subClassOf ik:Endpoint` and the property defs) is **always loaded** as a named
//! graph and folded into the union default graph, so a catalog query can join endpoint
//! *instances* against the *schema* with no extra `graph=` — e.g.
//! `?e rdf:type/rdfs:subClassOf* ik:Endpoint` walks the class hierarchy (Oxigraph has no
//! reasoner, so the `subClassOf` axiom is traversed explicitly via a property path).
//! Because `graph=` is therefore optional, a query may run against the vocabulary alone.
//!
//! Results content-negotiate via `as=`: SELECT/ASK serialize as `application/sparql-
//! results+json` (default) / `+xml` / `text/csv` / `text/tab-separated-values`;
//! CONSTRUCT/DESCRIBE serialize as RDF (`text/turtle` default / N-Triples / …), which
//! composes with `urn:rdf:transrept` for an HTML view.
//!
//! The result is `.cacheable()` and — because each graph is resolved with `inv.source` —
//! depends on every source's golden thread, so it is cached and auto-invalidated when any
//! source changes. Built on Oxigraph's in-memory store (no rocksdb); runs in the browser.
//!
//! **Shared-store variant**: [`space_with_store`] binds the same four IRIs over a
//! caller-owned `Arc<`[`Store`]`>` — the seam that makes a host's live RDF (an explanation
//! archive, annotation graphs) SPARQL-able as ONE shared graph — **plus a fifth,
//! `urn:sparql:update`**: a `Verb::Sink` that applies a SPARQL 1.1 UPDATE to that store
//! in one transaction. It is the only writing verb in `urn:sparql:*`, and the first
//! write to this crate's stores that goes THROUGH the kernel rather than around it (see
//! [`UPDATE_THREAD`] for what that does and does not buy). The caller owns the store's
//! contents and lifecycle (nothing auto-loaded; [`load_vocabulary`] is the opt-in),
//! `graph=` is not offered, and results are uncacheable (live data, and still a
//! partially-covered golden thread). [`space`]'s behavior is unchanged.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, NamedNodeRef};
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use std::sync::Arc;

// Re-exported so downstream hosts can name ONE canonical store type: the
// `Arc<Store>` a host hands to `space_with_store` must be the *same* `Store` type
// other modules (e.g. ikigai-browse's explanation archive) hold — Rust unifies
// them only when everyone resolves the same oxigraph version. Depending on
// `ikigai_sparql::Store` instead of a direct oxigraph dep makes that alignment
// structural rather than coincidental.
pub use oxigraph::store::Store;

/// The four SPARQL **query** verbs as resources. All four resolve identically — the query
/// form (SELECT/ASK/CONSTRUCT/DESCRIBE) determines the result shape — but they're
/// distinct, discoverable IRIs, each carrying a UNIQUE description id (`sparql-{form}`)
/// so their catalog subjects and any id-keyed projection (an MCP tool name) don't collide.
///
/// **`urn:sparql:update` is deliberately NOT bound here** — only [`space_with_store`]
/// binds it. This space builds a fresh dataset per query from the `graph=` list and drops
/// it when the call returns, so an update against it would write into a store that is
/// thrown away microseconds later: a silent no-op wearing the costume of a write. Leaving
/// the IRI unbound turns that into an honest `Error::Unresolved` at the kernel, and keeps
/// the action manifold from offering something that can never take effect.
pub fn space() -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for (verb, id) in FORMS {
        space = space.bind(
            Exact::new(format!("urn:sparql:{verb}")),
            SparqlEndpoint {
                verb,
                id,
                shared: None,
            },
        );
    }
    space
}

/// The four SPARQL query verbs **and `urn:sparql:update`** over a **shared,
/// caller-owned store** — the seam that makes a host's live RDF (ikigai-browse's
/// explanation archive, annotation graphs, …) SPARQL-able *and writable* through
/// `urn:sparql:*`: every module holding a clone of the same `Arc<Store>` and this
/// space see ONE graph.
///
/// Contract (deliberately different from [`space`]):
///
/// - **The caller owns the store's contents and lifecycle.** Nothing is auto-loaded —
///   not even the ikigai vocabulary (call [`load_vocabulary`] if schema joins are
///   wanted). Writes happen through the raw handle (or other endpoints holding it),
///   or — new — through `urn:sparql:update`, the one writing verb in this crate.
/// - **No `graph=` argument, on any of the five.** Loading kernel-resolved sources
///   would mutate the shared store permanently; per-query federation is [`space`]'s
///   job. Passing `graph=` is an error, and `describe()` doesn't offer it (the
///   manifold must not over-offer).
/// - **`urn:sparql:update` is capability-gated** on [`CAP_UPDATE`], and that scope is
///   coarse on purpose — read its docs before handing it out; it includes `DROP ALL`.
/// - **Query semantics differ from update semantics, by design.** A *query*'s default
///   graph is the union of all graphs in the store, so triples in named graphs are
///   visible to plain queries and `GRAPH <uri> { … }` still addresses one — the same
///   as [`space`]. An *update* gets plain SPARQL 1.1 semantics: no union, so
///   `DELETE WHERE { ?s ?p ?o }` touches the real default graph only and named graphs
///   need an explicit `GRAPH ?g`. ⚠ That asymmetry is a trap worth knowing (SELECT
///   sees a quad the twin DELETE will not remove), and it is still the right call:
///   unioning an update's WHERE while its DELETE template writes to the real default
///   graph would match everything and delete nothing, silently. See
///   the private `SparqlUpdateEndpoint`.
/// - **Results are uncacheable — and `urn:sparql:update` does not yet change that.**
///   This is the sentence that used to read "no golden thread covers it", and it is
///   now false in its premise and true in its conclusion, so it is worth stating
///   precisely. The kernel DOES cut a thread when an update is sunk through it (see
///   [`UPDATE_THREAD`]) — that mechanism now exists and this crate tests it. What is
///   still missing is *coverage*: the raw `Arc<Store>` handle remains a writer the
///   kernel cannot see (ikigai-browse's annotate path is the live example), so a
///   reader depending on that thread would be invalidated on kernel writes and NOT on
///   raw ones. A thread that is right on some writes and wrong on others is worse
///   than no thread at all — it converts "always fresh" into "fresh until someone
///   writes the other way, then stale with no bound and no signal", which is exactly
///   the stale-cache shape this ecosystem has already paid for twice. So reads stay
///   `Expiry::Always`. The flip is one line the day every writer to a given store
///   goes through the kernel, or a freshness watcher cuts [`UPDATE_THREAD`] on any
///   change to it.
pub fn space_with_store(store: Arc<Store>) -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for (verb, id) in FORMS {
        space = space.bind(
            Exact::new(format!("urn:sparql:{verb}")),
            SparqlEndpoint {
                verb,
                id,
                shared: Some(Arc::clone(&store)),
            },
        );
    }
    // The fifth IRI, bound ONLY here: an update needs somewhere durable to write, and
    // only the shared store is durable (see [`space`]).
    space.bind(
        Exact::new("urn:sparql:update"),
        SparqlUpdateEndpoint { store },
    )
}

/// The capability `urn:sparql:update` requires — and the kernel enforces, because it is
/// declared (`Description::requires` ⇒ `enforce_requires`, checked before `invoke` and
/// before any cache lookup). The ablation is one line: delete the `.requires` from
/// the private `SparqlUpdateEndpoint::describe` and `a_caller_without_the_capability_is_denied`
/// fails.
///
/// ★ **One coarse scope, and it is the keys to the store.** Say so out loud rather than
/// letting the omission say it: SPARQL UPDATE is not a family of small permissions.
/// `DROP ALL`, `CLEAR ALL` and a bare `DELETE WHERE { ?s ?p ?o }` each empty the store,
/// and `INSERT DATA` writes any triple into any graph. Holding this scope is therefore
/// equivalent to holding write-everything-and-erase-everything on whatever store the
/// host bound. The name is deliberately unqualified so nothing about it suggests
/// otherwise.
///
/// A finer shape is designable — the parsed update exposes its operation list, so an
/// endpoint could gate `DROP`/`CLEAR`/`LOAD` apart from `INSERT`, or hold a graph
/// allow-list. It is **not** v1, for two reasons. A partial gate is theater: refusing
/// `DROP ALL` while admitting `DELETE WHERE { ?s ?p ?o }` denies a spelling, not an act.
/// And graph-level write policy belongs to the store's owner — the host that called
/// [`space_with_store`] and chooses whether to bind this endpoint at all, under which
/// IRI, and to whom it mints the scope. Coarse and honestly labelled beats fine and
/// leaky. Revisit when a host actually needs to hand out a narrower write.
pub const CAP_UPDATE: &str = "urn:cap:sparql:update";

/// The golden thread a successful `urn:sparql:update` cuts.
///
/// # Does an update cut a thread, and which one?
///
/// **Yes, and it is this one — for free, with no extra code.** The kernel cuts the
/// thread named after a mutating request's target on success, so sinking
/// `urn:sparql:update` bumps the generation of the thread `"urn:sparql:update"`. Any
/// cacheable representation that declared `depends_on(UPDATE_THREAD)` recomputes on its
/// next read. `update_cuts_a_thread_a_cacheable_reader_can_depend_on` proves that end to
/// end through a real kernel. This is the first write in this crate's history the kernel
/// can see at all.
///
/// # Then why are this crate's own reads still uncacheable?
///
/// Because the cut is **correct but not complete**, and a partially-correct thread is
/// worse than none:
///
/// - [`space_with_store`]'s store is caller-owned and, by its documented contract,
///   written through the raw `Arc<Store>` handle by other modules — ikigai-browse's
///   annotate path is the live example. Those writes never reach the kernel and cut
///   nothing.
/// - So `.cacheable().depends_on(UPDATE_THREAD)` on a SELECT would be invalidated by
///   kernel writes and silently NOT by raw ones. A read would go from "always fresh" to
///   "fresh until someone writes the other way, then stale forever" — an unbounded,
///   signal-free wrong answer, where today's cost is only a recomputation.
/// - Note what is *not* the objection: bluntness. A store-wide cut invalidating every
///   derivation at once is coarse but honest, and would be perfectly acceptable. The
///   objection is coverage, and coverage is not this crate's to fix — the raw writers
///   live in other repos.
///
/// The flip is one line (`.cacheable().depends_on(UPDATE_THREAD)` in
/// `SparqlEndpoint::invoke`'s shared-store arm) the day either holds: every writer to a
/// given store goes through the kernel, or a freshness watcher over the store cuts this
/// same thread on any change. Until then, don't cache.
///
/// # Why the thread is named after the endpoint
///
/// Because that is what the kernel's automatic cut uses — the request target's IRI. A
/// state-naming thread (`urn:sparql:store`) would read better, but an endpoint cannot
/// cut an arbitrary thread except by resolving `urn:kernel:cut`, which requires
/// `urn:cap:kernel:cut` — a *system-wide* cache-invalidation authority, strictly broader
/// than "write this one store", and one every updater would then have to hold. Paying
/// that for a nicer name, on a cut nothing depends on yet, is a bad trade for v1.
///
/// ⚠ Two consequences of that choice, stated so they are not rediscovered: mount the
/// endpoint at an alias and the cut is named after the alias, not this constant; and two
/// distinct shared stores in one process share this one thread name, so a cut on either
/// invalidates readers of both. Both are fixed by the same future work (an explicit,
/// store-named cut) and neither bites while reads are uncacheable.
pub const UPDATE_THREAD: &str = "urn:sparql:update";

/// The XSD `string` datatype IRI — the `class` of every by-value input here: query and
/// update text, a media type, and `graph=` (a list of IRIs — see its ArgSpec for why
/// that is a string and not `xsd:anyURI`).
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// What SELECT and ASK serialize as — the SPARQL 1.1 results formats `as=` selects.
/// The first is the default.
const RESULTS_OUTPUTS: [&str; 4] = [
    "application/sparql-results+json",
    "application/sparql-results+xml",
    "text/csv",
    "text/tab-separated-values",
];

/// What CONSTRUCT and DESCRIBE serialize as — RDF faces, every one an `as=` value
/// [`rdf_format`] accepts. The first is the default. Declared per form because a
/// description that says `sparql-results+json` over a Turtle-producing action hides the
/// RDF face from every consumer that reads outputs — a conformance walk included.
const GRAPH_OUTPUTS: [&str; 6] = [
    "text/turtle",
    "application/n-triples",
    "application/n-quads",
    "application/trig",
    "application/rdf+xml",
    "application/ld+json",
];

/// The four query forms and their UNIQUE description ids (see [`SparqlEndpoint`]).
const FORMS: [(&str, &str); 4] = [
    ("select", "sparql-select"),
    ("ask", "sparql-ask"),
    ("describe", "sparql-describe"),
    ("construct", "sparql-construct"),
];

/// Load the bundled ikigai vocabulary (`ikigai_vocab::VOCABULARY`) into `store` as the
/// named graph `urn:ikigai:vocab`. [`space`] does this automatically into its private
/// per-query store; a shared store ([`space_with_store`]) gets NOTHING automatically —
/// the caller owns its contents — so this is the explicit opt-in for hosts that want
/// schema joins (`?e a/rdfs:subClassOf* ik:Endpoint`) over their shared graph.
pub fn load_vocabulary(store: &Store) -> Result<()> {
    let vocab_graph = NamedNodeRef::new(ikigai_vocab::VOCAB_IRI)
        .map_err(|e| Error::Endpoint(format!("vocab graph name: {e}")))?;
    store
        .load_from_slice(
            RdfParser::from_format(RdfFormat::Turtle).with_default_graph(vocab_graph),
            ikigai_vocab::VOCABULARY.as_bytes(),
        )
        .map_err(|e| Error::Endpoint(format!("loading the ikigai vocabulary: {e}")))?;
    Ok(())
}

/// A fresh in-memory store pre-seeded with the bundled ikigai vocabulary, loaded as the
/// named graph `urn:ikigai:vocab`. Bundled (`include_str!` via `ikigai_vocab::VOCABULARY`)
/// rather than resolved through the kernel, so the schema is present even when no host
/// mounts `urn:ikigai:vocab`; it's static, so it carries no golden thread and doesn't
/// affect cacheability. Callers add their `graph=` sources on top, then union the default
/// graph — so `?e rdf:type/rdfs:subClassOf* ik:Endpoint` joins instances to this schema.
fn store_with_vocabulary() -> Result<Store> {
    let store = Store::new().map_err(|e| Error::Endpoint(format!("store init: {e}")))?;
    load_vocabulary(&store)?;
    Ok(store)
}

/// One SPARQL query form bound to `urn:sparql:{verb}`. Resolution is identical across all
/// four (the query itself carries the form); only the *identity* differs — `id` is a
/// UNIQUE description id (`sparql-select`/`…-ask`/`…-describe`/`…-construct`) so the catalog
/// subject (`urn:ikigai:endpoint:{id}`) and any id-keyed projection (an MCP tool name)
/// stay distinct. `verb` is the bare form, used only to label the title.
struct SparqlEndpoint {
    verb: &'static str,
    id: &'static str,
    /// `Some` = the shared-store variant ([`space_with_store`]): query the given live
    /// store, uncacheable, no `graph=`. `None` = the classic per-query dataset.
    shared: Option<Arc<Store>>,
}

#[async_trait]
impl Endpoint for SparqlEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let query_str = inv.inline_str("query").map_err(|_| {
            Error::Endpoint("urn:sparql:* needs a `query=<sparql>` argument".to_string())
        })?;

        // The shared-store variant: run the query over the caller-owned live store.
        // Uncacheable — raw-handle writes carry no golden thread, so a cached result
        // could never be invalidated (Expiry::Always is Representation's default).
        if let Some(store) = &self.shared {
            if inv.inline_str("graph").is_ok_and(|g| !g.trim().is_empty()) {
                return Err(Error::Endpoint(
                    "`graph=` is not supported over a shared store: loading sources would \
                     mutate the caller-owned store. Write through its handle instead, or \
                     mount ikigai_sparql::space() for per-query federation."
                        .to_string(),
                ));
            }
            let results = evaluate(query_str, store)?;
            let (media, bytes) = serialize_results(results, inv.inline_str("as").ok())?;
            return Ok(Representation::new(
                ReprType::new(&media).with_param("charset", "utf-8"),
                bytes,
            ));
        }

        // `graph=` is optional: the vocabulary graph (below) is always present, so a query
        // can run against it alone. Listed sources federate on top of it.
        let graph_list = inv.inline_str("graph").unwrap_or("");

        // Build the dataset, pre-seeded with the bundled ikigai vocabulary (see
        // `store_with_vocabulary`). Each listed source is resolved through the kernel and
        // loaded as a named graph (named by its URI). `inv.source` records the source's
        // golden thread, so the cached result invalidates when any source changes.
        let store = store_with_vocabulary()?;
        for uri in graph_list
            .split([',', ' ', '\n', '\t'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let source = resolve_graph(inv, uri).await?;
            let format = rdf_format(source.repr_type.media_type.as_str())
                .unwrap_or_else(|| sniff(&source.bytes));
            let graph = NamedNodeRef::new(uri)
                .map_err(|e| Error::Endpoint(format!("graph name `{uri}` is not an IRI: {e}")))?;
            store
                .load_from_slice(
                    RdfParser::from_format(format).with_default_graph(graph),
                    &source.bytes,
                )
                .map_err(|e| Error::Endpoint(format!("loading <{uri}>: {e}")))?;
        }

        let results = evaluate(query_str, &store)?;
        let (media, bytes) = serialize_results(results, inv.inline_str("as").ok())?;
        Ok(
            Representation::new(ReprType::new(&media).with_param("charset", "utf-8"), bytes)
                .cacheable(),
        )
    }

    fn name(&self) -> &str {
        self.id
    }

    fn describe(&self) -> Description {
        // CONSTRUCT and DESCRIBE answer with a graph; SELECT and ASK with a result set.
        // The form is fixed by the IRI, so the default `as=` and the outputs are too.
        let graph_form = matches!(self.verb, "construct" | "describe");
        let outputs: &[&str] = if graph_form {
            &GRAPH_OUTPUTS
        } else {
            &RESULTS_OUTPUTS
        };
        let desc = Description::new(self.id)
            .title(format!("SPARQL {}", self.verb.to_uppercase()))
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("query")
                    .summary("the SPARQL query (SELECT/ASK/DESCRIBE/CONSTRUCT)")
                    .class(XSD_STRING),
            );
        // The two variants offer DIFFERENT arguments: the shared-store form has no
        // `graph=` (it would mutate the caller-owned store), and advertising it here
        // would make the manifold over-offer.
        let desc = if self.shared.is_some() {
            desc.summary(
                "Run a SPARQL query over the host's shared live RDF store (the graph other \
                 modules write through the same store handle). Live data: uncacheable.",
            )
        } else {
            desc.summary(
                "Run a SPARQL query over one or more resolvable, cacheable graphs \
                 (federation by listing graphs). The ikigai vocabulary (urn:ikigai:vocab) \
                 is always loaded, so endpoint instances can join the class/property schema.",
            )
            .input(
                ArgSpec::new("graph")
                    .summary(
                        "optional: one or more graph source IRIs, comma- or space-separated; \
                         each resolved through the kernel and loaded as a named graph. Omit to \
                         query the always-present ikigai vocabulary alone.",
                    )
                    // A LIST of IRIs, and an ArgSpec has no way to say "many": `xsd:anyURI`
                    // would tell a validator that `a, b` is malformed, which it is not.
                    .class(XSD_STRING)
                    .optional(),
            )
        };
        let desc = desc.input(
            ArgSpec::new("as")
                .summary(
                    "result representation: SELECT/ASK → application/sparql-results+json \
                         (default), +xml, text/csv, text/tab-separated-values; \
                         CONSTRUCT/DESCRIBE → text/turtle (default), application/n-triples, …",
                )
                .class(XSD_STRING)
                .default_value(outputs[0]),
        );
        outputs.iter().fold(desc, |desc, media| desc.output(*media))
    }
}

/// `urn:sparql:update` — the one **writing** verb in `urn:sparql:*`, bound only by
/// [`space_with_store`].
///
/// A `Verb::Sink` that applies a SPARQL 1.1 UPDATE (`INSERT DATA`, `DELETE … INSERT …
/// WHERE`, `CLEAR`, `DROP`, …) to the caller-owned live store. Before this existed the
/// ecosystem could query its graphs from anywhere and could only *change* them through
/// bespoke Rust — which is why a namespace rename shipped as a one-shot binary
/// (`ikigai-browse`'s `migrate-annotation-ns`); with this bound, that whole transform is
/// one `DELETE … INSERT … WHERE`, and
/// `the_browse_namespace_migration_is_now_one_query` in this crate's tests is that
/// query, run against a store shaped like the real one.
///
/// Three things worth knowing, all of them tested:
///
/// - **Atomic.** Parse first, then run every operation of the update inside ONE
///   transaction, committed only if all of them succeed. A `;`-separated update that
///   fails halfway leaves the store exactly as it was — no partially-applied rename,
///   which for a namespace move is the one outcome worse than not running.
/// - **Plain SPARQL dataset semantics — deliberately NOT the query endpoints' union.**
///   The default graph is the store's real default graph; named graphs need `GRAPH ?g`.
///   Unioning the WHERE clause while the DELETE/INSERT templates still target the real
///   default graph would produce an update that matches everything and writes nothing,
///   silently. Better a documented asymmetry than a silent no-op. ⚠ So a SELECT can see
///   a quad that the identically-worded DELETE will not remove.
/// - **`graph=` is refused, not ignored.** Same reason as the query endpoints: this
///   store is the caller's, and there is nothing per-query to write to.
///
/// ⚠ **`LOAD <url>` is not available**, and that is a capability property, not an
/// oversight. It would need oxigraph's `http-client` feature, whose fetch is oxigraph's
/// own — it never passes through `urn:httpGet`, so it is invisible to the kernel and
/// ungated by `urn:cap:net:*`. Enabling it would silently upgrade [`CAP_UPDATE`] into
/// "arbitrary outbound HTTP from inside the store", which is a much larger grant than
/// the scope's name claims, and it would also drag a native TLS stack into a crate that
/// must keep compiling to wasm32. `LOAD` fails with "HTTP client is not available"
/// (cleanly, and — being an evaluation error — rolling the whole update back). The
/// ikigai way to pull a remote graph in is to resolve it through the kernel, where the
/// net capability applies: `urn:httpGet` on the query side, or a host that sources the
/// document and sinks an `INSERT DATA` built from it.
///
/// The result is uncacheable (a Sink's result is never cached anyway) and the kernel
/// cuts [`UPDATE_THREAD`] on success.
struct SparqlUpdateEndpoint {
    store: Arc<Store>,
}

#[async_trait]
impl Endpoint for SparqlUpdateEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // Refuse `graph=` loudly rather than ignoring it: a caller passing it has a
        // per-query federation model in mind, and this endpoint has none to offer.
        if inv.inline_str("graph").is_ok_and(|g| !g.trim().is_empty()) {
            return Err(Error::Endpoint(
                "`graph=` is not supported by urn:sparql:update: an update writes to the \
                 caller-owned shared store, and a per-query dataset would be discarded when \
                 the call returns. Drop `graph=`, or address a named graph from inside the \
                 update with `GRAPH <uri> { … }`."
                    .to_string(),
            ));
        }

        // `update=` is the SPARQL 1.1 Protocol's name for update text (the siblings' arg
        // is `query=`, and this is not a query); piped `content` is the pipeline form,
        // which is also what the bare `sink urn:sparql:update <text>` REPL shape produces.
        let update_str = inv
            .inline_str("update")
            .or_else(|_| inv.inline_str("content"))
            .map_err(|_| {
                Error::MissingArgument(
                    "update (the SPARQL UPDATE text, or pipe it in as content)".to_string(),
                )
            })?;

        let before = self
            .store
            .len()
            .map_err(|e| Error::Endpoint(format!("store size: {e}")))?;

        // Parse, then execute. `on_store` opens a transaction and `execute` commits it
        // only after every operation succeeds — so a syntax error applies nothing, and a
        // multi-operation update that fails halfway rolls the whole thing back.
        let prepared = SparqlEvaluator::new()
            .parse_update(update_str)
            .map_err(|e| Error::Endpoint(format!("SPARQL UPDATE syntax error: {e}")))?;
        prepared
            .on_store(&self.store)
            .execute()
            .map_err(|e| Error::Endpoint(format!("update evaluation error: {e}")))?;

        let after = self
            .store
            .len()
            .map_err(|e| Error::Endpoint(format!("store size: {e}")))?;

        // A stable, pinned one-line receipt (`update_reports_the_quad_delta_and_is
        // _uncacheable`): the quad
        // count before and after. Useful precisely where this endpoint replaces a
        // migration script, whose whole reason for a dry run was "how much did I move?".
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            format!("updated: {before} -> {after} quads\n").into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "sparql-update"
    }

    fn describe(&self) -> Description {
        Description::new("sparql-update")
            .title("SPARQL UPDATE")
            .summary(
                "Apply a SPARQL 1.1 UPDATE to the host's shared live RDF store, in one \
                 transaction (all operations commit, or none do). The only writing verb in \
                 urn:sparql:*. Plain SPARQL dataset semantics: the default graph is the \
                 store's default graph, not the union the query endpoints use — address a \
                 named graph with GRAPH <uri> { … }.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            // ★ Single-verb endpoint, so it authors flat: `Description::action_specs()`
            // synthesizes the Sink ActionSpec and carries this `requires` into it, which
            // is what `Kernel::issue` enforces before `invoke` ever runs. Declared here =
            // enforced there; see CAP_UPDATE for why the scope is coarse and what it
            // grants (short version: DROP ALL — this is the keys to the store).
            .requires(CAP_UPDATE)
            .input(
                ArgSpec::new("update")
                    .summary(
                        "the SPARQL UPDATE text (INSERT DATA / DELETE … INSERT … WHERE / \
                         CLEAR / DROP / …); falls back to piped `content` — one of the two \
                         must be present",
                    )
                    .class(XSD_STRING)
                    .optional(),
            )
            .input(
                ArgSpec::new("content")
                    .summary("the SPARQL UPDATE text as piped content — the `… | urn:sparql:update` form")
                    .class(XSD_STRING)
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
    }
}

/// Resolve a graph source through the kernel. An `http(s)://` URL is fetched via the
/// HTTP module (`urn:httpGet`) — a bare URL isn't itself a bound resource — while a
/// `urn:`/`file:` graph resolves directly. Either way the kernel records the source's
/// golden thread, so the query result is cacheable and invalidates when the graph changes.
async fn resolve_graph(inv: &Invocation<'_>, uri: &str) -> Result<Representation> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        let get = Iri::parse("urn:httpGet").expect("urn:httpGet is a valid IRI");
        let request = Request::new(Verb::Source, get)
            .with_arg("url", ArgRef::Inline(uri.as_bytes().to_vec()));
        inv.issue(request).await
    } else {
        let iri =
            Iri::parse(uri).map_err(|e| Error::Endpoint(format!("bad graph IRI `{uri}`: {e}")))?;
        inv.source(&iri).await
    }
}

/// Parse and run a query over `store` with the default graph set to the union of all
/// its graphs — so a query without an explicit `GRAPH`/`FROM` spans every graph, and
/// `GRAPH <uri> { … }` still addresses one. Both variants share these semantics.
fn evaluate<'a>(query_str: &str, store: &'a Store) -> Result<QueryResults<'a>> {
    let mut prepared = SparqlEvaluator::new()
        .parse_query(query_str)
        .map_err(|e| Error::Endpoint(format!("SPARQL syntax error: {e}")))?;
    prepared.dataset_mut().set_default_graph_as_union();
    prepared
        .on_store(store)
        .execute()
        .map_err(|e| Error::Endpoint(format!("query evaluation error: {e}")))
}

/// Serialize query results by their kind, honoring the `as` representation.
fn serialize_results(results: QueryResults, as_type: Option<&str>) -> Result<(String, Vec<u8>)> {
    let io = |e: std::io::Error| Error::Endpoint(format!("serialize: {e}"));
    match results {
        QueryResults::Solutions(solutions) => {
            let format = results_format_or_default(as_type)?;
            let variables = solutions.variables().to_vec();
            let mut serializer = QueryResultsSerializer::from_format(format)
                .serialize_solutions_to_writer(Vec::new(), variables)
                .map_err(io)?;
            for solution in solutions {
                let solution = solution.map_err(|e| Error::Endpoint(format!("query: {e}")))?;
                serializer.serialize(&solution).map_err(io)?;
            }
            Ok((
                format.media_type().to_string(),
                serializer.finish().map_err(io)?,
            ))
        }
        QueryResults::Boolean(value) => {
            let format = results_format_or_default(as_type)?;
            let bytes = QueryResultsSerializer::from_format(format)
                .serialize_boolean_to_writer(Vec::new(), value)
                .map_err(io)?;
            Ok((format.media_type().to_string(), bytes))
        }
        QueryResults::Graph(triples) => {
            let format = graph_format_or_default(as_type)?;
            let mut serializer = RdfSerializer::from_format(format).for_writer(Vec::new());
            for triple in triples {
                let triple = triple.map_err(|e| Error::Endpoint(format!("query: {e}")))?;
                serializer
                    .serialize_quad(&triple.in_graph(GraphName::DefaultGraph))
                    .map_err(io)?;
            }
            Ok((
                format.media_type().to_string(),
                serializer.finish().map_err(io)?,
            ))
        }
    }
}

/// The SELECT/ASK format for an `as` the caller either gave or did not.
///
/// **A bound must refuse, not substitute.** Absent an `as=` this picks the declared
/// default; given one it cannot serve, it *errors* rather than quietly answering in some
/// other format. Substituting made the declared `outputs` list true by accident instead of
/// by contract — `as=text/turtle` on a SELECT returned JSON, and a typo (`as=jsno`) came
/// back as a plausible answer in the wrong syntax with nothing said. `urn:rdf:transrept`
/// has always refused the same way; these two arms were the outliers.
fn results_format_or_default(as_type: Option<&str>) -> Result<QueryResultsFormat> {
    let Some(spec) = as_type else {
        return Ok(QueryResultsFormat::Json);
    };
    results_format(Some(spec)).ok_or_else(|| {
        Error::Endpoint(format!(
            "SPARQL SELECT/ASK: unknown target `{spec}` — try \
             application/sparql-results+json, application/sparql-results+xml, text/csv, \
             or text/tab-separated-values (short aliases json, xml, csv, tsv are accepted). \
             A graph syntax is not one of them: only CONSTRUCT and DESCRIBE answer with a graph"
        ))
    })
}

/// The CONSTRUCT/DESCRIBE format for an `as` the caller either gave or did not — the graph
/// half of [`results_format_or_default`], refusing on the same terms and for the same reason.
fn graph_format_or_default(as_type: Option<&str>) -> Result<RdfFormat> {
    let Some(spec) = as_type else {
        return Ok(RdfFormat::Turtle);
    };
    rdf_format(spec).ok_or_else(|| {
        Error::Endpoint(format!(
            "SPARQL CONSTRUCT/DESCRIBE: unknown target `{spec}` — try text/turtle, \
             application/n-triples, application/n-quads, application/trig, \
             application/rdf+xml, or application/ld+json (short aliases ttl, nt, nq, trig, \
             rdfxml, jsonld are accepted). A results syntax is not one of them: only SELECT \
             and ASK answer with a result set"
        ))
    })
}

/// SELECT/ASK result format from an `as` media type or short alias.
fn results_format(as_type: Option<&str>) -> Option<QueryResultsFormat> {
    let media = media_base(as_type?);
    if let Some(format) = QueryResultsFormat::from_media_type(media) {
        return Some(format);
    }
    Some(match media {
        "json" => QueryResultsFormat::Json,
        "xml" => QueryResultsFormat::Xml,
        "csv" => QueryResultsFormat::Csv,
        "tsv" => QueryResultsFormat::Tsv,
        _ => return None,
    })
}

/// RDF format (for CONSTRUCT/DESCRIBE output and for loading a source) from a media type
/// or short alias.
fn rdf_format(spec: &str) -> Option<RdfFormat> {
    let media = media_base(spec);
    if let Some(format) = RdfFormat::from_media_type(media) {
        return Some(format);
    }
    Some(match media {
        "turtle" | "ttl" => RdfFormat::Turtle,
        "ntriples" | "nt" | "n-triples" => RdfFormat::NTriples,
        "nquads" | "nq" | "n-quads" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdfxml" | "rdf/xml" | "xml" => RdfFormat::RdfXml,
        "jsonld" | "json-ld" | "json" => {
            RdfFormat::from_media_type("application/ld+json").expect("ld+json")
        }
        _ => return None,
    })
}

/// The bare media type (strip parameters and surrounding whitespace).
fn media_base(media: &str) -> &str {
    media.split(';').next().unwrap_or(media).trim()
}

/// Sniff an input graph's syntax when its content-type isn't a known RDF media type:
/// `{`/`[` ⇒ JSON-LD; a leading IRI `<scheme:…>` ⇒ Turtle; a leading XML element ⇒
/// RDF/XML; else Turtle (subsumes N-Triples). Same discriminator as ikigai-rdf.
fn sniff(bytes: &[u8]) -> RdfFormat {
    let rest = &bytes[bytes.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
    match rest.first() {
        Some(b'{') | Some(b'[') => {
            RdfFormat::from_media_type("application/ld+json").expect("ld+json")
        }
        Some(b'<') if angle_token_is_iri(rest) => RdfFormat::Turtle,
        Some(b'<') => RdfFormat::RdfXml,
        _ => RdfFormat::Turtle,
    }
}

/// How many opening bytes the `<…>` test scans for the closing bracket.
const ANGLE_SCAN: usize = 2048;

/// For a leading `<…>` token, whether it is a Turtle/N-Triples IRI subject rather than an
/// XML start tag — the whole of the distinction, since both syntaxes open with `<`.
///
/// The test used to be "does the token contain `://`?", which asks about an *authority*,
/// not a scheme — so every `urn:` IRI (the scheme ikigai names all of its resources with)
/// read as an element tag, and a Turtle document went to the RDF/XML parser to die as
/// `Unknown prefix urn:`. Three cheap signals replace it, all on the bracketed token:
///
/// 1. it closes within the scan window with no character Turtle's `IRIREF` production
///    forbids before the `>` — which rules out every start tag carrying attributes, plus
///    `<?xml …?>` and `<!DOCTYPE …>`;
/// 2. the text before its first `:` is a URI scheme ([`is_uri_scheme`]) with something
///    after the colon — `<doc>` and `<html>` carry no scheme at all;
/// 3. it is not *also* a well-formed XML QName ([`is_xml_qname`]). This is the tie-break
///    `://` was standing in for: an XML name admits only letters, digits, `.`, `-`, `_`
///    and the one prefix colon, so an attribute-less `<rdf:RDF>` satisfies (1) and (2) and
///    is resolved as markup, while `urn:demo:a`'s second colon and `http://ex/a`'s slashes
///    cannot occur in one.
///
/// The residue is a single-colon, all-name-character IRI such as `<urn:x>`, which reads as
/// a tag: nothing in the token separates the two shapes, and ikigai's own URNs are
/// `urn:nid:nss`. Kept in step with the copies in `ikigai-sniff` and `ikigai-rdf`.
fn angle_token_is_iri(rest: &[u8]) -> bool {
    let window = &rest[1..rest.len().min(ANGLE_SCAN)];
    let Some(end) = window.iter().position(|&b| b == b'>') else {
        return false; // unterminated within the window: not an IRI reference
    };
    let token = &window[..end];
    if !token.iter().all(|&b| is_iri_char(b)) {
        return false;
    }
    let Some(colon) = token.iter().position(|&b| b == b':') else {
        return false;
    };
    let (scheme, after_scheme) = token.split_at(colon);
    is_uri_scheme(scheme) && after_scheme.len() > 1 && !is_xml_qname(token)
}

/// Whether a byte may appear inside a Turtle `IRIREF`. The production excludes the ASCII
/// control range, the space, and ``"<>\^`{|}`` — the same set RFC 3987 keeps out of an IRI.
/// (A single quote is *not* excluded; an attribute written with them, `xmlns='…'`, is
/// caught by the space in front of it.)
fn is_iri_char(b: u8) -> bool {
    !(b <= 0x20
        || b == 0x7F
        || matches!(
            b,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        ))
}

/// Whether the bytes are a URI scheme: `[A-Za-z][A-Za-z0-9+.-]*` (RFC 3986 §3.1).
fn is_uri_scheme(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b) if b.is_ascii_alphabetic())
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Whether the token is *also* a well-formed XML QName — `prefix:local`, where both parts
/// are XML names — which is the shape an RDF/XML root tag takes when it carries no
/// attributes (`<rdf:RDF>`). ASCII-only: a non-ASCII name character puts the token outside
/// the overlap this has to arbitrate, and [`is_iri_char`] has already admitted it.
fn is_xml_qname(token: &[u8]) -> bool {
    token.iter().filter(|&&b| b == b':').count() == 1
        && token
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"@prefix ex: <http://ex/> . ex:a ex:name "Ada" ; ex:knows ex:b ."#;
    const B: &str = r#"@prefix ex: <http://ex/> . ex:b ex:name "Bob" ."#;

    /// Test helper: load named graphs from inline Turtle, run a query, return (media, body).
    /// Pre-seeds the bundled vocabulary exactly as production does (`store_with_vocabulary`).
    fn run(graphs: &[(&str, &str)], query: &str, as_type: Option<&str>) -> (String, String) {
        let store = store_with_vocabulary().unwrap();
        for (uri, ttl) in graphs {
            store
                .load_from_slice(
                    RdfParser::from_format(RdfFormat::Turtle)
                        .with_default_graph(NamedNodeRef::new(uri).unwrap()),
                    ttl.as_bytes(),
                )
                .unwrap();
        }
        let results = evaluate(query, &store).unwrap();
        let (media, bytes) = serialize_results(results, as_type).unwrap();
        (media, String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn a_urn_subject_is_sniffed_as_turtle_not_rdfxml() {
        // A `graph=` source that arrives without a known RDF media type is sniffed here,
        // so the same `://` bug landed a `urn:`-named graph in the RDF/XML parser. Every
        // ikigai resource is named `urn:…`, which is exactly the shape it got wrong.
        assert_eq!(
            sniff(b"<urn:demo:a> <urn:demo:p> <urn:demo:b> .\n"),
            RdfFormat::Turtle
        );
        assert_eq!(sniff(b"<http://ex/a> <http://ex/p> 1 ."), RdfFormat::Turtle);
        assert_eq!(
            sniff(b"@prefix ex: <urn:demo:> . ex:a ex:p 1 ."),
            RdfFormat::Turtle
        );
        assert_eq!(
            sniff(br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#),
            RdfFormat::RdfXml
        );
        assert_eq!(sniff(b"<rdf:RDF></rdf:RDF>"), RdfFormat::RdfXml);
        assert_eq!(sniff(br#"{"@id":"urn:demo:a"}"#).to_string(), "JSON-LD");
    }

    #[test]
    fn a_urn_named_graph_is_queryable_when_its_media_type_is_unknown() {
        // End to end through the same path production takes: an untyped source, sniffed,
        // loaded as a named graph, queried. Before the fix the load failed outright.
        let store = store_with_vocabulary().unwrap();
        let ttl = "<urn:demo:a> <urn:demo:p> \"Ada\" .\n";
        assert_eq!(sniff(ttl.as_bytes()), RdfFormat::Turtle);
        store
            .load_from_slice(
                RdfParser::from_format(sniff(ttl.as_bytes()))
                    .with_default_graph(NamedNodeRef::new("urn:demo:graph").unwrap()),
                ttl.as_bytes(),
            )
            .unwrap();
        let results = evaluate(
            "SELECT ?o WHERE { GRAPH <urn:demo:graph> { <urn:demo:a> <urn:demo:p> ?o } }",
            &store,
        )
        .unwrap();
        let (_, bytes) = serialize_results(results, Some("text/csv")).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("Ada"));
    }

    /// Every bound `urn:sparql:{verb}` must project a DISTINCT description id (and name) —
    /// else all four collide to one catalog subject and one MCP tool (the 4× dupe that F4
    /// fixed). Guards against the single-constant id regressing back in.
    #[test]
    fn each_form_has_a_unique_id_and_name() {
        let endpoints: Vec<SparqlEndpoint> = FORMS
            .iter()
            .map(|(verb, id)| SparqlEndpoint {
                verb,
                id,
                shared: None,
            })
            .collect();
        // describe().id and name() agree, and all four are distinct.
        let ids: Vec<String> = endpoints.iter().map(|e| e.describe().id).collect();
        for (e, id) in endpoints.iter().zip(&ids) {
            assert_eq!(e.name(), id, "name() must match describe().id");
        }
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "sparql form ids collide: {ids:?}");
    }

    #[test]
    fn graph_and_as_are_optional_only_query_is_required() {
        // Over MCP the required list becomes the inputSchema `required` — `graph` and `as`
        // both have working defaults, so only `query` may be required (F5).
        let desc = SparqlEndpoint {
            verb: "select",
            id: "sparql-select",
            shared: None,
        }
        .describe();
        let required: Vec<&str> = desc
            .inputs
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(
            required,
            vec!["query"],
            "only query is required: {required:?}"
        );
    }

    #[test]
    fn select_over_a_graph_returns_json_solutions() {
        let (media, body) = run(
            &[("http://g/a", A)],
            "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
            None,
        );
        assert!(media.contains("sparql-results+json"));
        assert!(body.contains("Ada"));
    }

    #[test]
    fn ask_returns_a_boolean() {
        let (_, yes) = run(
            &[("http://g/a", A)],
            "ASK { ?s <http://ex/name> \"Ada\" }",
            None,
        );
        assert!(yes.contains("true"));
        let (_, no) = run(
            &[("http://g/a", A)],
            "ASK { ?s <http://ex/name> \"Nope\" }",
            None,
        );
        assert!(no.contains("false"));
    }

    /// Like [`run`], but hands back the `Result` so a refusal is observable.
    fn try_run(graphs: &[(&str, &str)], query: &str, as_type: Option<&str>) -> Result<String> {
        let store = store_with_vocabulary().unwrap();
        for (uri, ttl) in graphs {
            store
                .load_from_slice(
                    RdfParser::from_format(RdfFormat::Turtle)
                        .with_default_graph(NamedNodeRef::new(uri).unwrap()),
                    ttl.as_bytes(),
                )
                .unwrap();
        }
        let results = evaluate(query, &store).unwrap();
        serialize_results(results, as_type).map(|(media, _)| media)
    }

    #[test]
    fn an_unknown_as_is_refused_not_substituted() {
        // A bound must refuse, not substitute. Each of these used to serialize in the
        // default format and say nothing, which made a typo indistinguishable from a
        // deliberate choice and the declared `outputs` list true only by accident.
        let graphs = &[("http://g/a", A)][..];
        let select = "SELECT ?name WHERE { ?s <http://ex/name> ?name }";
        let ask = "ASK { ?s <http://ex/name> \"Ada\" }";
        let construct = "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }";

        for (query, bad) in [(select, "jsno"), (ask, "application/nonsense")] {
            let err = format!("{}", try_run(graphs, query, Some(bad)).unwrap_err());
            assert!(
                err.contains("unknown target") && err.contains(bad),
                "SELECT/ASK must name the target it refused: {err}"
            );
        }
        let err = format!("{}", try_run(graphs, construct, Some("ttl2")).unwrap_err());
        assert!(
            err.contains("unknown target") && err.contains("ttl2"),
            "{err}"
        );
    }

    #[test]
    fn a_cross_form_as_is_refused_rather_than_silently_ignored() {
        // The sharpest case, because it looks like a legitimate request: a graph syntax
        // asked of SELECT, or a results syntax asked of CONSTRUCT. Both are well-formed
        // media types the *other* form serves, and both used to come back in the default
        // format — the caller's `as=` silently dropped on the floor.
        let graphs = &[("http://g/a", A)][..];
        let err = format!(
            "{}",
            try_run(
                graphs,
                "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
                Some("text/turtle"),
            )
            .unwrap_err()
        );
        assert!(err.contains("CONSTRUCT and DESCRIBE"), "{err}");
        let err = format!(
            "{}",
            try_run(
                graphs,
                "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }",
                Some("application/sparql-results+json"),
            )
            .unwrap_err()
        );
        assert!(err.contains("SELECT and ASK"), "{err}");
    }

    #[test]
    fn every_declared_output_is_actually_servable() {
        // The other direction of the same contract: refusing an unknown `as=` is only
        // honest if everything the manifold announces is accepted. Walks both declared
        // lists through the real serializer.
        let graphs = &[("http://g/a", A)][..];
        for media in RESULTS_OUTPUTS {
            let got = try_run(
                graphs,
                "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
                Some(media),
            )
            .unwrap_or_else(|e| panic!("declared output `{media}` is refused: {e}"));
            assert_eq!(media_base(&got), media, "served what was asked for");
        }
        for media in GRAPH_OUTPUTS {
            let got = try_run(
                graphs,
                "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }",
                Some(media),
            )
            .unwrap_or_else(|e| panic!("declared output `{media}` is refused: {e}"));
            assert_eq!(media_base(&got), media, "served what was asked for");
        }
    }

    #[test]
    fn an_absent_as_still_takes_the_declared_default() {
        // Refusing must not swallow the no-`as=` case: absent an `as=`, each form serves
        // the first entry of its own declared list, which is what `default_value` promises.
        let graphs = &[("http://g/a", A)][..];
        assert_eq!(
            media_base(
                &try_run(
                    graphs,
                    "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
                    None
                )
                .unwrap()
            ),
            RESULTS_OUTPUTS[0]
        );
        assert_eq!(
            media_base(
                &try_run(
                    graphs,
                    "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }",
                    None,
                )
                .unwrap()
            ),
            GRAPH_OUTPUTS[0]
        );
    }

    #[test]
    fn construct_emits_rdf_for_transreption() {
        let (media, ttl) = run(
            &[("http://g/a", A)],
            "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }",
            Some("text/turtle"),
        );
        assert!(media.contains("turtle"));
        assert!(ttl.contains("http://ex/label"));
        assert!(ttl.contains("Ada"));
    }

    #[test]
    fn federation_unions_graphs_yet_keeps_them_addressable() {
        // The union default graph spans both sources…
        let (_, both) = run(
            &[("http://g/a", A), ("http://g/b", B)],
            "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
            Some("text/csv"),
        );
        assert!(
            both.contains("Ada") && both.contains("Bob"),
            "union default spans graphs"
        );
        // …and each graph stays addressable by its URI.
        let (_, only_b) = run(
            &[("http://g/a", A), ("http://g/b", B)],
            "SELECT ?name WHERE { GRAPH <http://g/b> { ?s <http://ex/name> ?name } }",
            Some("text/csv"),
        );
        assert!(
            only_b.contains("Bob") && !only_b.contains("Ada"),
            "named graph isolates"
        );
    }

    /// A catalog-like graph holds endpoint *instances*; the always-loaded vocabulary holds
    /// the `ik:Transreptor rdfs:subClassOf ik:Endpoint` axiom. A property-path query joins
    /// them — finding the transreptor as an ik:Endpoint with no reasoner and no extra graph.
    const CATALOG: &str = r#"@prefix ik: <https://ikigai-rs.dev/ns#> .
        <urn:fn:toUpper>     a ik:Endpoint ; ik:id "toUpper" .
        <urn:rdf:transrept>  a ik:Endpoint, ik:Transreptor ; ik:id "rdf-transrept" ;
                             ik:transreptsFrom "text/turtle" ; ik:transreptsTo "text/html" ."#;

    #[test]
    fn vocabulary_is_always_present_for_schema_queries() {
        // No `graph=` at all: the bundled vocabulary alone answers a schema question.
        let (_, body) = run(
            &[],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             ASK { ik:Transreptor rdfs:subClassOf ik:Endpoint }",
            None,
        );
        assert!(
            body.contains("true"),
            "vocab carries the subClassOf axiom: {body}"
        );
    }

    #[test]
    fn subclass_path_finds_transreptors_as_endpoints() {
        // Walk rdf:type/rdfs:subClassOf*: every endpoint, transreptors included, via the
        // axiom from the always-present vocabulary joined to the catalog instances.
        let (_, all) = run(
            &[("urn:kernel:catalog", CATALOG)],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             SELECT ?id WHERE { ?e a/rdfs:subClassOf* ik:Endpoint ; ik:id ?id }",
            Some("text/csv"),
        );
        assert!(all.contains("toUpper"), "plain endpoint: {all}");
        assert!(
            all.contains("rdf-transrept"),
            "transreptor counts as an endpoint: {all}"
        );

        // And the transreptor is selectable by its declared conversion.
        let (_, html_producers) = run(
            &[("urn:kernel:catalog", CATALOG)],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> \
             SELECT ?id WHERE { ?e a ik:Transreptor ; ik:id ?id ; ik:transreptsTo \"text/html\" }",
            Some("text/csv"),
        );
        assert!(
            html_producers.contains("rdf-transrept") && !html_producers.contains("toUpper"),
            "only the transreptor that produces text/html: {html_producers}"
        );
    }

    /// The shared-store seam (`space_with_store`): one caller-owned `Arc<Store>` written
    /// through its raw handle, queried live through `urn:sparql:*`.
    mod shared_store {
        use super::super::*;
        use futures::executor::block_on;
        use ikigai_core::{ArgRef, Capability, Expiry, Kernel, Request};
        use oxigraph::model::{GraphName, Literal, NamedNode, Quad};

        fn name_quad(subject: &str, name: &str, graph: GraphName) -> Quad {
            Quad::new(
                NamedNode::new(subject).unwrap(),
                NamedNode::new("http://ex/name").unwrap(),
                Literal::new_simple_literal(name),
                graph,
            )
        }

        fn kernel_over(store: &Arc<Store>) -> Kernel {
            Kernel::new(Arc::new(space_with_store(Arc::clone(store))))
        }

        fn issue(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<Representation> {
            let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
            for (k, v) in args {
                request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
            }
            block_on(kernel.issue(request, &Capability::root()))
        }

        fn select_names(kernel: &Kernel) -> String {
            let out = issue(
                kernel,
                "urn:sparql:select",
                &[
                    ("query", "SELECT ?name WHERE { ?s <http://ex/name> ?name }"),
                    ("as", "text/csv"),
                ],
            )
            .unwrap();
            String::from_utf8(out.bytes).unwrap()
        }

        #[test]
        fn raw_handle_writes_are_visible_through_the_endpoint() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            // Live freshness, not a cache: the same query straddles the write.
            assert!(!select_names(&kernel).contains("Ada"), "starts empty");
            store
                .insert(&name_quad("http://ex/a", "Ada", GraphName::DefaultGraph))
                .unwrap();
            assert!(select_names(&kernel).contains("Ada"), "write is visible");

            // A named-graph write is visible too (union default graph)…
            let annotations = NamedNode::new("urn:test:annotations").unwrap();
            store
                .insert(&name_quad("http://ex/b", "Bob", annotations.into()))
                .unwrap();
            let names = select_names(&kernel);
            assert!(names.contains("Ada") && names.contains("Bob"), "{names}");

            // …and the named graph stays individually addressable.
            let out = issue(
                &kernel,
                "urn:sparql:select",
                &[
                    (
                        "query",
                        "SELECT ?name WHERE { GRAPH <urn:test:annotations> { ?s <http://ex/name> ?name } }",
                    ),
                    ("as", "text/csv"),
                ],
            )
            .unwrap();
            let only = String::from_utf8(out.bytes).unwrap();
            assert!(only.contains("Bob") && !only.contains("Ada"), "{only}");
        }

        #[test]
        fn two_spaces_over_one_arc_see_each_others_writes() {
            // Two independent hosts (kernels) over ONE Arc: the store is shared state,
            // not copied per space — a write lands in both views.
            let store = Arc::new(Store::new().unwrap());
            let (a, b) = (kernel_over(&store), kernel_over(&store));
            store
                .insert(&name_quad("http://ex/a", "Ada", GraphName::DefaultGraph))
                .unwrap();
            assert!(select_names(&a).contains("Ada"), "space A sees the write");
            assert!(select_names(&b).contains("Ada"), "space B sees the write");
        }

        #[test]
        fn results_over_a_live_store_are_uncacheable() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            let out = issue(&kernel, "urn:sparql:ask", &[("query", "ASK { ?s ?p ?o }")]).unwrap();
            // Raw-handle writes carry no golden thread, so a cached result could never
            // be invalidated: the representation must stay Expiry::Always.
            assert!(
                matches!(out.expiry, Expiry::Always),
                "live shared store must not cache: {:?}",
                out.expiry
            );
        }

        #[test]
        fn graph_arg_is_rejected_and_not_offered() {
            // Runtime: loading a source would mutate the caller-owned store.
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            let err = issue(
                &kernel,
                "urn:sparql:select",
                &[
                    ("query", "SELECT * WHERE { ?s ?p ?o }"),
                    ("graph", "http://ex/some.ttl"),
                ],
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("shared store"),
                "clear rejection: {err}"
            );

            // Manifold: the shared-store describe() must not offer `graph=` (over-offer).
            let desc = SparqlEndpoint {
                verb: "select",
                id: "sparql-select",
                shared: Some(Arc::clone(&store)),
            }
            .describe();
            assert!(
                !desc.inputs.iter().any(|a| a.name == "graph"),
                "shared-store manifold must not offer graph="
            );
        }

        #[test]
        fn nothing_is_autoloaded_and_vocabulary_is_opt_in() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            let schema_ask = &[(
                "query",
                "PREFIX ik: <https://ikigai-rs.dev/ns#> \
                 PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                 ASK { ik:Transreptor rdfs:subClassOf ik:Endpoint }",
            )][..];

            // The caller owns the store's contents: no vocabulary sneaks in.
            let before = issue(&kernel, "urn:sparql:ask", schema_ask).unwrap();
            assert!(String::from_utf8(before.bytes).unwrap().contains("false"));

            // Opt-in via load_vocabulary, visible immediately (live store).
            load_vocabulary(&store).unwrap();
            let after = issue(&kernel, "urn:sparql:ask", schema_ask).unwrap();
            assert!(String::from_utf8(after.bytes).unwrap().contains("true"));
        }
    }

    /// `urn:sparql:update` — the first write in this crate that goes THROUGH the kernel,
    /// and the golden-thread question that forces (see [`UPDATE_THREAD`]).
    mod update {
        use super::super::*;
        use futures::executor::block_on;
        use ikigai_core::{ArgRef, Capability, Expiry, Kernel, Request};
        use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
        use std::sync::atomic::{AtomicUsize, Ordering};

        const INSERT_ADA: &str = r#"INSERT DATA { <http://ex/a> <http://ex/name> "Ada" }"#;
        const NAMES: &str = "SELECT ?n WHERE { ?s <http://ex/name> ?n }";

        fn kernel_over(store: &Arc<Store>) -> Kernel {
            Kernel::new(Arc::new(space_with_store(Arc::clone(store))))
        }

        fn issue_as(
            kernel: &Kernel,
            verb: Verb,
            iri: &str,
            args: &[(&str, &str)],
            cap: &Capability,
        ) -> Result<Representation> {
            let mut request = Request::new(verb, Iri::parse(iri).unwrap());
            for (k, v) in args {
                request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
            }
            block_on(kernel.issue(request, cap))
        }

        /// Sink an update as root; return the receipt line.
        fn update(kernel: &Kernel, text: &str) -> Result<String> {
            issue_as(
                kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", text)],
                &Capability::root(),
            )
            .map(|r| String::from_utf8(r.bytes).unwrap())
        }

        /// Read back THROUGH THE KERNEL — never the raw handle. That is the whole point
        /// of `update_then_select_sees_the_change_through_the_kernel`.
        fn select_csv(kernel: &Kernel, query: &str) -> String {
            let out = issue_as(
                kernel,
                Verb::Source,
                "urn:sparql:select",
                &[("query", query), ("as", "text/csv")],
                &Capability::root(),
            )
            .unwrap();
            String::from_utf8(out.bytes).unwrap()
        }

        #[test]
        fn update_then_select_sees_the_change_through_the_kernel() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            assert!(!select_csv(&kernel, NAMES).contains("Ada"), "starts empty");
            update(&kernel, INSERT_ADA).unwrap();
            assert!(
                select_csv(&kernel, NAMES).contains("Ada"),
                "an INSERT sunk through the kernel is visible to a SELECT through the kernel"
            );

            // …and it round-trips: the delete is a kernel write too.
            update(
                &kernel,
                "DELETE WHERE { <http://ex/a> <http://ex/name> ?n }",
            )
            .unwrap();
            assert!(!select_csv(&kernel, NAMES).contains("Ada"), "DELETE lands");
        }

        #[test]
        fn update_writes_into_a_named_graph_and_select_finds_it() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            update(
                &kernel,
                r#"INSERT DATA { GRAPH <urn:test:g> { <http://ex/b> <http://ex/name> "Bob" } }"#,
            )
            .unwrap();
            // The query endpoints union the default graph, so a plain SELECT sees it…
            assert!(select_csv(&kernel, NAMES).contains("Bob"));
            // …and it is still individually addressable.
            let only = select_csv(
                &kernel,
                "SELECT ?n WHERE { GRAPH <urn:test:g> { ?s <http://ex/name> ?n } }",
            );
            assert!(only.contains("Bob"), "{only}");
        }

        /// ★ The gate — and it has been watched to fail, not assumed to work.
        ///
        /// Ablation performed 2026-09-04: delete `.requires(CAP_UPDATE)` from
        /// `SparqlUpdateEndpoint::describe` and this test does not merely lose an error
        /// string — the unauthorized `Capability::scoped(["urn:cap:sparql:query"])`
        /// **succeeds**, returning `updated: 0 -> 1 quads`. An ungated write actually
        /// lands. `the_sink_action_declares_the_scope_the_kernel_enforces` fails in the
        /// same run, from the manifold's side.
        ///
        /// That is the shape of the enforcement: the declaration is the ONLY input to
        /// `Kernel::issue`'s `enforce_requires`, which runs before `invoke` and before
        /// the cache lookup. There is no second, redundant runtime check inside
        /// `invoke` — deliberately, since the scope is coarse and unparameterized (an
        /// ACL-style module like ikigai-fs keeps a finer runtime ceiling on top because
        /// its policy depends on arguments; this one's does not). One consequence worth
        /// stating: a host that invokes this endpoint OUTSIDE a kernel, via
        /// `Invocation::detached`, gets no gate at all.
        #[test]
        fn a_caller_without_the_capability_is_denied_permanently() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            let err = issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", INSERT_ADA)],
                // A capability that can read but not write.
                &Capability::scoped(["urn:cap:sparql:query"]),
            )
            .unwrap_err();

            assert!(
                matches!(err, Error::Denied(_)),
                "a missing capability must be Denied, not a generic endpoint error: {err:?}"
            );
            assert!(
                !err.is_transient(),
                "a denial is PERMANENT — retrying the same request must never be advised"
            );
            assert!(
                err.to_string().contains(CAP_UPDATE),
                "the denial names the scope it wanted: {err}"
            );
            assert_eq!(
                store.len().unwrap(),
                0,
                "a denied update must not have touched the store"
            );

            // The scope itself is sufficient — the gate refuses the caller, not everyone.
            issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", INSERT_ADA)],
                &Capability::scoped([CAP_UPDATE]),
            )
            .unwrap();
            assert_eq!(store.len().unwrap(), 1, "the granted caller wrote");
        }

        /// Declared = enforced, from the manifold's side: the Sink action the catalog
        /// projects must carry the same scope the kernel enforces. If these ever drift,
        /// the manifold lies about what a capability can do.
        #[test]
        fn the_sink_action_declares_the_scope_the_kernel_enforces() {
            let store = Arc::new(Store::new().unwrap());
            let desc = SparqlUpdateEndpoint { store }.describe();
            let sink: Vec<_> = desc
                .action_specs()
                .into_iter()
                .filter(|a| a.verb == Verb::Sink)
                .collect();
            assert_eq!(sink.len(), 1, "exactly one Sink action");
            assert_eq!(
                sink[0].requires,
                vec![CAP_UPDATE.to_string()],
                "the projected action's requires IS the enforced floor"
            );
            assert_eq!(
                desc.id, "sparql-update",
                "distinct catalog subject / tool name"
            );
        }

        #[test]
        fn a_malformed_update_applies_nothing() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            // A syntax error anywhere fails the whole text at parse time.
            let err = update(&kernel, "INSERT DATA { <http://ex/a> <http://ex/name> ").unwrap_err();
            assert!(err.to_string().contains("syntax error"), "{err}");
            assert_eq!(store.len().unwrap(), 0, "nothing applied");

            // …including when a VALID operation precedes the broken one, which is the
            // case that actually matters: a half-applied rename is worse than none.
            let err = update(
                &kernel,
                &format!("{INSERT_ADA} ; INSERT DATA {{ <http://ex/b> ??? }}"),
            )
            .unwrap_err();
            assert!(err.to_string().contains("syntax error"), "{err}");
            assert_eq!(
                store.len().unwrap(),
                0,
                "the leading valid operation must NOT have been applied"
            );

            // And a store that already holds data is left exactly as it was.
            update(&kernel, INSERT_ADA).unwrap();
            let before = store.len().unwrap();
            assert!(update(&kernel, "DELETE WHERE { ?s ?p").is_err());
            assert_eq!(
                store.len().unwrap(),
                before,
                "unchanged after a failed update"
            );
        }

        /// The harder half of "fails cleanly": an update whose operations all PARSE, and
        /// which fails partway through EVALUATION. `LOAD` is the reachable trigger — this
        /// build has oxigraph's `http-client` off on purpose (see [`SparqlUpdateEndpoint`])
        /// — and it proves the transaction, not just the parser: the `INSERT DATA` that
        /// already ran is rolled back, not committed.
        #[test]
        fn a_mid_update_failure_rolls_the_whole_transaction_back() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            update(&kernel, INSERT_ADA).unwrap();

            let err = update(
                &kernel,
                r#"INSERT DATA { <http://ex/b> <http://ex/name> "Bob" } ;
                   LOAD <http://example.invalid/g>"#,
            )
            .unwrap_err();
            assert!(err.to_string().contains("evaluation error"), "{err}");
            assert!(
                !select_csv(&kernel, NAMES).contains("Bob"),
                "the operation that SUCCEEDED before the failure must be rolled back"
            );
            assert!(
                select_csv(&kernel, NAMES).contains("Ada"),
                "and Ada survives"
            );
            assert_eq!(store.len().unwrap(), 1, "exactly the pre-update store");
        }

        #[test]
        fn graph_is_refused_and_not_offered() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            let err = issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", INSERT_ADA), ("graph", "http://ex/some.ttl")],
                &Capability::root(),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("`graph=` is not supported"),
                "a clean refusal, not a silent ignore: {err}"
            );
            assert_eq!(store.len().unwrap(), 0, "refused before writing");

            // The manifold must not offer it either.
            let desc = SparqlUpdateEndpoint {
                store: Arc::clone(&store),
            }
            .describe();
            assert!(
                !desc.inputs.iter().any(|a| a.name == "graph"),
                "urn:sparql:update must not advertise graph="
            );
        }

        /// `space()` builds and drops a dataset per query, so an update against it would
        /// be a silent no-op. It is unbound there instead — an honest resolution failure.
        #[test]
        fn the_per_query_space_does_not_bind_update() {
            let kernel = Kernel::new(Arc::new(space()));
            let err = issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", INSERT_ADA)],
                &Capability::root(),
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::Unresolved(_)),
                "unbound, not a no-op write: {err:?}"
            );
        }

        #[test]
        fn the_update_text_can_arrive_as_piped_content() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("content", INSERT_ADA)],
                &Capability::root(),
            )
            .unwrap();
            assert!(select_csv(&kernel, NAMES).contains("Ada"));

            // Neither argument at all is a MissingArgument, not a silent success.
            let err = issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[],
                &Capability::root(),
            )
            .unwrap_err();
            assert!(matches!(err, Error::MissingArgument(_)), "{err:?}");
        }

        /// The receipt line's shape, pinned: it leaves the process, so a test owns it
        /// rather than the doc comment.
        #[test]
        fn update_reports_the_quad_delta_and_is_uncacheable() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            assert_eq!(
                update(&kernel, INSERT_ADA).unwrap(),
                "updated: 0 -> 1 quads\n"
            );
            assert_eq!(
                update(&kernel, "DELETE WHERE { ?s ?p ?o }").unwrap(),
                "updated: 1 -> 0 quads\n"
            );

            let out = issue_as(
                &kernel,
                Verb::Sink,
                "urn:sparql:update",
                &[("update", INSERT_ADA)],
                &Capability::root(),
            )
            .unwrap();
            assert!(matches!(out.expiry, Expiry::Always), "{:?}", out.expiry);
        }

        /// ⚠ Query semantics union the graphs; update semantics do not. Documented on
        /// [`SparqlUpdateEndpoint`], pinned here so the asymmetry cannot drift silently
        /// into "SELECT and DELETE disagree and nobody wrote it down".
        #[test]
        fn update_uses_plain_dataset_semantics_not_the_query_union() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            update(
                &kernel,
                r#"INSERT DATA { GRAPH <urn:test:g> { <http://ex/b> <http://ex/name> "Bob" } }"#,
            )
            .unwrap();

            // SELECT sees it (union default graph)…
            assert!(select_csv(&kernel, NAMES).contains("Bob"));
            // …but the identically-worded DELETE does not touch it: the update's default
            // graph is the store's real default graph.
            update(&kernel, "DELETE WHERE { ?s ?p ?o }").unwrap();
            assert!(
                select_csv(&kernel, NAMES).contains("Bob"),
                "a default-graph DELETE must not reach into named graphs"
            );
            // Naming the graph is how you reach it.
            update(&kernel, "DELETE WHERE { GRAPH ?g { ?s ?p ?o } }").unwrap();
            assert!(!select_csv(&kernel, NAMES).contains("Bob"));
        }

        // --- the golden-thread question -------------------------------------------

        /// A cacheable reader that declares [`UPDATE_THREAD`] — standing in for what a
        /// shared-store SELECT would become the day every writer to a store goes through
        /// the kernel. It counts its own invocations, so a cache hit is observable.
        struct Probe(Arc<AtomicUsize>);

        #[async_trait]
        impl Endpoint for Probe {
            async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
                let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(
                    Representation::new(ReprType::new("text/plain"), n.to_string().into_bytes())
                        .cacheable()
                        .depends_on(UPDATE_THREAD),
                )
            }
            fn name(&self) -> &str {
                "probe"
            }
            fn describe(&self) -> Description {
                Description::new("probe").verb(Verb::Source)
            }
        }

        /// ★★ The answer to "does an update cut a thread, and which one", executable.
        ///
        /// First half: it does. Sinking `urn:sparql:update` cuts [`UPDATE_THREAD`] (the
        /// kernel's automatic cut of a mutating request's target), and a cacheable
        /// reader that declared it recomputes. That mechanism did not exist in this
        /// crate before — every write was raw.
        ///
        /// Second half, and the reason shared-store reads STAY uncacheable: a raw
        /// `Arc<Store>` write cuts nothing, so the very same reader goes stale and
        /// stays stale, with no signal. Coverage, not bluntness, is what is missing.
        #[test]
        fn update_cuts_a_thread_but_a_raw_handle_write_does_not() {
            let store = Arc::new(Store::new().unwrap());
            let hits = Arc::new(AtomicUsize::new(0));
            let kernel = Kernel::new(Arc::new(
                space_with_store(Arc::clone(&store))
                    .bind(Exact::new("urn:test:probe"), Probe(Arc::clone(&hits))),
            ));
            let read = |k: &Kernel| {
                let out =
                    issue_as(k, Verb::Source, "urn:test:probe", &[], &Capability::root()).unwrap();
                String::from_utf8(out.bytes).unwrap()
            };

            assert_eq!(read(&kernel), "1", "first read computes");
            assert_eq!(read(&kernel), "1", "second read is a cache hit");

            // A kernel write cuts the thread: the reader recomputes.
            update(&kernel, INSERT_ADA).unwrap();
            assert_eq!(
                read(&kernel),
                "2",
                "urn:sparql:update cut UPDATE_THREAD and the cached reader was invalidated"
            );
            assert_eq!(read(&kernel), "2", "and it re-cached");

            // A RAW write does not. This is the whole argument for keeping shared-store
            // reads uncacheable: the thread would be right here and wrong there.
            store
                .insert(&Quad::new(
                    NamedNode::new("http://ex/b").unwrap(),
                    NamedNode::new("http://ex/name").unwrap(),
                    Literal::new_simple_literal("Bob"),
                    GraphName::DefaultGraph,
                ))
                .unwrap();
            assert_eq!(
                read(&kernel),
                "2",
                "a raw-handle write cuts nothing — a reader depending on this thread \
                 would now be serving a stale answer with no bound and no signal"
            );
        }

        /// The shared-store query endpoints must NOT have quietly become cacheable on the
        /// strength of the cut above. This is the decision itself, machine-checked.
        #[test]
        fn shared_store_reads_stay_uncacheable_despite_the_cut() {
            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);
            let out = issue_as(
                &kernel,
                Verb::Source,
                "urn:sparql:ask",
                &[("query", "ASK { ?s ?p ?o }")],
                &Capability::root(),
            )
            .unwrap();
            assert!(
                matches!(out.expiry, Expiry::Always),
                "raw-handle writers still bypass the kernel: see UPDATE_THREAD. {:?}",
                out.expiry
            );
        }

        // --- the acceptance test --------------------------------------------------

        /// ★ The reason this endpoint exists.
        ///
        /// `ikigai-browse`'s `migrate-annotation-ns` is a one-shot BINARY whose own module
        /// doc says: "when `urn:sparql:update` exists this is one `DELETE … INSERT …
        /// WHERE` against a bound store, and this file should be deleted rather than
        /// generalized." This is that query, run through the kernel over a store shaped
        /// like the real one — including the three things that make the rewrite awkward:
        /// IRIs move in EVERY position (subject, predicate, object and graph name), a
        /// LITERAL that merely quotes the old prefix must NOT move, and the whole thing
        /// must be atomic.
        #[test]
        fn the_browse_namespace_migration_is_now_one_query() {
            const OLD: &str = "urn:annotation:";
            const NEW: &str = "urn:iki:annotation:";

            let store = Arc::new(Store::new().unwrap());
            let kernel = kernel_over(&store);

            // A browse-shaped store: an annotation, its selector (object position), an
            // incoming prov:generated reference (object position, subject does NOT move),
            // a quad living in a graph NAMED under the old prefix, and — the trap — an
            // oa:exact literal that quotes a line of source mentioning the old prefix.
            update(
                &kernel,
                r#"
                PREFIX oa: <http://www.w3.org/ns/oa#>
                PREFIX prov: <http://www.w3.org/ns/prov#>
                INSERT DATA {
                    <urn:annotation:m0> a oa:Annotation ;
                        oa:hasSelector <urn:annotation:m0:sel> ;
                        oa:exact "const OLD_PREFIX: &str = \"urn:annotation:\";" .
                    <urn:review:r0> prov:generated <urn:annotation:m0> .
                    GRAPH <urn:annotation:graph> { <urn:annotation:m1> a oa:Annotation }
                }"#,
            )
            .unwrap();
            let total = store.len().unwrap();

            // ONE update, two operations (default graph, then named graphs), one
            // transaction. `isIRI` is the guard that leaves the literal alone.
            let mv = |v: &str| {
                format!(
                    "IF(isIRI(?{v}) && STRSTARTS(STR(?{v}), \"{OLD}\"), \
                     IRI(CONCAT(\"{NEW}\", STRAFTER(STR(?{v}), \"{OLD}\"))), ?{v})"
                )
            };
            let migration = format!(
                "DELETE {{ ?s ?p ?o }} INSERT {{ ?s2 ?p2 ?o2 }} WHERE {{
                     ?s ?p ?o .
                     BIND({s} AS ?s2) BIND({p} AS ?p2) BIND({o} AS ?o2)
                     FILTER(!sameTerm(?s, ?s2) || !sameTerm(?p, ?p2) || !sameTerm(?o, ?o2))
                 }} ;
                 DELETE {{ GRAPH ?g {{ ?s ?p ?o }} }} INSERT {{ GRAPH ?g2 {{ ?s2 ?p2 ?o2 }} }} WHERE {{
                     GRAPH ?g {{ ?s ?p ?o }}
                     BIND({s} AS ?s2) BIND({p} AS ?p2) BIND({o} AS ?o2) BIND({g} AS ?g2)
                     FILTER(!sameTerm(?s, ?s2) || !sameTerm(?p, ?p2)
                            || !sameTerm(?o, ?o2) || !sameTerm(?g, ?g2))
                 }}",
                s = mv("s"),
                p = mv("p"),
                o = mv("o"),
                g = mv("g"),
            );
            update(&kernel, &migration).unwrap();

            // Nothing under the old prefix survives in an IRI position…
            let leftover = select_csv(
                &kernel,
                &format!(
                    "SELECT ?s WHERE {{ {{ ?s ?p ?o }} UNION {{ GRAPH ?g {{ ?s ?p ?o }} }} \
                     FILTER(STRSTARTS(STR(?s), \"{OLD}\") || STRSTARTS(STR(?p), \"{OLD}\") \
                     || (isIRI(?o) && STRSTARTS(STR(?o), \"{OLD}\"))) }}"
                ),
            );
            assert_eq!(
                leftover.lines().count(),
                1,
                "header only — no IRI left under the old prefix: {leftover}"
            );

            // …the annotation, its selector and the incoming reference all moved together…
            for expected in [
                "urn:iki:annotation:m0",
                "urn:iki:annotation:m0:sel",
                "urn:iki:annotation:m1",
            ] {
                let found = select_csv(
                    &kernel,
                    &format!(
                        "SELECT ?p WHERE {{ {{ <{expected}> ?p ?o }} UNION \
                         {{ GRAPH ?g {{ <{expected}> ?p ?o }} }} UNION \
                         {{ ?x ?p <{expected}> }} }}"
                    ),
                );
                assert!(found.lines().count() > 1, "{expected} is missing: {found}");
            }

            // …the graph NAME moved too…
            let named = select_csv(
                &kernel,
                "SELECT ?s WHERE { GRAPH <urn:iki:annotation:graph> { ?s ?p ?o } }",
            );
            assert!(named.contains("urn:iki:annotation:m1"), "{named}");

            // …the literal that merely QUOTES the old prefix did not move…
            let quoted = select_csv(
                &kernel,
                "PREFIX oa: <http://www.w3.org/ns/oa#> SELECT ?e WHERE { ?a oa:exact ?e }",
            );
            assert!(
                quoted.contains("urn:annotation:"),
                "the quoted source text is data, not an identifier: {quoted}"
            );

            // …and nothing was gained or lost on the way.
            assert_eq!(store.len().unwrap(), total, "quad count preserved");
        }
    }
}
