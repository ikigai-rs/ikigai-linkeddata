//! Every evaluation of caller SPARQL runs under a TIME budget, and stops when it expires
//! (ledger #964).
//!
//! The claim, measured on 0.1.11 with oxigraph 0.5.11 in a release build: a 1,000-step
//! property path (3 KB) took 19 s of a core, 200 triple patterns 7 s, and a three-way cross
//! product of the always-loaded vocabulary runs for as long as it is let. Through an anonymous
//! `urn:sparql:select` door, one request pinned a core for minutes.
//!
//! Every probe that could run long runs in a CHILD process (this binary re-executed with one
//! probe named in its environment), which the parent kills after [`HARD`]: on a build without
//! the budget, a probe reads as "still running when killed" instead of pinning CI.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_sparql::budget::{AnswerBound, MAX_ALGEBRA_NODES, MAX_JOIN_OPERANDS};
use ikigai_sparql::Store;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROBE: &str = "IKIGAI_SPARQL_BUDGET_PROBE";

/// How long the parent lets a child run before killing it.
const HARD: Duration = Duration::from_secs(60);

/// The budget the timing probes ask for, in milliseconds.
const BUDGET_MS: u64 = 400;

/// How far past its budget a stopped query may return and still count as stopped AT the
/// budget. Generous for a debug build on a loaded CI runner; the unbudgeted alternative is
/// "never", which is what [`HARD`] catches.
const SLACK: Duration = Duration::from_secs(4);

/// A cross product of the store with itself three ways — 470 vocabulary triples make 10^8
/// rows. Short, within every bound, and slow in the EVALUATOR, where the token reaches.
const PRODUCT: &str = "SELECT * WHERE { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }";
const PRODUCT_CONSTRUCT: &str = "CONSTRUCT { ?a ?b ?i } WHERE { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }";
/// The same product with a filter no row passes and no single pattern can take, so nothing
/// reaches this crate's serializer: only oxigraph's own token checks can stop it.
const PRODUCT_ASK: &str = "ASK { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i \
     FILTER(CONCAT(STR(?a), STR(?d), STR(?g)) = \"never\") }";

/// The claim's own shapes: a 5,000-step path (15 KB) and 2,000 triple patterns.
fn long_path(n: usize) -> String {
    format!(
        "PREFIX : <urn:> SELECT * WHERE {{ ?s :a{} ?o }}",
        "/:a".repeat(n)
    )
}

fn long_bgp(n: usize) -> String {
    format!(
        "SELECT * WHERE {{ {} }}",
        (0..n)
            .map(|i| format!("?s <urn:p> ?o{i} . "))
            .collect::<String>()
    )
}

/// The per-query space, or a shared store holding the vocabulary (the same 470 triples), each
/// with the given time ceiling, so the `budget=` a probe sends is what applies.
///
/// And with the answer-size bound at its CEILING (ledger #970): at the default bound the
/// products here are refused for their SIZE in a few hundred milliseconds, before the deadline
/// these tests are about (a debug build crossed 16 MiB at about 300 ms). A 400 ms product stays
/// far under 1 GiB, so the time budget is still what stops it.
fn kernel(shared: bool, ceiling: Duration) -> (Kernel, Arc<Store>) {
    let store = Arc::new(Store::new().unwrap());
    ikigai_sparql::load_vocabulary(&store).unwrap();
    let space = if shared {
        ikigai_sparql::space_with_store_and_bounds(
            Arc::clone(&store),
            ceiling,
            AnswerBound::CEILING,
        )
    } else {
        ikigai_sparql::space_with_bounds(ceiling, AnswerBound::CEILING)
    };
    (Kernel::new(Arc::new(space)), store)
}

fn query(
    kernel: &Kernel,
    form: &str,
    text: &str,
    budget_ms: Option<u64>,
) -> ikigai_core::Result<String> {
    let as_type: &[u8] = if form == "construct" {
        b"text/turtle"
    } else {
        b"text/csv"
    };
    let mut req = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:sparql:{form}")).unwrap(),
    )
    .with_arg("query", ArgRef::Inline(text.as_bytes().to_vec()))
    .with_arg("as", ArgRef::Inline(as_type.to_vec()));
    if let Some(ms) = budget_ms {
        req = req.with_arg("budget", ArgRef::Inline(ms.to_string().into_bytes()));
    }
    block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

