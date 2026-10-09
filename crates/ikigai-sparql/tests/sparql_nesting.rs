//! A SPARQL text that would overflow the stack is refused, or run on a stack big enough for
//! it, at EVERY door of this crate that parses caller text (ledger #962, the class found by
//! ikigai-store's ledger #915).
//!
//! The claim: oxigraph's SPARQL parser and evaluator are recursive, so a query's shape is a
//! claim on the stack of whatever thread parses it, and a stack overflow aborts the WHOLE
//! process, on any thread. `urn:sparql:select` is open to anonymous visitors on the live
//! reading room (`/r/urn:sparql:select?query=`), so one request could take the host down.
//! Every reproduction here therefore runs in a CHILD PROCESS (this test binary re-executed
//! with one probe named in its environment) on a 2 MiB thread, the size of a tokio worker's.
//! The parent asserts on the child's exit: an abort kills the child, never this binary, and
//! reads as a failure with the signal named.
//!
//! The doors: the four query forms over the per-query dataset ([`ikigai_sparql::space`]),
//! the same four over a shared store ([`ikigai_sparql::space_with_store`]), and
//! `urn:sparql:update`. All nine go through two parse sites in `src/lib.rs`: `evaluate` and
//! the update endpoint's `parse_update`.
//!
//! These tests use only the API that predates the fix, so they compile, and fail with the
//! child aborted, against 0.1.10 (`257f89c`).

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_sparql::Store;
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_SPARQL_NESTING_PROBE";
/// The bound, restated so these tests compile against 0.1.10, which has no constant to name.
/// A unit test in `src/limits.rs` pins `MAX_SPARQL_NESTING` to the same number.
const BOUND: usize = 64;

const FORMS: [&str; 4] = ["select", "ask", "construct", "describe"];

/// Every query door: `q-` is the per-query dataset, `s-` the shared store.
const QUERY_DOORS: [&str; 8] = [
    "q-select",
    "q-ask",
    "q-construct",
    "q-describe",
    "s-select",
    "s-ask",
    "s-construct",
    "s-describe",
];

/// The query text for `shape` at size `n`.
fn query(shape: &str, n: usize) -> String {
    let r = |s: &str, k: usize| s.repeat(k);
    match shape {
        "parens" => format!("SELECT * WHERE {{ FILTER({}1{}) }}", r("(", n), r(")", n)),
        // A run of `!`: one level per byte, the cheapest recursion there is.
        "bang" => format!("SELECT * WHERE {{ FILTER({}true) }}", r("!", n)),
        // FLAT chains the evaluator still recurses over once per term. Not nesting, so
        // nothing lexical refuses them; they run on the stack the text is given.
        "or" => format!(
            "SELECT * WHERE {{ ?s ?p ?o FILTER(false{}) }}",
            r("||false", n)
        ),
        "or1" => format!("SELECT * WHERE {{ FILTER(1{}) }}", r("||1", n)),
        "mul" => format!("SELECT * WHERE {{ FILTER(1{} > 0) }}", r("*1", n)),
        "plus" => format!("SELECT * WHERE {{ FILTER(1{} > 0) }}", r("+1", n)),
        "path" => format!(
            "PREFIX : <urn:> SELECT * WHERE {{ ?s :a{} ?o }}",
            r("/:a", n)
        ),
        "filters" => format!("SELECT * WHERE {{ ?s ?p ?o {} }}", r("FILTER(true) ", n)),
        "bgp" => format!(
            "SELECT * WHERE {{ {} }}",
            (0..n)
                .map(|i| format!("?s <urn:p> ?o{i} . "))
                .collect::<String>()
        ),
        // The largest LEGITIMATE queries are flat: a VALUES list, one row per item.
        "values" => format!(
            "SELECT ?i WHERE {{ VALUES ?i {{ {} }} }}",
            (0..n)
                .map(|i| format!("<urn:iki:ledger:default:item:{i:026}> "))
                .collect::<String>()
        ),
        other => panic!("no query shape named {other}"),
    }
}

