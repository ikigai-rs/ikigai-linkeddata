//! Deep caller XML is refused before feed-rs sees it, and a feed at the bound is answered on a
//! small thread (ledger #2624; `ikigai-jsonld/tests/json_depth.rs` is the pattern).
//!
//! Every probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in
//! its environment) on a 2 MiB thread, the size of a tokio worker's, so a stack overflow kills
//! the child and reads here as a failure with its signal named, not as the whole run aborting.
//!
//! What was measured: feed-rs 3.0 alone parses 65,000 nested levels of each shape below on that
//! thread (its element walk keeps a depth counter, and quick-xml refuses past 65,535), so the
//! parse itself is iterative for them; `feed_rs_alone_survives_deep_shapes` keeps that true.
//! The pre-scan's bound of [`MAX_FEED_DEPTH`] is the field guide's rule all the same: request
//! input is bounded before a parse, so a parser update that recursed could not reach the stack.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_feed::MAX_FEED_DEPTH;
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_FEED_DEPTH_PROBE";

/// A feed of shape `name` with `n` nested elements in its one entry. The entry's own fields
/// sit at depth 3, so `n` nested levels reach depth `fixed(name) + n`.
fn shape(name: &str, n: usize) -> String {
    let wrap = |open: &str, close: &str| format!("{}x{}", open.repeat(n), close.repeat(n));
    let entry = match name {
        // XHTML content: feed, entry, content, div, then the spans.
        "xhtml" => format!(
            "<content type=\"xhtml\"><div xmlns=\"http://www.w3.org/1999/xhtml\">{}</div></content>",
            wrap("<span>", "</span>")
        ),
        // Extension markup the parser skips: feed, entry, then the elements.
        "unknown" => wrap("<x>", "</x>"),
        // Atom's own `source`, which the entry handler knows: feed, entry, then the sources.
        "source" => wrap("<source>", "</source>"),
        other => panic!("no shape named {other}"),
    };
    format!(
        "<feed xmlns=\"http://www.w3.org/2005/Atom\"><id>tag:example.org,2026:f</id>\
         <entry><id>tag:example.org,2026:e</id><title>deep</title>{entry}</entry></feed>"
    )
}

/// The depth the shape's wrapper reaches before its `n` levels.
fn fixed(name: &str) -> usize {
    match name {
        "xhtml" => 4,
        _ => 2,
    }
}

const SHAPES: [&str; 3] = ["xhtml", "unknown", "source"];

fn door(doc: String) -> String {
    let kernel = Kernel::new(Arc::new(ikigai_feed::space()));
    let request = Request::new(Verb::Source, Iri::parse("urn:feed:graph").unwrap())
        .with_arg("content", ArgRef::Inline(doc.into_bytes()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(rep) => format!("ok {}", rep.bytes.len()),
        Err(e) => format!("err {e}"),
    }
}

fn feed_rs(doc: String) -> String {
    match feed_rs::parser::parse(doc.as_bytes()) {
        Ok(feed) => format!("ok {}", feed.entries.len()),
        Err(e) => format!("err {e}"),
    }
}

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, prints
/// the outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.split(':');
    let via = parts.next().unwrap().to_string();
    let name = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let doc = shape(&name, n);
            match via.as_str() {
                "door" => door(doc),
                "feed-rs" => feed_rs(doc),
                other => panic!("no probe named {other}"),
            }
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {}", outcome.replace('\n', " "));
    std::process::exit(0);
}

/// Run one probe in a child and return its outcome line, failing on a crash.
fn run(via: &str, name: &str, n: usize) -> String {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{via}:{name}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    match stdout.lines().find_map(|l| l.strip_prefix("OUTCOME ")) {
        Some(outcome) => outcome.to_string(),
        None => panic!(
            "{via} {name} at {n}: the child died ({:?}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn a_feed_at_the_bound_is_answered_and_one_past_it_refused() {
    for name in SHAPES {
        let at = MAX_FEED_DEPTH - fixed(name);
        let ok = run("door", name, at);
        assert!(ok.starts_with("ok "), "{name} at the bound: {ok}");
        let past = run("door", name, at + 1);
        assert!(
            past.contains("deeper than 64"),
            "{name} past the bound: {past}"
        );
    }
}

#[test]
fn a_very_deep_feed_is_refused_not_crashed() {
    for name in SHAPES {
        // 100,000 levels: deep, and inside the size bound (which would refuse it first otherwise).
        let outcome = run("door", name, 100_000);
        assert!(outcome.contains("deeper than 64"), "{name}: {outcome}");
    }
}

/// The measurement this file's docs cite, kept as a test so a feed-rs that starts recursing
/// on these shapes is noticed: 65,000 levels on a 2 MiB thread, parsed without a crash.
#[test]
fn feed_rs_alone_survives_deep_shapes() {
    for name in SHAPES {
        let outcome = run("feed-rs", name, 65_000);
        assert!(outcome.starts_with("ok "), "{name}: {outcome}");
    }
}