fn update(kernel: &Kernel, text: &str, budget_ms: Option<u64>) -> ikigai_core::Result<String> {
    let mut req = Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap())
        .with_arg("content", ArgRef::Inline(text.as_bytes().to_vec()));
    if let Some(ms) = budget_ms {
        req = req.with_arg("budget", ArgRef::Inline(ms.to_string().into_bytes()));
    }
    block_on(kernel.issue(req, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// This process's CPU time so far, from `ps` (std has no getrusage). `ps -o time=` prints
/// `[[dd-]hh:]mm:ss` on Linux and `m:ss.cc` on macOS; both parse as `:`-separated fields.
fn cpu_time() -> Duration {
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().unwrap(), r.to_string()),
        None => (0.0, text.clone()),
    };
    let secs = rest
        .split(':')
        .rev()
        .zip([1.0, 60.0, 3600.0])
        .map(|(field, unit)| field.parse::<f64>().unwrap_or(0.0) * unit)
        .sum::<f64>();
    Duration::from_secs_f64(days * 86_400.0 + secs)
}

/// The child's half: inert unless a parent named a probe.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.splitn(3, ':');
    let door = parts.next().unwrap().to_string();
    let form = parts.next().unwrap().to_string();
    let which = parts.next().unwrap().to_string();
    let text = match which.as_str() {
        "product" if form == "construct" => PRODUCT_CONSTRUCT.to_string(),
        "product" if form == "ask" => PRODUCT_ASK.to_string(),
        "product" => PRODUCT.to_string(),
        "path" => long_path(5_000),
        "bgp" => long_bgp(2_000),
        other => panic!("no probe named {other}"),
    };
    let (kernel, _) = kernel(door == "shared", Duration::from_secs(600));
    let started = Instant::now();
    let outcome = query(&kernel, &form, &text, Some(BUDGET_MS));
    let took = started.elapsed();
    let line = match outcome {
        Ok(body) => format!("ok {}", body.len()),
        Err(Error::Timeout(m)) => format!("timeout {m}"),
        Err(e) => format!("err {e}"),
    };
    println!("\nTOOK {}", took.as_millis());
    println!("OUTCOME {}", line.replace('\n', " "));
    // The core is released: once the refused call has returned, this process burns no CPU.
    // A worker still running would add ~3 s here. (`ps` reports whole seconds on Linux, so an
    // idle process can read as one second across a tick; a spinning one reads at least two.)
    let before = cpu_time();
    std::thread::sleep(Duration::from_secs(3));
    println!("IDLE_CPU_MS {}", (cpu_time() - before).as_millis());
    // And the next query is answered promptly.
    let started = Instant::now();
    let next = query(&kernel, "ask", "ASK { ?s ?p ?o }", Some(BUDGET_MS));
    println!(
        "NEXT {} {}",
        started.elapsed().as_millis(),
        next.map(|b| b.contains("true")).unwrap_or(false)
    );
    std::process::exit(0);
}

struct Probe {
    took: Duration,
    outcome: String,
    idle_cpu: Duration,
    next_took: Duration,
    next_ok: bool,
}

fn probe(door: &str, form: &str, which: &str) -> Probe {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{door}:{form}:{which}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let deadline = Instant::now() + HARD;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the {which} probe through {door} {form} was still running after {HARD:?} and \
                 was killed: no budget stopped it"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Reaped, not just at EOF (field guide 9h).
    let status = child.wait().unwrap();
    let out = reader.join().unwrap();
    assert!(status.success(), "the probe died: {status}\n{out}");
    let field = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or_else(|| panic!("the probe reported no {key}: {out}"))
            .to_string()
    };
    let ms = |s: &str| Duration::from_millis(s.parse().unwrap());
    let next = field("NEXT");
    let (next_took, next_ok) = next.split_once(' ').unwrap();
    Probe {
        took: ms(&field("TOOK")),
        outcome: field("OUTCOME"),
        idle_cpu: ms(&field("IDLE_CPU_MS")),
        next_took: ms(next_took),
        next_ok: next_ok == "true",
    }
}

// ------------------------------------------------------------------ the reproduction

