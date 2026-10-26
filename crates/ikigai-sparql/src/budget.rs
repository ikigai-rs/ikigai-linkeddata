//! A TIME budget on every SPARQL evaluation that parses caller text (ledger #964).
//!
//! [`limits`](crate::limits) keeps a query from overflowing the stack; it says nothing about
//! how long the query may run, and oxigraph is slow well before it is deep. Measured on this
//! workspace's oxigraph (0.5.11, release build, the per-query dataset), a 3 KB property path
//! of 1,000 steps took 19 s of a core and 200 triple patterns 7 s, and a 50-byte cross product
//! (`SELECT * { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }`) runs until it is killed. One
//! anonymous request could pin a core for as long as it liked.
//!
//! Two layers again, because oxigraph's cancellation reaches only part of the work:
//!
//! 1. **A deadline, enforced with oxigraph's [`CancellationToken`].** Every evaluation runs
//!    with a token that a watchdog thread cancels when the budget expires, and this crate's
//!    own serialization loop checks the same token once per row or triple. A query that
//!    crosses the deadline is refused with [`Error::Timeout`] naming the budget — never a
//!    partial answer — and the evaluation thread returns, which is what frees the core.
//! 2. **Bounds on the query's algebra, checked after parsing and before planning.** oxigraph
//!    checks its token only where the evaluator touches the dataset (a quad-pattern scan, a
//!    named-graph scan, internalizing a term). Its query PLANNER touches no dataset and is
//!    super-linear: the greedy join reordering is about cubic in the operands of one join,
//!    and an `OPTIONAL` chain or a long `||` is quadratic. So the expensive part of those
//!    attack shapes cannot be cancelled at all — measured, a 1,000-step path cancelled after
//!    0.5 s returned 18 s later. What bounds the planner is the size of what it is handed:
//!    [`MAX_JOIN_OPERANDS`] in any one join and [`MAX_ALGEBRA_NODES`] in the whole query,
//!    refused with a typed `InvalidArgument` naming the bound, like the stack bounds.
//!
//! `ASK` preserves parsed algebra order rather than applying oxigraph's query optimizations:
//! an optimized filtered cross product over vocabulary 0.2.0 spent seconds between dataset
//! touches, with no rows reaching our serializer to check the token. This improves that
//! shape's cancellation at the cost of optimized ASK planning; the gaps below still apply.
//!
//! # Who sets the budget
//!
//! **The host, in two places, and the caller can only tighten it.**
//!
//! - The space's CEILING is a constructor argument: [`crate::space_with_budget`] and
//!   [`crate::space_with_store_and_budget`]; [`crate::space`] and [`crate::space_with_store`]
//!   take [`DEFAULT_BUDGET`].
//! - Each request may carry `budget=<milliseconds>`, and the budget that applies is the
//!   SMALLER of that and the ceiling. A host's anonymous door stamps a small `budget=` on
//!   every request it forwards (overwriting any the caller sent); a caller who sends a large
//!   one gets the ceiling, never more. See [`effective_budget`].
//!
//! # What the budget does not cover
//!
//! - **The planner**, beyond the algebra bounds above: a query at those bounds still plans
//!   before the first token check. The worst measured shapes are in the table on
//!   [`MAX_ALGEBRA_NODES`].
//! - **Work between two dataset touches in an operator that consumes its whole input before
//!   yielding**: an aggregate (`COUNT`, `GROUP BY`), `ORDER BY`, and the build side of a join.
//!   oxigraph's cross-product and hash-join iterators combine in-memory tuples without
//!   checking the token, so `SELECT (COUNT(*) AS ?n) { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i . ?j ?k
//!   ?l }` over a 470-triple graph ran 4 s past its cancellation. Rows that reach this crate's
//!   serializer are checked one at a time, so the same product WITHOUT the aggregate stops at
//!   the deadline. Closing this needs oxigraph to check the token in those loops.
//! - **Resolving `graph=` sources** in [`crate::space`]: those are other endpoints' requests,
//!   under their own policies. The budget starts when parsing starts.
//! - **wasm**, which has no threads: no watchdog runs, and only the algebra bounds apply.
//!
//! # And the SIZE of the answer (ledger #970)
//!
//! A deadline bounds how long a query runs, not how much it produces meanwhile: a 939-byte
//! `VALUES` cross product (three lists of 100) answered with 275 MB of JSON in 680 ms
//! (release build), well inside a 5 s budget, and every byte of it was held in memory before
//! the first one left. So every answer is also bounded in size, by an [`AnswerBound`]: a
//! number of ROWS (solutions for `SELECT`, triples for `CONSTRUCT` and `DESCRIBE`) and a
//! number of serialized BYTES, for every form. `ASK` is exempt: its answer is one boolean.
//!
//! The bound is counted WHILE the answer is serialized, one row at a time and one write at a
//! time ([`CappedWriter`]), and the serialization stops at the first row or byte past it. An
//! answer over the bound is refused with [`Error::InvalidArgument`] on `query` ([`too_large`]),
//! the kind the algebra bounds use, and no part of it leaves: a bound refuses, it never
//! truncates.
//!
//! The space's bound is the host's, set when it builds the space ([`crate::space_with_bounds`],
//! [`crate::space_with_store_and_bounds`]; the other constructors take
//! [`AnswerBound::DEFAULT`]), and never above [`AnswerBound::CEILING`]. A request's
//! `max_rows=` and `max_bytes=` can only LOWER it ([`effective_answer`]).
//!
//! Not covered: what oxigraph materializes BEFORE the first row reaches the serializer. An
//! `ORDER BY`, a `GROUP BY` or a `DISTINCT` consumes its whole input first, and that input is
//! bounded only by the time budget.
//!
//! # Bounds come inline, or not at all
//!
//! `budget=`, `max_rows=` and `max_bytes=` are read with [`inline_bound`], which refuses one
//! given by reference (or as anything but UTF-8) instead of ignoring it. Ignoring it fell back
//! to the space's own bound, and a host that stamps a smaller bound only when the caller sent
//! none ("add if missing") saw one present and stamped nothing: a caller bypassed the door by
//! sending its bound as a reference.

use ikigai_core::{ArgRef, Error, Invocation, Result};
pub use oxigraph::sparql::CancellationToken;
use spargebra::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
};
use spargebra::{GraphUpdateOperation, Query, Update};
use std::io::Write;
use std::time::Duration;