/// The update text for `shape` at size `n`.
fn update(shape: &str, n: usize) -> String {
    match shape {
        // `INSERT {…}` closes before the `WHERE`, so this nests exactly as deep as `parens`.
        "parens" => format!(
            "INSERT {{ <urn:s> <urn:p> <urn:o> }} WHERE {{ FILTER({}1{}) }}",
            "(".repeat(n),
            ")".repeat(n)
        ),
        "or" => format!(
            "INSERT {{ <urn:s> <urn:p> <urn:o> }} WHERE {{ ?s ?p ?o FILTER(false{}) }}",
            "||false".repeat(n)
        ),
        "insert-data" => format!(
            "INSERT DATA {{ {} }}",
            (0..n)
                .map(|i| format!("<urn:s{i}> <urn:p> <urn:o> . "))
                .collect::<String>()
        ),
        other => panic!("no update shape named {other}"),
    }
}

/// The per-query space, or a shared store seeded with one quad so a query has a row to see.
fn kernel(shared: bool) -> Kernel {
    if !shared {
        return Kernel::new(Arc::new(ikigai_sparql::space()));
    }
    let store = Arc::new(Store::new().unwrap());
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space_with_store(store)));
    block_on(kernel.issue(
        Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap()).with_arg(
            "content",
            ArgRef::Inline(b"INSERT DATA { <urn:s> <urn:p> <urn:o> }".to_vec()),
        ),
        &Capability::root(),
    ))
    .expect("seeding");
    kernel
}

/// Issue one request at `door` (`q-select`, `s-ask`, `s-update`, …) carrying `text`.
fn issue_text(kernel: &Kernel, door: &str, text: String) -> ikigai_core::Result<String> {
    let (_, form) = door.split_once('-').unwrap();
    let req = if form == "update" {
        Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap())
            .with_arg("content", ArgRef::Inline(text.into_bytes()))
    } else {
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:sparql:{form}")).unwrap(),
        )
        .with_arg("query", ArgRef::Inline(text.into_bytes()))
    };
    block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

fn text_for(door: &str, shape: &str, n: usize) -> String {
    if door.ends_with("-update") {
        update(shape, n)
    } else {
        query(shape, n)
    }
}