#[test]
fn a_query_slow_in_the_evaluator_is_stopped_at_its_budget_at_every_query_door() {
    let budget = Duration::from_millis(BUDGET_MS);
    for door in ["query", "shared"] {
        for form in ["select", "construct", "ask"] {
            let p = probe(door, form, "product");
            let at = format!("{door} {form}");
            println!(
                "{at}: stopped after {:?}, {:?} CPU idle after, next query {:?}",
                p.took, p.idle_cpu, p.next_took
            );
            assert!(
                p.outcome.starts_with("timeout ") && p.outcome.contains(&format!("{BUDGET_MS} ms")),
                "{at}: refused as a timeout naming the budget, never a partial answer: {}",
                p.outcome
            );
            assert!(
                p.took >= budget && p.took < budget + SLACK,
                "{at}: stopped near the budget, took {:?}",
                p.took
            );
            // The evaluation thread stopped: nothing burned CPU after the call returned.
            assert!(
                p.idle_cpu < Duration::from_millis(2_000),
                "{at}: {:?} of CPU while idle — a worker is still running",
                p.idle_cpu
            );
            assert!(
                p.next_ok && p.next_took < Duration::from_secs(2),
                "{at}: the next query was answered in {:?}",
                p.next_took
            );
        }
    }
}

#[test]
fn the_measured_planner_shapes_are_refused_before_planning() {
    // On 0.1.11 these planned for 10+ minutes and 1+ minute, where no token reaches. Now they
    // are refused by the algebra bound, at once, and plan nothing.
    for (which, door) in [("path", "query"), ("bgp", "shared")] {
        let p = probe(door, "select", which);
        assert!(
            p.outcome.starts_with("err invalid argument `query`")
                && p.outcome.contains(&MAX_JOIN_OPERANDS.to_string()),
            "{which}: {}",
            p.outcome
        );
        assert!(
            p.took < Duration::from_secs(2),
            "{which}: took {:?}",
            p.took
        );
        assert!(p.next_ok, "{which}: the next query was not answered");
    }
}

// ------------------------------------------------------------------ under the budget

#[test]
fn a_legitimate_query_under_the_budget_is_answered() {
    for shared in [false, true] {
        let (kernel, _) = kernel(shared, Duration::from_secs(10));
        // A subclass walk over the vocabulary, the shape real catalog queries have.
        let rows = query(
            &kernel,
            "select",
            "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             SELECT ?c WHERE { ?c rdfs:subClassOf* ?super } ORDER BY ?c",
            Some(BUDGET_MS),
        )
        .unwrap();
        assert!(rows.lines().count() > 10, "{rows}");
        // A product too small to reach the budget is answered in full.
        let rows = query(
            &kernel,
            "select",
            "SELECT (COUNT(*) AS ?n) WHERE { ?a ?b ?c . ?d <urn:none> ?f }",
            None,
        )
        .unwrap();
        assert!(rows.contains('0'), "{rows}");
    }
}

#[test]
fn the_ecosystems_largest_query_shape_is_inside_the_algebra_bounds() {
    // The work ledger's listing query is the largest the ecosystem runs (survey 2026-10-09):
    // a handful of base patterns, sixteen OPTIONALs, a COALESCE in ORDER BY. Stated here at
    // about three times its size.
    let optionals = (0..48)
        .map(|i| format!("OPTIONAL {{ ?i <urn:p{i}> ?v{i} . ?v{i} <urn:q> ?w{i} }} "))
        .collect::<String>();
    let base = (0..MAX_JOIN_OPERANDS)
        .map(|i| format!("?i <urn:b{i}> ?b{i} . "))
        .collect::<String>();
    let text = format!(
        "SELECT * WHERE {{ {base} {optionals} FILTER(?b0 != <urn:x> && ?b1 != <urn:y>) }} \
         ORDER BY DESC(COALESCE(?v0, ?v1)) LIMIT 500"
    );
    let (kernel, _) = kernel(false, Duration::from_secs(10));
    query(&kernel, "select", &text, None).unwrap();
}

// ------------------------------------------------------------------ who sets the budget