/// The budget a space applies when its host names none, and the most any request may get
/// from [`crate::space`] or [`crate::space_with_store`]: **5 seconds**.
///
/// The heaviest legitimate query found in the ecosystem (a survey of every repo, 2026-10-09)
/// is the reading room's book CONSTRUCT over its 4.4 MB Zotero export — 65,607 quads in,
/// 11,445 triples out — and it evaluates and serializes in **18 ms** (release build). Every
/// other query found is smaller in both text and data. 5 s is over 250 times that, room for a
/// slower machine, a debug build and a library ten times the size, and it ends an attack in
/// seconds instead of minutes. A host exposing SPARQL to anonymous callers should stamp a
/// smaller `budget=` at that door: 1,000 ms is still 50 times the heaviest legitimate query.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(5);

/// The most operands one join may have after the planner flattens it: **32**.
///
/// A join's operands are its triple patterns (a sequence path `:a/:b/:c` is parsed into one
/// triple pattern per step) and every nested group, sub-`SELECT` and path pattern joined beside
/// them, counted as the planner will see them AFTER its normalization: a wrapper it can erase
/// (`GRAPH`, `FILTER`, `BIND`, an `OPTIONAL`'s or `MINUS`'s left side, …) is not one operand,
/// so its patterns count toward the enclosing join (ledger #1426, #1429; the rule is on
/// `Cost::join`); a `UNION` branch and an `OPTIONAL` or `MINUS` right side are joins of their
/// own. oxigraph's greedy join
/// reordering is about cubic in this number: measured, 32 patterns plan in ~4 ms, 64 in
/// ~70 ms, 100 in ~350 ms, 200 in 7 s, and 1,000 path steps in 19 s. The largest join the
/// ecosystem runs has 11 patterns (survey, 2026-10-09). 32 rather than 64 because joins
/// multiply under [`MAX_ALGEBRA_NODES`]: sixteen 63-pattern UNION branches planned in 0.78 s,
/// thirty-two 31-pattern ones in 0.1 s.
pub const MAX_JOIN_OPERANDS: usize = 32;

/// The most algebra nodes a query or update may have: **1024**.
///
/// A node is a triple pattern, a path operator, a graph-pattern operator (`OPTIONAL`, `UNION`,
/// `FILTER`, `BIND`, a group, …) or an expression operator (`||`, `=`, a function call, …).
/// Variables, IRIs and literals cost nothing, and neither do `VALUES` rows, `IN (…)` members
/// that are constants, `INSERT DATA`/`DELETE DATA` quads or template triples: those are flat
/// and cost the planner nothing measurable (4,096 `VALUES` rows or `IN` members plan in
/// under 5 ms). What this bounds is the shapes the planner is quadratic in: measured at 4,096
/// of each, an `OPTIONAL` chain planned in 5 s, an `?o = <x> || …` chain in 3 s, a `BIND` list
/// in 0.5 s. Inside both bounds the worst measured plan is ~125 ms (thirty-two `OPTIONAL`s of
/// 31 patterns each); an `OPTIONAL` chain of 512 plans in ~75 ms. The largest query the
/// ecosystem runs has about 60 (the work ledger's listing: 16 `OPTIONAL`s).
///
/// Measured 2026-10-09, oxigraph 0.5.11, release, aarch64-apple-darwin, with
/// `tests/sparql_time_measure.rs`, which re-measures all of this when oxigraph moves.
pub const MAX_ALGEBRA_NODES: usize = 1024;

/// The budget that applies to one request: the space's `ceiling`, or the request's own
/// `budget=` milliseconds when that is SMALLER. A request cannot raise its budget above the
/// ceiling the host built the space with; a budget that is not a positive whole number of
/// milliseconds is refused, naming the argument.
///
/// ```
/// use ikigai_sparql::budget::effective_budget;
/// use std::time::Duration;
///
/// let ceiling = Duration::from_secs(10);
/// assert_eq!(effective_budget(None, ceiling).unwrap(), ceiling);
/// assert_eq!(effective_budget(Some("250"), ceiling).unwrap(), Duration::from_millis(250));
/// // Asking for more than the ceiling gets the ceiling.
/// assert_eq!(effective_budget(Some("3600000"), ceiling).unwrap(), ceiling);
/// assert!(effective_budget(Some("0"), ceiling).is_err());
/// assert!(effective_budget(Some("1s"), ceiling).is_err());
/// ```
pub fn effective_budget(requested: Option<&str>, ceiling: Duration) -> Result<Duration> {
    let Some(text) = requested else {
        return Ok(ceiling);
    };
    match text.trim().parse::<u64>() {
        Ok(ms) if ms > 0 => Ok(ceiling.min(Duration::from_millis(ms))),
        _ => Err(Error::InvalidArgument {
            name: "budget".to_string(),
            detail: format!(
                "`{text}` is not a time budget: give a positive whole number of milliseconds. \
                 It can only lower the budget this space applies ({} ms), never raise it",
                ceiling.as_millis()
            ),
        }),
    }
}

/// The refusal for work stopped at its deadline. Transient by `ikigai_core`'s rules, so it is
/// never cached; the same query will be refused again under the same budget.
pub fn timeout(budget: Duration, what: &str) -> Error {
    Error::Timeout(format!(
        "{what} ran past its time budget of {} ms and was stopped (ledger #964). The budget is \
         set by the host, and a request's `budget=` can only lower it. Narrow the query — fewer \
         joins, a bound subject, a LIMIT — or ask the host for a larger budget",
        budget.as_millis()
    ))
}

