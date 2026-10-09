//! The measurement behind `src/budget.rs` (ledger #964): where the TIME of an expensive SPARQL
//! query goes, phase by phase, and how long oxigraph's cancellation token takes to stop it once
//! fired. Kept so it can be re-run when oxigraph moves, because the answer decides what a
//! budget built on that token can and cannot stop.
//!
//!     cargo test --release -p ikigai-sparql --test sparql_time_measure -- --ignored --nocapture
//!     SHAPES="or1 path" SIZES="1000 4000" CANCEL_MS=500 HARD_S=60 cargo test --release …
//!
//! Every probe runs in a CHILD process (this binary re-executed) on a stack sized the way
//! `limits::on_sparql_stack` sizes it, and the parent kills the child after `HARD_S` seconds,
//! so no probe can pin a core past that. The child calls oxigraph DIRECTLY, beneath this
//! crate's doors, over a store holding the bundled ikigai vocabulary (what the per-query
//! dataset always holds), and prints one line per phase as it finishes it — so a killed probe
//! still says which phase it was in:
//!
//! - `parse`   — `SparqlEvaluator::parse_query` (spargebra);
//! - `plan`    — `.on_store(..).execute()`, which optimizes (sparopt) and builds the plan;
//! - `drain`   — iterating the results, where the evaluation actually happens.
//!
//! With `CANCEL_MS` set, a timer cancels the token that many milliseconds after the start and
//! the child reports how long after the cancel the work came back. oxigraph checks the token
//! only where the evaluator touches the dataset (a quad-pattern scan, a named-graph scan,
//! internalizing a term), so parse and plan are never interrupted; this measures what that
//! means in seconds.
//!
//! Measured 2026-10-09, oxigraph 0.5.11 (spareval 0.2.7), release, aarch64-apple-darwin: see
//! the table in `src/budget.rs`.