fn issue(door: &str, shape: &str, n: usize) -> ikigai_core::Result<String> {
    issue_text(
        &kernel(door.starts_with("s-")),
        door,
        text_for(door, shape, n),
    )
}

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, a tokio
/// worker's stack, prints the outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.split(':');
    let door = parts.next().unwrap().to_string();
    let shape = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || match issue(&door, &shape, n) {
            Ok(text) => format!("ok {}", text.replace('\n', " ")),
            Err(e) => format!("err {}", e.to_string().replace('\n', " ")),
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: run one probe in a child and return what it said, or `Err` naming how
/// it died. An abort is the defect.
fn try_probe(door: &str, shape: &str, n: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{door}:{shape}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "the `{shape}` probe at {n} through `{door}` did not survive a 2 MiB thread: {} — {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("the `{door}` `{shape}` probe reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

fn probe(door: &str, shape: &str, n: usize) -> String {
    try_probe(door, shape, n).unwrap_or_else(|died| panic!("{died}"))
}

fn assert_refused(outcome: &str, arg: &str, what: &str) {
    assert!(
        outcome.starts_with(&format!("err invalid argument `{arg}`")) && outcome.contains("64"),
        "{what}: {outcome}"
    );
}

// ------------------------------------------------------------------ the reproduction

#[test]
fn three_thousand_parentheses_are_refused_at_every_query_door_and_abort_nothing() {
    for door in QUERY_DOORS {
        assert_refused(&probe(door, "parens", 3000), "query", door);
    }
}

#[test]
fn three_thousand_parentheses_are_refused_by_the_update_door_and_abort_nothing() {
    assert_refused(&probe("s-update", "parens", 3000), "content", "s-update");
}

#[test]
fn a_run_of_not_is_nesting_and_is_refused_at_any_length() {
    // Counted with the brackets, so even a run as long as the byte bound allows is refused.
    for n in [3000, (1 << 20) - 64] {
        assert_refused(&probe("q-select", "bang", n), "query", &n.to_string());
    }
}

#[test]
fn a_long_flat_chain_runs_on_the_stack_the_query_is_given() {
    // Inline on a 2 MiB thread this aborts a debug build (~300 terms do); it is not nesting,
    // so nothing refuses it, and a generated query may really have this shape. It runs, at
    // every door, evaluation and serialization included.
    for door in QUERY_DOORS {
        let outcome = probe(door, "or", 600);
        assert!(outcome.starts_with("ok "), "{door}: {outcome}");
    }
    let outcome = probe("s-update", "or", 600);
    assert!(outcome.starts_with("ok updated:"), "{outcome}");
}

#[test]
fn a_large_flat_legitimate_query_is_answered_not_refused() {
    // 15,000 VALUES rows (~855 KB, under the 1 MiB bound) and 20,000 inserted triples:
    // the shapes a real host sends at volume. Neither recurses.
    let outcome = probe("q-select", "values", 15_000);
    let head: String = outcome.chars().take(200).collect();
    assert!(outcome.starts_with("ok "), "{head}");
    assert!(
        outcome.contains("item:00000000000000000000014999"),
        "the last row is answered: {head}"
    );
    let outcome = probe("s-update", "insert-data", 20_000);
    assert!(outcome.starts_with("ok updated: 1 -> 20001"), "{outcome}");
}

// ------------------------------------------------------------------ at and under the bound

#[test]
fn a_query_at_the_bound_parses_and_runs_at_every_door() {
    // `SELECT * WHERE { FILTER(` is two levels; the rest brings it to exactly the bound.
    // The IRI does not fix the form here, so a SELECT is answered at all eight query doors.
    for door in QUERY_DOORS {
        issue(door, "parens", BOUND - 2).unwrap_or_else(|e| panic!("{door}: {e}"));
        let over = issue(door, "parens", BOUND - 1).unwrap_err().to_string();
        assert!(over.contains("deeper than 64"), "{door}: {over}");
    }
    issue("s-update", "parens", BOUND - 2).unwrap_or_else(|e| panic!("s-update: {e}"));
    let over = issue("s-update", "parens", BOUND - 1)
        .unwrap_err()
        .to_string();
    assert!(over.contains("deeper than 64"), "s-update: {over}");
    // Sent as `update=` rather than piped, the refusal names `update`: the argument the
    // caller actually used.
    let req = Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap()).with_arg(
        "update",
        ArgRef::Inline(update("parens", BOUND - 1).into_bytes()),
    );
    let err = block_on(kernel(true).issue(req, &Capability::root())).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("invalid argument `update`: this SPARQL text nests"),
        "{err}"
    );
}

#[test]
fn the_costliest_bracket_at_the_bound_runs() {
    // A function call costs the parser the most stack per level (~60 KiB, unoptimized).
    let calls = format!(
        "SELECT * WHERE {{ ?s ?p ?o FILTER({}?o{}) }}",
        "STR(".repeat(BOUND - 2),
        ")".repeat(BOUND - 2)
    );
    for shared in [false, true] {
        issue_text(&kernel(shared), "x-select", calls.clone()).unwrap();
    }
}

#[test]
fn brackets_in_strings_iris_and_comments_are_not_nesting() {
    let deep = "(".repeat(500);
    let text = format!(
        "SELECT * WHERE {{ ?s ?p ?o # {deep}\n\
         FILTER(?o != \"{deep}\" && ?o != '''{deep}''' && ?o != <urn:x:{deep}>) }}"
    );
    let rows = issue_text(&kernel(true), "s-select", text).unwrap();
    assert!(rows.contains("urn:o"), "{rows}");
}

#[test]
fn a_less_than_that_hides_a_string_from_an_iri_reading_is_still_counted() {
    // `( ?a<'> ) ' && (…`: skip `<'>` as an IRI and the string the PARSER opens at that `'`
    // is invisible, so a naive scan sees one level where the parser sees one per repetition.
    let text = format!(
        "SELECT * WHERE {{ ?a ?p ?o FILTER({}true) }}",
        "( ?a<'> ) ' && ".repeat(200)
    );
    let err = issue_text(&kernel(false), "q-select", text).unwrap_err();
    assert!(err.to_string().contains("deeper than 64"), "{err}");
}