/// Run `work` with a [`CancellationToken`] that a watchdog cancels once `budget` has passed.
/// Returns what `work` returned and whether the deadline fired before it finished. The caller
/// decides what a fired deadline means: a query refuses, an update that committed anyway has
/// still been applied.
///
/// The watchdog is a second thread that waits on a channel for at most `budget`; `work`
/// returning drops the sender, which wakes and ends it without cancelling, and this joins it
/// before returning, so nothing outlives the call. A watchdog that cannot be spawned is a
/// transient [`Error::Unavailable`] and `work` does not run: unbudgeted is the hole this
/// closes. On wasm there are no threads; `work` runs and nothing cancels the token.
///
/// ```
/// use ikigai_sparql::budget::with_deadline;
/// use std::time::{Duration, Instant};
///
/// let (spun, fired) = with_deadline(Duration::from_millis(50), |token| {
///     let start = Instant::now();
///     while !token.is_cancelled() {
///         std::hint::spin_loop();
///     }
///     start.elapsed()
/// })
/// .unwrap();
/// assert!(fired && spun >= Duration::from_millis(50));
/// let (_, fired) = with_deadline(Duration::from_secs(5), |_| ()).unwrap();
/// assert!(!fired);
/// ```
pub fn with_deadline<T, F>(budget: Duration, work: F) -> Result<(T, bool)>
where
    F: FnOnce(&CancellationToken) -> T,
{
    let token = CancellationToken::new();
    #[cfg(not(target_family = "wasm"))]
    {
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let watchdog = token.clone();
        let handle = std::thread::Builder::new()
            .name("ikigai-sparql-budget".to_string())
            .spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(budget) {
                    watchdog.cancel();
                }
            })
            .map_err(|e| {
                Error::Unavailable(format!(
                    "could not start the thread that enforces this SPARQL text's time budget: {e}"
                ))
            })?;
        let out = work(&token);
        drop(done);
        let _ = handle.join();
        Ok((out, token.is_cancelled()))
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = budget;
        Ok((work(&token), false))
    }
}

/// The most rows an answer may hold when its host names no other bound: **100,000**.
///
/// A row is a solution of a `SELECT` or a triple of a `CONSTRUCT` or `DESCRIBE`; an `ASK` has
/// none. The heaviest legitimate query found in the ecosystem (the survey behind
/// [`DEFAULT_BUDGET`]) answers with 11,445 triples, so this is almost nine times that. A
/// caller that wants more pages with `LIMIT` and `OFFSET`, which is what a bound should make
/// it do. Counted while the answer is serialized, and the row past the bound is refused, never
/// dropped.
pub const DEFAULT_MAX_ROWS: u64 = 100_000;

/// The most serialized bytes an answer may hold when its host names no other bound:
/// **16 MiB**.
///
/// Rows vary in width far more than in count, so rows alone do not bound memory: a
/// three-variable `SELECT` row of small literals is about 275 bytes as SPARQL JSON results
/// (measured: 1,000,000 rows, 274,760,059 bytes), and a row of long literals can be any size.
/// At that width this bound binds first, at about 61,000 rows. Counted at every write the
/// serializer makes, and the write past the bound is refused, never cut.
pub const DEFAULT_MAX_BYTES: u64 = 16 << 20;

/// The most rows any host may allow an answer: **10,000,000**. A host asking for more when
/// it builds its bound is refused ([`AnswerBound::new`]).
pub const CEILING_MAX_ROWS: u64 = 10_000_000;

/// The most serialized bytes any host may allow an answer: **1 GiB**. A host asking for more
/// when it builds its bound is refused ([`AnswerBound::new`]).
pub const CEILING_MAX_BYTES: u64 = 1 << 30;

/// How large one answer may be: at most [`rows`](AnswerBound::rows) rows and at most
/// [`bytes`](AnswerBound::bytes) serialized bytes, both at least 1 and at most
/// [`AnswerBound::CEILING`].
///
/// ```
/// use ikigai_sparql::budget::{AnswerBound, CEILING_MAX_ROWS, DEFAULT_MAX_BYTES};
///
/// let bound = AnswerBound::new(1_000_000, DEFAULT_MAX_BYTES).unwrap();
/// assert_eq!(bound.rows(), 1_000_000);
/// assert!(AnswerBound::new(CEILING_MAX_ROWS + 1, DEFAULT_MAX_BYTES).is_err());
/// assert!(AnswerBound::new(0, DEFAULT_MAX_BYTES).is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnswerBound {
    rows: u64,
    bytes: u64,
}

impl AnswerBound {
    /// [`DEFAULT_MAX_ROWS`] and [`DEFAULT_MAX_BYTES`]: the bound of a space whose host named
    /// none.
    pub const DEFAULT: AnswerBound = AnswerBound {
        rows: DEFAULT_MAX_ROWS,
        bytes: DEFAULT_MAX_BYTES,
    };

    /// [`CEILING_MAX_ROWS`] and [`CEILING_MAX_BYTES`]: the largest bound a host may set.
    pub const CEILING: AnswerBound = AnswerBound {
        rows: CEILING_MAX_ROWS,
        bytes: CEILING_MAX_BYTES,
    };

    /// A bound of `rows` rows and `bytes` bytes. Either one zero or over its ceiling is
    /// refused, naming `max_rows` or `max_bytes`: a host configuration that asks for more than
    /// the ceiling is a mistake to say out loud, not one to quietly clamp.
    pub fn new(rows: u64, bytes: u64) -> Result<Self> {
        let check = |name: &str, value: u64, ceiling: u64, unit: &str| {
            if value == 0 || value > ceiling {
                Err(Error::InvalidArgument {
                    name: name.to_string(),
                    detail: format!(
                        "an answer bound of {value} {unit} is out of range: it must be at least \
                         1 and at most {ceiling} {unit}"
                    ),
                })
            } else {
                Ok(value)
            }
        };
        Ok(AnswerBound {
            rows: check("max_rows", rows, CEILING_MAX_ROWS, "rows")?,
            bytes: check("max_bytes", bytes, CEILING_MAX_BYTES, "bytes")?,
        })
    }

    /// The most rows (solutions, or triples) an answer may hold.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The most serialized bytes an answer may hold.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Default for AnswerBound {
    fn default() -> Self {
        AnswerBound::DEFAULT
    }
}