#[test]
fn a_caller_cannot_raise_the_budget_above_the_hosts_ceiling() {
    // The host built the space with a 300 ms ceiling; the caller asks for an hour.
    let (kernel, _) = kernel(true, Duration::from_millis(300));
    let started = Instant::now();
    let err = query(&kernel, "select", PRODUCT, Some(3_600_000)).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout(m) if m.contains("300 ms")),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_millis(300) + SLACK);
    // And without a `budget=` at all, the ceiling applies.
    let err = query(&kernel, "select", PRODUCT, None).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout(m) if m.contains("300 ms")),
        "{err}"
    );
}

#[test]
fn a_budget_that_is_not_positive_milliseconds_is_refused_by_name() {
    let (kernel, _) = kernel(true, Duration::from_secs(10));
    for bad in ["0", "-5", "1.5", "1s", ""] {
        let req = Request::new(Verb::Source, Iri::parse("urn:sparql:ask").unwrap())
            .with_arg("query", ArgRef::Inline(b"ASK {}".to_vec()))
            .with_arg("budget", ArgRef::Inline(bad.as_bytes().to_vec()));
        let err = block_on(kernel.issue(req, &Capability::root())).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "budget"),
            "{bad:?}: {err}"
        );
    }
}

// ------------------------------------------------------------------ updates

#[test]
fn an_update_stopped_at_its_budget_applies_nothing() {
    let (kernel, store) = kernel(true, Duration::from_secs(60));
    let before = store.len().unwrap();
    // The first operation is a write that would land at once; the second is a product that
    // reaches the deadline. (An update's default graph is the store's real one, which is
    // empty here: the vocabulary is a named graph, so the product names it.) One transaction: the stop takes the first write with it.
    let started = Instant::now();
    let err = update(
        &kernel,
        "INSERT DATA { <urn:budget:landed> <urn:p> <urn:o> } ; \
         INSERT { <urn:budget:derived> <urn:p> ?c } WHERE { GRAPH ?v { ?a ?b ?c . ?d ?e ?f . \
         ?g ?h ?i FILTER(CONCAT(STR(?a), STR(?d), STR(?g)) = \"never\") } }",
        Some(BUDGET_MS),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Timeout(m) if m.contains("nothing was applied")),
        "{err}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(BUDGET_MS) + SLACK,
        "{:?}",
        started.elapsed()
    );
    assert_eq!(store.len().unwrap(), before, "no partial write");
    let landed = query(&kernel, "ask", "ASK { <urn:budget:landed> ?p ?o }", None).unwrap();
    assert!(landed.contains("false"), "{landed}");
    // An update inside its budget still commits.
    let receipt = update(
        &kernel,
        "INSERT DATA { <urn:budget:landed> <urn:p> <urn:o> }",
        Some(BUDGET_MS),
    )
    .unwrap();
    assert!(receipt.starts_with("updated:"), "{receipt}");
    assert_eq!(store.len().unwrap(), before + 1);
}

#[test]
fn an_update_over_the_algebra_bounds_is_refused_before_it_runs() {
    let (kernel, store) = kernel(true, Duration::from_secs(10));
    let before = store.len().unwrap();
    let wide = format!(
        "INSERT DATA {{ <urn:x> <urn:p> <urn:o> }} ; INSERT {{ <urn:y> <urn:p> ?o0 }} WHERE {{ {} }}",
        (0..=MAX_JOIN_OPERANDS)
            .map(|i| format!("?s <urn:p> ?o{i} . "))
            .collect::<String>()
    );
    let err = update(&kernel, &wide, None).unwrap_err();
    assert!(
        matches!(&err, Error::InvalidArgument { name, detail } if name == "content"
            && detail.contains("MAX_JOIN_OPERANDS")),
        "{err}"
    );
    let many = format!(
        "DELETE {{ <urn:x> <urn:p> ?o }} WHERE {{ ?s ?p ?o FILTER({}) }}",
        (0..MAX_ALGEBRA_NODES)
            .map(|i| format!("?o = <urn:x{i}>"))
            .collect::<Vec<_>>()
            .join(" || ")
    );
    let err = update(&kernel, &many, None).unwrap_err();
    assert!(
        matches!(&err, Error::InvalidArgument { detail, .. } if detail.contains("MAX_ALGEBRA_NODES")),
        "{err}"
    );
    assert_eq!(store.len().unwrap(), before);
}