#[test]
fn a_query_past_the_byte_bound_is_refused_by_name_before_any_graph_is_resolved() {
    let values = "<urn:x> ".repeat((1 << 20) / 8 + 1);
    let text = format!("SELECT * WHERE {{ VALUES ?x {{ {values} }} }}");
    for form in FORMS {
        // `graph=` names a source nothing binds: refused for its size first, it is never
        // resolved, so the error is the bound and not `Unresolved`.
        let req = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:sparql:{form}")).unwrap(),
        )
        .with_arg("query", ArgRef::Inline(text.clone().into_bytes()))
        .with_arg("graph", ArgRef::Inline(b"urn:example:nowhere".to_vec()));
        let err = block_on(kernel(false).issue(req, &Capability::root()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`query`") && err.contains("1048576"),
            "{form}: {err}"
        );
    }
    let err = issue_text(
        &kernel(true),
        "s-update",
        format!("INSERT DATA {{ {values} }}"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("`content`") && err.contains("1048576"),
        "{err}"
    );
}

/// RELEASE BUILDS ONLY: the shapes the ikigai-store #915 survey measured aborting a release
/// build on a 2 MiB thread, each past that size, at both kinds of query door. Nested shapes
/// are refused; every flat one runs on the stack `on_sparql_stack` sizes for it. A debug
/// build spends ~20-50x the stack per level, so these sizes are past what it can guarantee
/// (see `src/limits.rs`).
///
///     cargo test --release -p ikigai-sparql --test sparql_nesting -- --ignored --nocapture
#[test]
#[ignore]
fn the_measured_release_aborts_are_refused_or_answered() {
    // Every probe runs and every failure is listed, so a run on an unfixed build is the
    // whole reproduction table rather than its first row.
    let mut cases: Vec<(&str, &str, usize, &str)> = Vec::new();
    for door in ["q-select", "s-select"] {
        cases.push((door, "parens", 1_000, "refused"));
        cases.push((door, "bang", 6_000, "refused"));
        // Not here, though both aborted a release build inline: a 5,000-step property path
        // (15 KB) and 2,000 triple patterns. On the sized stack neither aborts, and neither
        // finishes in reasonable time either: the path ran past ten minutes of a core and
        // the patterns past a minute before being stopped (2026-10-09). That is TIME, which
        // `src/budget.rs` now bounds (ledger #964), not a stack outcome to pin.
        //
        // Since that budget, these four are over `budget::MAX_ALGEBRA_NODES` too, so they are
        // refused AFTER parsing and before planning. What this pins is unchanged: the parse,
        // which recurses on all of them, runs on the sized stack and aborts nothing.
        for (shape, n) in [
            ("or1", 2_500),
            ("mul", 5_000),
            ("plus", 5_000),
            ("filters", 2_500),
        ] {
            cases.push((door, shape, n, "parsed"));
        }
    }
    cases.push(("s-update", "parens", 1_000, "refused"));
    cases.push(("s-update", "or", 2_500, "parsed"));
    let mut failures = Vec::new();
    for (door, shape, n, want) in cases {
        let started = std::time::Instant::now();
        let outcome = try_probe(door, shape, n);
        let took = started.elapsed();
        let good = match (&outcome, want) {
            (Ok(o), "ok") => o.starts_with("ok "),
            (Ok(o), "parsed") => {
                o.starts_with("ok ")
                    || (o.starts_with("err invalid argument") && o.contains("MAX_ALGEBRA_NODES"))
            }
            (Ok(o), _) => o.starts_with("err invalid argument") && o.contains("64"),
            (Err(_), _) => false,
        };
        let line: String = match &outcome {
            Ok(o) | Err(o) => o.chars().take(160).collect(),
        };
        println!("{door} {shape} {n} ({took:.1?}): {line}");
        if !good {
            failures.push(format!("{door} {shape} {n}: {line}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