/// The bound that applies to one answer: the space's, with each of the request's `max_rows=`
/// and `max_bytes=` applied when it is SMALLER. Like [`effective_budget`], a request can only
/// lower its bound, and a value that is not a positive whole number is refused, naming the
/// argument.
///
/// ```
/// use ikigai_sparql::budget::{effective_answer, AnswerBound};
///
/// let space = AnswerBound::DEFAULT;
/// assert_eq!(effective_answer(None, None, space).unwrap(), space);
/// let lowered = effective_answer(Some("10"), Some("4096"), space).unwrap();
/// assert_eq!((lowered.rows(), lowered.bytes()), (10, 4096));
/// // Asking for more than the space's bound gets the space's bound.
/// let asked = effective_answer(Some("999999999"), None, space).unwrap();
/// assert_eq!(asked.rows(), space.rows());
/// assert!(effective_answer(Some("0"), None, space).is_err());
/// assert!(effective_answer(None, Some("16MiB"), space).is_err());
/// ```
pub fn effective_answer(
    max_rows: Option<&str>,
    max_bytes: Option<&str>,
    space: AnswerBound,
) -> Result<AnswerBound> {
    let lower = |name: &str, requested: Option<&str>, bound: u64, unit: &str| {
        let Some(text) = requested else {
            return Ok(bound);
        };
        match text.trim().parse::<u64>() {
            Ok(n) if n > 0 => Ok(bound.min(n)),
            _ => Err(Error::InvalidArgument {
                name: name.to_string(),
                detail: format!(
                    "`{text}` is not an answer bound: give a positive whole number of {unit}. \
                     It can only lower the bound this space applies ({bound} {unit}), never \
                     raise it"
                ),
            }),
        }
    };
    Ok(AnswerBound {
        rows: lower("max_rows", max_rows, space.rows, "rows")?,
        bytes: lower("max_bytes", max_bytes, space.bytes, "bytes")?,
    })
}

/// A bounding argument (`budget=`, `max_rows=`, `max_bytes=`) as inline text: `None` when
/// the request does not carry it, and a refusal naming it when it is given any other way — by
/// reference, as content, or as bytes that are not UTF-8.
///
/// Refused, not ignored: an ignored bound falls back to the space's own, and a host door that
/// stamps a smaller bound only when the caller sent none would see one present and stamp
/// nothing. The caller would have bypassed the door by sending its bound as a reference.
pub fn inline_bound<'a>(inv: &Invocation<'a>, name: &str) -> Result<Option<&'a str>> {
    let request = inv.request;
    let refuse = |how: &str| Error::InvalidArgument {
        name: name.to_string(),
        detail: format!(
            "`{name}=` was given {how}: a bound is read only as an inline value, and one given \
             any other way is refused rather than ignored, because ignoring it would apply this \
             space's own bound in place of the one a host's door meant to stamp (ledger #970)"
        ),
    };
    match request.args.get(name) {
        None => Ok(None),
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|_| refuse("as bytes that are not UTF-8 text")),
        Some(ArgRef::Reference(_)) => Err(refuse("by reference")),
        Some(ArgRef::Content(_)) => Err(refuse("as interned content")),
    }
}

/// What an answer bound counts, for [`too_large`]'s wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// Solutions of a `SELECT`.
    Rows,
    /// Triples of a `CONSTRUCT` or `DESCRIBE`; bounded by `max_rows=` all the same.
    Triples,
    /// Serialized bytes, of any form but `ASK`.
    Bytes,
}

/// The refusal for an answer over its [`AnswerBound`]: [`Error::InvalidArgument`] on `query`,
/// naming the bound, its value and the cure. Never cached, like every refusal.
///
/// ```
/// use ikigai_sparql::budget::{too_large, Measure};
///
/// let text = too_large(Measure::Rows, 100_000).to_string();
/// assert!(text.contains(
///     "the answer exceeds 100000 rows; add LIMIT, narrow the query, or ask the host for more"
/// ));
/// ```
pub fn too_large(measure: Measure, bound: u64) -> Error {
    let (unit, arg) = match measure {
        Measure::Rows => ("rows", "max_rows"),
        Measure::Triples => ("triples", "max_rows"),
        Measure::Bytes => ("bytes", "max_bytes"),
    };
    Error::InvalidArgument {
        name: "query".to_string(),
        detail: format!(
            "the answer exceeds {bound} {unit}; add LIMIT, narrow the query, or ask the host for \
             more. It was refused, not truncated: no part of it was sent (ledger #970). A \
             request's `{arg}=` can only lower this bound"
        ),
    }
}

/// A [`Write`] that keeps what is written in memory and refuses the write that would take it
/// past `cap` bytes, remembering that it did. An answer serializer writes into one, so the
/// byte bound is enforced as the answer is produced, not measured after it was all held.
///
/// The serializer's own error for the refused write is not the answer's refusal: check
/// [`over`](CappedWriter::over) and refuse with [`too_large`].
///
/// ```
/// use ikigai_sparql::budget::CappedWriter;
/// use std::io::Write;
///
/// let mut out = CappedWriter::new(8);
/// out.write_all(b"12345678").unwrap();
/// assert!(!out.over());
/// assert!(out.write_all(b"9").is_err());
/// assert!(out.over());
/// assert_eq!(out.into_bytes(), b"12345678");
/// ```
#[derive(Debug)]
pub struct CappedWriter {
    bytes: Vec<u8>,
    cap: u64,
    over: bool,
}

impl CappedWriter {
    /// An empty writer that holds at most `cap` bytes.
    pub fn new(cap: u64) -> Self {
        CappedWriter {
            bytes: Vec::new(),
            cap,
            over: false,
        }
    }

    /// Whether a write was refused for passing the cap.
    pub fn over(&self) -> bool {
        self.over
    }

