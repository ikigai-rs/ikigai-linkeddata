//! A closure request inside the DEFAULT bounds can hold a core for minutes and take gigabytes
//! before the output bound refuses it (ledger #3433). This is the reproduction; it FAILS on
//! this crate today, so it is `#[ignore]`d and run by hand:
//!
//!     cargo test --release -p ikigai-reason --test closure_expansion -- --ignored --nocapture
//!
//! The input bound (1,000 triples) is checked before `reason()`, and the output bound
//! (100,000 triples) after it, because reasonable 0.4.4 cannot be interrupted or capped from
//! outside. Equality is what gets past both: reasonable materializes an `owl:sameAs` class of
//! `k` members as all `k²` pairs and copies every triple about a member to every other member,
//! so time grows about with the cube of the class. Measured on reasonable 0.4.4, release build,
//! x86_64 Linux, `urn:reason:closure` through `space()` (the defaults), wall clock and peak RSS
//! of the whole test process:
//!
//! | shape | input triples | Turtle | outcome | time | peak RSS |
//! | --- | --- | --- | --- | --- | --- |
//! | `owl:sameAs` chain | 201 | ~6 KB | answered | 2.2 s | |
//! | `owl:sameAs` chain | 401 | ~12 KB | `OverBudget` | 19 s | |
//! | `owl:sameAs` chain | 801 | ~23 KB | `OverBudget` | 167 s | 9.4 GB |
//! | `owl:FunctionalProperty`, 400 values | 402 | ~9 KB | `OverBudget` | 11 s | 3.4 GB |
//! | `owl:TransitiveProperty` chain | 1,000 | ~23 KB | `OverBudget` | 8 s | 1.7 GB |
//! | symmetric and transitive chain | 401 | ~9 KB | `OverBudget` | 2.8 s | 0.5 GB |
//!
//! A debug build is about ten times slower (the 201-member chain took 24 s). The functional
//! property row is why a pre-scan of ASSERTED `owl:sameAs` would not close this: equality is
//! also inferred (`prp-fp`, `prp-ifp`, `prp-key`, `cls-maxc2`, ...), from schema the rules
//! themselves derive. Closing it needs a reasoner that checks a deadline or an output count
//! inside its fixpoint loop.
//!
//! Each probe runs in a CHILD process (this binary re-executed with the probe named in its
//! environment), which the parent kills after [`HARD`], so a run that does not stop reads as a
//! failure here instead of pinning the machine.

// Only the kernel half of the shared fixtures is used here.
#[allow(dead_code)]
mod common;

use common::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROBE: &str = "IKIGAI_REASON_EXPANSION_PROBE";

/// How long a closure inside the default bounds may take, answer or refusal, before the
/// parent kills it. Generous: the largest legitimate fixture here takes well under a second.
const HARD: Duration = Duration::from_secs(30);

/// The probe's input: under 1,000 triples (the default input bound) in every shape.
fn shape(name: &str) -> String {
    let mut ttl = String::new();
    match name {
        // 801 triples: a chain of 800 `owl:sameAs` and one triple on its head.
        "same-as" => {
            for i in 0..800 {
                ttl += &format!("ex:n{i} owl:sameAs ex:n{} .\n", i + 1);
            }
            ttl += "ex:n0 ex:q ex:n0 .\n";
        }
        // 802 triples and no `owl:sameAs` at all: 800 values of a functional property are
        // inferred equal.
        "functional" => {
            ttl += "ex:f a owl:FunctionalProperty .\nex:a ex:q ex:a .\n";
            for i in 0..800 {
                ttl += &format!("ex:a ex:f ex:n{i} .\n");
            }
        }
        other => panic!("no probe named {other}"),
    }
    ttl
}

/// The child's half: inert unless a parent named a probe.
#[test]
fn probe_child() {
    let Ok(name) = std::env::var(PROBE) else {
        return;
    };
    let ttl = shape(&name);
    let kernel = with_graph(&ttl);
    let started = Instant::now();
    let outcome = match issue(&kernel, "closure", &[("graph", "urn:test:graph")]) {
        Ok(rep) => format!("ok {} bytes", rep.bytes.len()),
        Err(e) => format!("err {e}"),
    };
    println!("\nTOOK {} OUTCOME {outcome}", started.elapsed().as_millis());
    std::process::exit(0);
}

fn probe(name: &str) -> Result<String, Duration> {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, name)
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
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > HARD {
            let _ = child.kill();
            let _ = child.wait();
            return Err(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    let out = reader.join().unwrap();
    assert!(status.success(), "the {name} probe died: {status}\n{out}");
    Ok(out
        .lines()
        .find(|l| l.starts_with("TOOK "))
        .unwrap_or_else(|| panic!("the {name} probe reported nothing: {out}"))
        .to_string())
}

#[test]
#[ignore = "reproduction of ledger #3433: fails until the reasoner can be stopped inside its loop"]
fn a_closure_inside_the_default_bounds_finishes_promptly() {
    let mut held = Vec::new();
    for name in ["same-as", "functional"] {
        let triples = shape(name).lines().count();
        match probe(name) {
            Ok(line) => println!("{name} ({triples} input triples): {line}"),
            Err(after) => {
                println!("{name} ({triples} input triples): still reasoning after {after:?}");
                held.push(name);
            }
        }
    }
    assert!(
        held.is_empty(),
        "inputs inside the default 1,000-triple bound held the reasoner past {HARD:?}: {held:?}"
    );
}