use oxigraph::sparql::{CancellationToken, QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ENV: &str = "IKIGAI_SPARQL_TIME_MEASURE";

fn join_width() -> usize {
    std::env::var("JOIN_W")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(31)
}

fn shape(name: &str, n: usize) -> String {
    let r = |s: &str, k: usize| s.repeat(k);
    match name {
        // The #915 measurement: quadratic in the chain.
        "or1" => format!("SELECT * WHERE {{ FILTER(1{}) }}", r("||1", n)),
        "or-row" => format!(
            "SELECT * WHERE {{ ?s ?p ?o FILTER(false{}) }}",
            r("||false", n)
        ),
        "plus" => format!("SELECT * WHERE {{ FILTER(1{} > 0) }}", r("+1", n)),
        // The #962 measurement: a sequence path of one-letter prefixed names.
        "path" => format!(
            "PREFIX : <urn:> SELECT * WHERE {{ ?s :a{} ?o }}",
            r("/:a", n)
        ),
        // A path over a predicate the vocabulary HAS, so every step matches something.
        "path-type" => format!(
            "SELECT * WHERE {{ ?s <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>{} ?o }}",
            r("/<http://www.w3.org/2000/01/rdf-schema#subClassOf>*", n)
        ),
        "bgp" => format!(
            "SELECT * WHERE {{ {} }}",
            (0..n)
                .map(|i| format!("?s <urn:p> ?o{i} . "))
                .collect::<String>()
        ),
        "bgp-any" => format!(
            "SELECT * WHERE {{ {} }}",
            (0..n)
                .map(|i| format!("?s{i} ?p{i} ?o{i} . "))
                .collect::<String>()
        ),
        // A small query that is slow in the EVALUATOR: a cross product of the vocabulary.
        "cross" => format!(
            "SELECT (COUNT(*) AS ?c) WHERE {{ {} }}",
            (0..n)
                .map(|i| format!("?s{i} ?p{i} ?o{i} . "))
                .collect::<String>()
        ),
        "filters" => format!("SELECT * WHERE {{ ?s ?p ?o {} }}", r("FILTER(true) ", n)),
        "union" => format!(
            "SELECT * WHERE {{ {{ ?s ?p ?o }}{} }}",
            r(" UNION { ?s ?p ?o }", n)
        ),
        "optional" => format!(
            "SELECT * WHERE {{ ?s ?p ?o {} }}",
            r("OPTIONAL { ?s ?p ?o } ", n)
        ),
        "groups" => format!("SELECT * WHERE {{ {} }}", r("{ ?s ?p ?o } ", n)),
        "minus" => format!(
            "SELECT * WHERE {{ ?s ?p ?o {} }}",
            r("MINUS { ?s <urn:p> ?o } ", n)
        ),
        "binds" => format!(
            "SELECT * WHERE {{ ?s ?p ?o {} }}",
            (0..n)
                .map(|i| format!("BIND(1 AS ?b{i}) "))
                .collect::<String>()
        ),
        "in" => format!(
            "SELECT * WHERE {{ ?s ?p ?o FILTER(?o IN ({})) }}",
            (0..n)
                .map(|i| format!("<urn:x{i}>"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "eqor" => format!(
            "SELECT * WHERE {{ ?s ?p ?o FILTER({}) }}",
            (0..n)
                .map(|i| format!("?o = <urn:x{i}>"))
                .collect::<Vec<_>>()
                .join(" || ")
        ),
        "values" => format!(
            "SELECT * WHERE {{ VALUES ?s {{ {} }} ?s ?p ?o }}",
            (0..n).map(|i| format!("<urn:x{i}> ")).collect::<String>()
        ),
        "alt" => format!("SELECT * WHERE {{ ?s <urn:p>{} ?o }}", r("|<urn:p>", n)),
        "star-seq" => format!("SELECT * WHERE {{ ?s (<urn:p>{})* ?o }}", r("/<urn:p>", n)),
        "subquery" => format!(
            "SELECT * WHERE {{ {} }}",
            r("{ SELECT ?s WHERE { ?s ?p ?o } LIMIT 1 } ", n)
        ),
        // n UNION branches of `JOIN_W` (default 31) joined patterns each: the worst a query
        // inside both algebra bounds can hand the planner (32 branches of 31 is ~1,023 nodes).
        "union-of-joins" => format!(
            "SELECT * WHERE {{ {} }}",
            (0..n)
                .map(|b| format!(
                    "{{ {} }}",
                    (0..join_width())
                        .map(|i| format!("?s{b} <urn:p> ?o{i} . "))
                        .collect::<String>()
                ))
                .collect::<Vec<_>>()
                .join(" UNION ")
        ),
        "optional-of-joins" => format!(
            "SELECT * WHERE {{ ?s ?p ?x {} }}",
            (0..n)
                .map(|b| format!(
                    "OPTIONAL {{ {} }} ",
                    (0..join_width())
                        .map(|i| format!("?s <urn:p{b}> ?o{b}x{i} . "))
                        .collect::<String>()
                ))
                .collect::<String>()
        ),
        // A real query over real data, for the legitimate side of the evidence: the text from
        // `QUERY_FILE`, run over `DATA_FILE` (see `probe`).
        "file" => std::fs::read_to_string(std::env::var("QUERY_FILE").unwrap()).unwrap(),
        other => panic!("unknown shape {other}"),
    }
}

fn line(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn probe(name: &str, n: usize, cancel_ms: Option<u64>) {
    let text = shape(name, n);
    line(&format!("BYTES {}", text.len()));
    let store = Store::new().unwrap();
    ikigai_sparql::load_vocabulary(&store).unwrap();
    if let Ok(path) = std::env::var("DATA_FILE") {
        // RDF/XML or Turtle by extension, with a base for relative IRIs (a Zotero export's).
        let format = if path.ends_with(".rdf") || path.ends_with(".xml") {
            oxigraph::io::RdfFormat::RdfXml
        } else {
            oxigraph::io::RdfFormat::Turtle
        };
        let loaded = Instant::now();
        store
            .load_from_slice(
                oxigraph::io::RdfParser::from_format(format)
                    .with_base_iri("http://zotero.local/")
                    .unwrap(),
                &std::fs::read(&path).unwrap(),
            )
            .unwrap();
        line(&format!(
            "LOADED {} quads in {:.0?}",
            store.len().unwrap(),
            loaded.elapsed()
        ));
    }
    let token = CancellationToken::new();
    let start = Instant::now();
    let cancelled_at = std::sync::Arc::new(std::sync::OnceLock::<Instant>::new());
    if let Some(ms) = cancel_ms {
        let (token, cancelled_at) = (token.clone(), cancelled_at.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = cancelled_at.set(Instant::now());
            token.cancel();
        });
    }
    let since_cancel = || {
        cancelled_at
            .get()
            .map(|at| format!(" ({:.0?} after the cancel)", at.elapsed()))
            .unwrap_or_default()
    };
    let parsed = SparqlEvaluator::new()
        .with_cancellation_token(token.clone())
        .parse_query(&text);
    line(&format!(
        "PHASE parse {:.0?}{}",
        start.elapsed(),
        since_cancel()
    ));
    let mut prepared = match parsed {
        Ok(p) => p,
        Err(e) => return line(&format!("OUTCOME parse-error {e}")),
    };
    prepared.dataset_mut().set_default_graph_as_union();
    let results = prepared.on_store(&store).execute();
    line(&format!(
        "PHASE plan {:.0?}{}",
        start.elapsed(),
        since_cancel()
    ));
    let outcome = match results {
        Err(e) => format!("eval-error {e}"),
        Ok(QueryResults::Solutions(solutions)) => {
            let mut rows = 0usize;
            let mut err = None;
            for s in solutions {
                match s {
                    Ok(_) => rows += 1,
                    Err(e) => {
                        err = Some(e.to_string());
                        break;
                    }
                }
            }
            match err {
                Some(e) => format!("drain-error after {rows} rows: {e}"),
                None => format!("rows {rows}"),
            }
        }
        Ok(QueryResults::Graph(triples)) => {
            let mut n = 0usize;
            for t in triples {
                if t.is_err() {
                    break;
                }
                n += 1;
            }
            format!("triples {n}")
        }
        Ok(_) => "boolean".to_string(),
    };
    line(&format!(
        "PHASE drain {:.0?}{}",
        start.elapsed(),
        since_cancel()
    ));
    line(&format!("OUTCOME {outcome}"));
}

#[test]
fn measure_child() {
    let Ok(spec) = std::env::var(ENV) else { return };
    let mut it = spec.split(':');
    let name = it.next().unwrap().to_string();
    let n = it.next().unwrap().parse::<usize>().unwrap();
    let cancel_ms = it.next().and_then(|s| s.parse::<u64>().ok());
    let bytes = shape(&name, n).len();
    std::thread::Builder::new()
        .stack_size((16 << 20) + bytes * 512)
        .spawn(move || probe(&name, n, cancel_ms))
        .unwrap()
        .join()
        .unwrap();
    std::process::exit(0);
}

/// Run one probe in a child, killing it after `hard`. Returns its lines and whether it was killed.
fn child(name: &str, n: usize, cancel_ms: Option<u64>, hard: Duration) -> (Vec<String>, bool) {
    let spec = match cancel_ms {
        Some(ms) => format!("{name}:{n}:{ms}"),
        None => format!("{name}:{n}"),
    };
    let mut proc_ = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "measure_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV, spec)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = proc_.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let deadline = Instant::now() + hard;
    let mut killed = false;
    loop {
        if proc_.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = proc_.kill();
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = proc_.wait();
    let out = reader.join().unwrap();
    let lines = out
        .lines()
        .filter(|l| {
            ["PHASE", "OUTCOME", "BYTES", "LOADED"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .map(|l| l.chars().take(160).collect())
        .collect();
    (lines, killed)
}

#[test]
#[ignore]
fn measure_phases_and_cancellation() {
    let shapes = std::env::var("SHAPES").unwrap_or_else(|_| "or1 path bgp cross".into());
    let sizes = std::env::var("SIZES").unwrap_or_else(|_| "500 1000 2000".into());
    let cancel_ms = std::env::var("CANCEL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let hard = Duration::from_secs(
        std::env::var("HARD_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30),
    );
    for name in shapes.split_whitespace() {
        for n in sizes.split_whitespace() {
            let n: usize = n.parse().unwrap();
            let started = Instant::now();
            let (lines, killed) = child(name, n, cancel_ms, hard);
            println!(
                "MEASURE shape={name} n={n} cancel_ms={cancel_ms:?} wall={:.1?}{} :: {}",
                started.elapsed(),
                if killed { " KILLED" } else { "" },
                lines.join(" | ")
            );
        }
    }
}