    /// What was written, never more than the cap.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len() as u64 + buf.len() as u64 > self.cap {
            self.over = true;
            return Err(std::io::Error::other(format!(
                "the answer passed its bound of {} bytes",
                self.cap
            )));
        }
        // Grow by doubling, as a Vec would, but never past the cap: the bound is on memory,
        // and a plain Vec's doubling would reserve up to twice the cap.
        let needed = self.bytes.len() + buf.len();
        if needed > self.bytes.capacity() {
            let target = (self.bytes.capacity() * 2)
                .max(needed)
                .min(usize::try_from(self.cap).unwrap_or(usize::MAX));
            self.bytes.reserve_exact(target - self.bytes.len());
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Refuse a parsed query whose algebra exceeds [`MAX_JOIN_OPERANDS`] or [`MAX_ALGEBRA_NODES`],
/// naming the argument `arg` and the bound. Called after parsing and before planning.
pub fn check_query(query: &Query, arg: &str) -> Result<()> {
    Cost::of_query(query).verdict(arg)
}

/// [`check_query`] for an update: the `WHERE` of every `DELETE`/`INSERT` operation counts
/// toward one total, and each join is bounded on its own.
pub fn check_update(update: &Update, arg: &str) -> Result<()> {
    let mut cost = Cost::default();
    for operation in &update.operations {
        if let GraphUpdateOperation::DeleteInsert { pattern, .. } = operation {
            cost.pattern(pattern);
        }
    }
    cost.verdict(arg)
}

/// One walk of a parsed algebra: what it costs the planner, and whether it calls `SERVICE`.
///
/// The walk is shared with [`crate::egress`] because it is the one traversal here that visits
/// every pattern, including those inside an expression's `EXISTS`, and a second walker would be
/// a second place for a new algebra node to be missed.
#[derive(Default)]
pub(crate) struct Cost {
    nodes: usize,
    widest_join: usize,
    /// The first `SERVICE` the walk met, as written (`<iri>` or `?var`).
    pub(crate) first_service: Option<String>,
}

impl Cost {
    pub(crate) fn of_query(query: &Query) -> Self {
        match query {
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Describe { pattern, .. }
            | Query::Construct { pattern, .. } => Self::of_pattern(pattern),
        }
    }

    pub(crate) fn of_pattern(pattern: &GraphPattern) -> Self {
        let mut cost = Cost::default();
        cost.pattern(pattern);
        cost
    }

    fn verdict(&self, arg: &str) -> Result<()> {
        if self.widest_join > MAX_JOIN_OPERANDS {
            return Err(Error::InvalidArgument {
                name: arg.to_string(),
                detail: format!(
                    "this SPARQL text joins {} patterns in one group, and this endpoint refuses \
                     more than {MAX_JOIN_OPERANDS} (MAX_JOIN_OPERANDS) before planning: the \
                     planner's join ordering grows about with the cube of that number and \
                     cannot be interrupted (ledger #964). A sequence path counts one pattern a \
                     step. Split the query, or move a list of values into VALUES",
                    self.widest_join
                ),
            });
        }
        if self.nodes > MAX_ALGEBRA_NODES {
            return Err(Error::InvalidArgument {
                name: arg.to_string(),
                detail: format!(
                    "this SPARQL text has {} algebra operators (patterns, OPTIONAL/UNION/FILTER/\
                     BIND, path and expression operators), and this endpoint refuses more than \
                     {MAX_ALGEBRA_NODES} (MAX_ALGEBRA_NODES) before planning: the planner is \
                     quadratic in several of them and cannot be interrupted (ledger #964). \
                     Constants in VALUES and IN (…) cost nothing; use them for long lists",
                    self.nodes
                ),
            });
        }
        Ok(())
    }

    // The walk is kept identical to ikigai-store's `budget.rs`, which copies it (ledger #970):
    // change both or neither. The design is the doc comment on `join` below; copy it verbatim.

    /// Walk `pattern` as a join of its own (the root of a query or an update's `WHERE`, a
    /// `UNION` branch, the right side of an `OPTIONAL` or a `MINUS`, the inside of a
    /// sub-`SELECT`, a `GROUP`, a `SERVICE` or an `EXISTS`), and record how wide it is.
    fn pattern(&mut self, pattern: &GraphPattern) {
        let operands = self.join(pattern);
        self.widest_join = self.widest_join.max(operands);
    }

    /// How many operands `pattern` can put into the join it sits in, AS THE PLANNER WILL SEE
    /// THEM: never fewer, and only as many more as the rule below costs.
    ///
    /// # What is counted, and why (ledger #964, #1426, #1429)
    ///
    /// oxigraph plans a query with sparopt's `Optimizer::optimize_graph_pattern`, which runs
    /// three passes: `normalize_pattern`, then `reorder_joins`, then `push_filters`.
    /// `reorder_joins` FLATTENS each tree of `Join`s into one list and orders it greedily, about
    /// cubic in the list's length, and nothing in it checks a cancellation token. So the number
    /// that must be bounded is the length of each flattened list AFTER normalization, not the
    /// width of each group as written. Normalization erases wrappers, and an erased wrapper's
    /// patterns join the ENCLOSING list:
    ///
    /// - `GRAPH <g> { … }` / `GRAPH ?g { … }` always (`from_sparql_algebra` pushes the graph
    ///   name into each quad pattern; ledger #1426);
    /// - a `FILTER` whose expression normalizes to a constant true (`FILTER(true)`,
    ///   `FILTER(1)`, `FILTER(BOUND(?s))` where `?s` is always bound, …);
    /// - an `OPTIONAL` whose right side is empty or never matches (`OPTIONAL {}`,
    ///   `OPTIONAL { … FILTER(false) }`), which returns its left side;
    /// - a `BIND` whose expression has no possible value (`BIND(?unbound AS ?x)`), which
    ///   returns its inner;
    /// - a `MINUS` whose right side never matches, which returns its left side;
    /// - a `LATERAL` with an empty side, which returns the other side;
    /// - an `ORDER BY` with no keys and an `OFFSET 0` with no `LIMIT`, which return their inner.
    ///
    /// Whether most of these erase depends on sparopt's TYPE INFERENCE over the inner pattern,
    /// and neither `normalize_pattern` nor the type inference is public (sparopt 0.3.7; the
    /// only public entry, `optimize_graph_pattern`, also runs `reorder_joins`, the very thing
    /// being bounded). So the walk does not decide: **every wrapper that normalization CAN
    /// erase is counted as erased.** A `GRAPH`, `FILTER`, `BIND`, `ORDER BY` or `OFFSET`/`LIMIT`
    /// passes its inner's operands through to the enclosing join; an `OPTIONAL` or a `MINUS`
    /// passes its LEFT side through, and its right side is a join of its own; a `LATERAL`
    /// passes through the wider of its two sides. Adding wrappers one at a time did not
    /// converge (the three in ledger #1429 were found after the fix for `GRAPH`); this rule
    /// covers every wrapper by construction, including ones nobody has measured.
    ///
    /// Counting a wrapper as erased never UNDER-counts. If the wrapper survives, it is one
    /// operand of the enclosing list and its inner is a list of its own; both are at most what
    /// this walk adds to the enclosing join, because every pattern counts at least one operand
    /// here (an empty group counts one, though it may vanish). The cost is an over-count: two
    /// groups of 20 patterns each closed by a real `FILTER` are counted as one join of 40 and
    /// refused, where the planner orders two joins of 20. Split such a query, or move a
    /// pattern into a sub-`SELECT`.
    ///
    /// Never erased, so one operand each, with a join of their own inside: a sub-`SELECT`
    /// (its `Project`), `DISTINCT`, `REDUCED`, `GROUP BY`, `UNION` (a union keeps its wrapper
    /// even when one branch is empty), `SERVICE`, `VALUES`, and a property path that is not a
    /// sequence (a sequence path `:a/:b/:c` is parsed into one pattern a step).
    ///
    /// Iterative over the join and its erasable wrappers, so a long `OPTIONAL` or `BIND` chain
    /// is a loop, not a recursion.
    fn join(&mut self, pattern: &GraphPattern) -> usize {
        let mut operands = 0;
        let mut todo = vec![pattern];
        while let Some(next) = todo.pop() {
            match next {
                GraphPattern::Bgp { patterns } => {
                    self.nodes += patterns.len();
                    operands += patterns.len().max(1);
                }
                GraphPattern::Join { left, right } => {
                    self.nodes += 1;
                    todo.push(left);
                    todo.push(right);
                }
                GraphPattern::Graph { inner, .. } | GraphPattern::Slice { inner, .. } => {
                    self.nodes += 1;
                    todo.push(inner);
                }
                GraphPattern::Filter { expr, inner } => {
                    self.nodes += 1;
                    self.expression(expr);
                    todo.push(inner);
                }
                GraphPattern::Extend {
                    inner, expression, ..
                } => {
                    self.nodes += 1;
                    self.expression(expression);
                    todo.push(inner);
                }
                GraphPattern::OrderBy { inner, expression } => {
                    self.nodes += 1;
                    for order in expression {
                        match order {
                            OrderExpression::Asc(e) | OrderExpression::Desc(e) => {
                                self.expression(e)
                            }
                        }
                    }
                    todo.push(inner);
                }
                GraphPattern::LeftJoin {
                    left,
                    right,
                    expression,
                } => {
                    self.nodes += 1;
                    todo.push(left);
                    self.pattern(right);
                    if let Some(expression) = expression {
                        self.expression(expression);
                    }
                }
                GraphPattern::Minus { left, right } => {
                    self.nodes += 1;
                    todo.push(left);
                    self.pattern(right);
                }
                GraphPattern::Lateral { left, right } => {
                    self.nodes += 1;
                    let left = self.join(left);
                    let right = self.join(right);
                    operands += left.max(right);
                }
                GraphPattern::Path { path, .. } => {
                    operands += 1;
                    self.path(path);
                }
                GraphPattern::Union { left, right } => {
                    self.nodes += 1;
                    operands += 1;
                    self.pattern(left);
                    self.pattern(right);
                }
                GraphPattern::Group {
                    inner, aggregates, ..
                } => {
                    self.nodes += 1;
                    operands += 1;
                    for (_, aggregate) in aggregates {
                        if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                            self.expression(expr);
                        }
                    }
                    self.pattern(inner);
                }
                GraphPattern::Service { name, inner, .. } => {
                    self.first_service.get_or_insert_with(|| name.to_string());
                    self.nodes += 1;
                    operands += 1;
                    self.pattern(inner);
                }
                GraphPattern::Project { inner, .. }
                | GraphPattern::Distinct { inner }
                | GraphPattern::Reduced { inner } => {
                    self.nodes += 1;
                    operands += 1;
                    self.pattern(inner);
                }
                GraphPattern::Values { .. } => {
                    self.nodes += 1;
                    operands += 1;
                }
            }
        }
        operands
    }

    fn path(&mut self, path: &PropertyPathExpression) {
        self.nodes += 1;
        match path {
            PropertyPathExpression::NamedNode(_)
            | PropertyPathExpression::NegatedPropertySet(_) => {}
            PropertyPathExpression::Reverse(p)
            | PropertyPathExpression::ZeroOrMore(p)
            | PropertyPathExpression::OneOrMore(p)
            | PropertyPathExpression::ZeroOrOne(p) => self.path(p),
            PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
                self.path(a);
                self.path(b);
            }
        }
    }

    fn expression(&mut self, expression: &Expression) {
        match expression {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::Bound(_) => {}
            Expression::In(e, members) => {
                self.nodes += 1;
                self.expression(e);
                for member in members {
                    self.expression(member);
                }
            }
            Expression::Exists(pattern) => {
                self.nodes += 1;
                self.pattern(pattern);
            }
            Expression::Coalesce(args) | Expression::FunctionCall(_, args) => {
                self.nodes += 1;
                for arg in args {
                    self.expression(arg);
                }
            }
            Expression::If(a, b, c) => {
                self.nodes += 1;
                self.expression(a);
                self.expression(b);
                self.expression(c);
            }
            Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
                self.nodes += 1;
                self.expression(e);
            }
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => {
                self.nodes += 1;
                self.expression(a);
                self.expression(b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::SparqlParser;

    fn cost(text: &str) -> (usize, usize) {
        let mut cost = Cost::default();
        match SparqlParser::new().parse_query(text).unwrap() {
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Describe { pattern, .. }
            | Query::Construct { pattern, .. } => cost.pattern(&pattern),
        }
        (cost.widest_join, cost.nodes)
    }

    fn query(text: &str) -> Result<()> {
        check_query(&SparqlParser::new().parse_query(text).unwrap(), "query")
    }

    fn patterns(n: usize) -> String {
        (0..n).map(|i| format!("?s <urn:p> ?o{i} . ")).collect()
    }

    #[test]
    fn a_join_counts_its_patterns_its_path_steps_and_its_nested_groups() {
        assert_eq!(cost("SELECT * WHERE { ?s ?p ?o }").0, 1);
        assert_eq!(
            cost(&format!("SELECT * WHERE {{ {} }}", patterns(10))).0,
            10
        );
        // A sequence path is parsed into one pattern a step.
        assert_eq!(
            cost("SELECT * WHERE { ?s <urn:a>/<urn:b>/<urn:c> ?o }").0,
            3
        );
        // Nested groups are flattened into one join, as the planner flattens them.
        let nested = format!(
            "SELECT * WHERE {{ {{ {} }} {{ {} }} {{ SELECT * {{ ?x ?y ?z }} }} }}",
            patterns(5),
            patterns(6)
        );
        assert_eq!(cost(&nested).0, 5 + 6 + 1);
        // OPTIONAL and UNION are separate joins: each side is bounded on its own.
        let split = format!(
            "SELECT * WHERE {{ {} OPTIONAL {{ {} }} }}",
            patterns(30),
            patterns(30)
        );
        assert_eq!(cost(&split).0, 30);
    }

    /// `blocks` `GRAPH` blocks of `per_block` patterns each, side by side in one group, and
    /// `name` the graph each names (`<urn:g>` or `?g`). The auditor's shape (ledger #1426).
    fn graph_blocks(blocks: usize, per_block: usize, name: &str) -> String {
        let body: String = (0..blocks)
            .map(|b| {
                let inner: String = (0..per_block)
                    .map(|i| format!("?s <urn:p> ?o{b}_{i} . "))
                    .collect();
                format!("GRAPH {name} {{ {inner} }} ")
            })
            .collect();
        format!("SELECT * WHERE {{ {body} }}")
    }

    #[test]
    fn a_graph_block_is_not_one_operand_its_patterns_join_the_enclosing_join() {
        // sparopt erases the GRAPH wrapper into quad patterns and then flattens the joins,
        // so six blocks of 31 plan as ONE join of 186 (measured: 3.8 s in a release build,
        // twelve blocks 64 s). They are counted that way, and refused.
        for name in ["<urn:g>", "?g"] {
            assert_eq!(cost(&graph_blocks(6, 31, name)).0, 6 * 31, "{name}");
            let err = query(&graph_blocks(6, 31, name)).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidArgument { name, detail } if name == "query"
                    && detail.contains("MAX_JOIN_OPERANDS")
                    && detail.contains("186")),
                "{name}: {err}"
            );
        }
        // Two blocks of 16 are 32: at the bound, admitted. One more pattern is refused.
        query(&graph_blocks(2, 16, "<urn:g>")).unwrap();
        let err = query(&format!(
            "SELECT * WHERE {{ GRAPH <urn:g> {{ {} }} GRAPH <urn:h> {{ {} }} ?x ?y ?z }}",
            patterns(16),
            patterns(16)
        ))
        .unwrap_err();
        assert!(err.to_string().contains("33"), "{err}");
        // A GRAPH nested in a GRAPH, or in a nested group, still joins the enclosing join.
        let nested = format!(
            "SELECT * WHERE {{ {} GRAPH <urn:g> {{ {} GRAPH ?h {{ {} }} }} {{ GRAPH ?k {{ {} }} }} }}",
            patterns(3),
            patterns(4),
            patterns(5),
            patterns(6)
        );
        assert_eq!(cost(&nested).0, 3 + 4 + 5 + 6);
        // A GRAPH around a whole group is the same join as the group without it.
        assert_eq!(
            cost(&format!(
                "SELECT * WHERE {{ GRAPH <urn:g> {{ {} {{ {} }} }} }}",
                patterns(10),
                patterns(10)
            ))
            .0,
            20
        );
    }

    /// `blocks` groups of `per_block` patterns side by side in one group, each group closed
    /// by `wrap` (with `{b}` replaced by the block's number). The shapes of ledger #1429.
    fn wrapped_blocks(blocks: usize, per_block: usize, wrap: &str) -> String {
        let body: String = (0..blocks)
            .map(|b| {
                let inner: String = (0..per_block)
                    .map(|i| format!("?s <urn:p> ?o{b}_{i} . "))
                    .collect();
                format!("{{ {inner} {} }} ", wrap.replace("{b}", &b.to_string()))
            })
            .collect();
        format!("SELECT * WHERE {{ {body} }}")
    }

    /// Every wrapper sparopt's normalization can ERASE before it reorders joins (ledger #1429),
    /// each the way it is erased: a constant-true filter is dropped, a filter whose value is
    /// decided by the inner's variable types becomes constant, an empty or never-matching
    /// OPTIONAL returns its left side, a BIND of an unbound variable returns its inner, a
    /// never-matching MINUS returns its left side, an empty LATERAL returns its left side.
    const ERASED: [&str; 9] = [
        "FILTER(true)",
        "FILTER(1)",
        "FILTER(!false)",
        "FILTER(BOUND(?s))",
        "OPTIONAL {}",
        "OPTIONAL { ?a{b} ?p{b} ?c{b} FILTER(false) }",
        "BIND(?unbound{b} AS ?x{b})",
        "MINUS { ?a{b} ?p{b} ?c{b} FILTER(false) }",
        "LATERAL {}",
    ];

    #[test]
    fn a_wrapper_the_planner_can_erase_does_not_reset_the_join_count() {
        // Measured on 0.2.3 (release, empty store): six blocks of 31 under FILTER(true),
        // OPTIONAL {} or BIND(?unbound AS ?x) passed check_query as joins of 31 and planned as
        // ONE join of 186, ~4.1 s each, uncancellably. They are counted as that join.
        for wrap in ERASED {
            let text = wrapped_blocks(6, 31, wrap);
            assert_eq!(cost(&text).0, 6 * 31, "{wrap}");
            let err = query(&text).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidArgument { name, detail } if name == "query"
                    && detail.contains("MAX_JOIN_OPERANDS")),
                "{wrap}: {err}"
            );
        }
        // Nested erasable wrappers erase all the way down.
        let nested = format!(
            "SELECT * WHERE {{ {{ {{ {{ {} FILTER(true) }} OPTIONAL {{}} }} BIND(?u AS ?x) }} {} }}",
            patterns(20),
            patterns(13)
        );
        assert_eq!(cost(&nested).0, 33);
        assert!(query(&nested).is_err());
    }

    #[test]
    fn wrappers_that_survive_normalization_are_not_over_refused_within_the_bound() {
        // The controls from ledger #1429: a real filter and a sub-SELECT. Within the bound
        // both are admitted, wrapped or not.
        for wrap in [
            "FILTER(?o{b}_0 != 1)",
            "BIND(1 AS ?x{b})",
            "OPTIONAL { ?s <urn:q> ?q{b} }",
        ] {
            let text = wrapped_blocks(2, 16, wrap);
            assert_eq!(cost(&text).0, 32, "{wrap}");
            query(&text).unwrap();
        }
        // A sub-SELECT is a Project, which the planner never erases: one operand, and its own
        // join inside, so six of 31 are seven joins within the bound.
        let subselects: String = (0..6)
            .map(|b| {
                let inner: String = (0..31)
                    .map(|i| format!("?s <urn:p> ?o{b}_{i} . "))
                    .collect();
                format!("{{ SELECT * {{ {inner} }} OFFSET 0 }} ")
            })
            .collect();
        let text = format!("SELECT * WHERE {{ {subselects} }}");
        assert_eq!(cost(&text).0, 31);
        query(&text).unwrap();
        // UNION branches, an OPTIONAL's right side and a MINUS's right side stay joins of
        // their own, under every wrapper.
        let split = format!(
            "SELECT * WHERE {{ {{ {} FILTER(true) }} OPTIONAL {{ {} FILTER(true) }} MINUS {{ {} }} }}",
            patterns(30),
            patterns(30),
            patterns(30)
        );
        assert_eq!(cost(&split).0, 30);
        query(&split).unwrap();
    }

    #[test]
    fn graph_blocks_split_across_union_optional_or_minus_are_bounded_one_side_at_a_time() {
        // The same 186 patterns, one block a UNION branch: six joins of 31, each within the
        // bound, so admitted.
        let branches: Vec<String> = (0..6)
            .map(|b| {
                let inner: String = (0..31)
                    .map(|i| format!("?s <urn:p> ?o{b}_{i} . "))
                    .collect();
                format!("{{ GRAPH <urn:g> {{ {inner} }} }}")
            })
            .collect();
        let union = format!("SELECT * WHERE {{ {} }}", branches.join(" UNION "));
        assert_eq!(cost(&union).0, 31);
        query(&union).unwrap();
        // OPTIONAL and MINUS sides are joins of their own too.
        let optional = format!(
            "SELECT * WHERE {{ GRAPH <urn:g> {{ {} }} OPTIONAL {{ GRAPH ?g {{ {} }} }} }}",
            patterns(30),
            patterns(30)
        );
        assert_eq!(cost(&optional).0, 30);
        query(&optional).unwrap();
        let minus = format!(
            "SELECT * WHERE {{ GRAPH <urn:g> {{ {} }} MINUS {{ GRAPH ?g {{ {} }} }} }}",
            patterns(30),
            patterns(30)
        );
        assert_eq!(cost(&minus).0, 30);
        query(&minus).unwrap();
        // But a branch over the bound is refused on its own.
        let wide = format!(
            "SELECT * WHERE {{ {{ ?a ?b ?c }} UNION {{ {} }} }}",
            graph_blocks(2, 17, "<urn:g>")
                .trim_start_matches("SELECT * WHERE {")
                .trim_end_matches('}')
        );
        assert_eq!(cost(&wide).0, 34);
        assert!(query(&wide).is_err());
    }

    #[test]
    fn an_update_counts_graph_blocks_in_its_where_clause_the_same_way() {
        let text = graph_blocks(6, 31, "<urn:g>").replacen(
            "SELECT * WHERE",
            "DELETE { ?s <urn:p> ?o0_0 } WHERE",
            1,
        );
        let update = SparqlParser::new().parse_update(&text).unwrap();
        let err = check_update(&update, "content").unwrap_err();
        assert!(err.to_string().contains("MAX_JOIN_OPERANDS"), "{err}");
    }

    #[test]
    fn constants_in_values_and_in_lists_cost_nothing() {
        let values = format!(
            "SELECT * WHERE {{ VALUES ?s {{ {} }} ?s ?p ?o }}",
            "<urn:x> ".repeat(5_000)
        );
        let (_, nodes) = cost(&values);
        assert!(nodes < 10, "{nodes}");
        let list = format!(
            "SELECT * WHERE {{ ?s ?p ?o FILTER(?o IN ({})) }}",
            vec!["<urn:x>"; 5_000].join(", ")
        );
        let (_, nodes) = cost(&list);
        assert!(nodes < 10, "{nodes}");
    }

    #[test]
    fn the_bounds_admit_at_the_bound_and_refuse_one_past_it_by_name() {
        query(&format!(
            "SELECT * WHERE {{ {} }}",
            patterns(MAX_JOIN_OPERANDS)
        ))
        .unwrap();
        let err = query(&format!(
            "SELECT * WHERE {{ {} }}",
            patterns(MAX_JOIN_OPERANDS + 1)
        ))
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, detail } if name == "query"
                && detail.contains("MAX_JOIN_OPERANDS")),
            "{err}"
        );
        // `1||1||…`: n terms are n-1 operators, plus the FILTER and the empty group.
        let chain = |terms: usize| format!("SELECT * WHERE {{ FILTER(1{}) }}", "||1".repeat(terms));
        let (_, nodes) = cost(&chain(0));
        query(&chain(MAX_ALGEBRA_NODES - nodes)).unwrap();
        let err = query(&chain(MAX_ALGEBRA_NODES - nodes + 1)).unwrap_err();
        assert!(err.to_string().contains("MAX_ALGEBRA_NODES"), "{err}");
    }

    #[test]
    fn an_update_counts_every_where_clause_toward_one_total() {
        let half = MAX_ALGEBRA_NODES / 2 + 1;
        let op = format!(
            "DELETE {{ ?s ?p ?o }} WHERE {{ ?s ?p ?o FILTER({}) }}",
            vec!["?o = 1"; half / 2].join(" || ")
        );
        let one = SparqlParser::new().parse_update(&op).unwrap();
        check_update(&one, "update").unwrap();
        let two = SparqlParser::new()
            .parse_update(&format!("{op} ; {op} ; {op}"))
            .unwrap();
        let err = check_update(&two, "update").unwrap_err();
        assert!(err.to_string().contains("MAX_ALGEBRA_NODES"), "{err}");
        // INSERT DATA is flat, however long.
        let data = format!(
            "INSERT DATA {{ {} }}",
            "<urn:s> <urn:p> <urn:o> . ".repeat(10_000)
        );
        check_update(&SparqlParser::new().parse_update(&data).unwrap(), "update").unwrap();
    }
}
