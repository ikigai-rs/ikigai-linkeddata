//! Deep HTML is refused as over budget, and HTML at the bound is answered, on a small thread
//! (ledger #2674; `ikigai-jsonld/tests/json_depth.rs` is the pattern).
//!
//! Every probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in
//! its environment) on a 2 MiB thread, the size of a tokio worker's, so a stack overflow kills
//! the child and reads here as a failure with its signal named, not as the whole run aborting.
//!
//! The deepest probe is 700,000 nested elements (under 8 MB, inside the 8 MiB input bound):
//! the in-parse guard stops html5ever's tree builder a chunk past twice the bound, so its
//! scope checks over the stack of open elements never see 700,000 entries, and nothing
//! below recurses over the tree.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_html::MAX_HTML_DEPTH;
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_HTML_DEPTH_PROBE";

/// HTML of shape `name` with `n` nested elements, and the element depth it reaches.
fn shape(name: &str, n: usize) -> (String, usize) {
    let wrap = |open: &str, close: &str| format!("{}x{}", open.repeat(n), close.repeat(n));
    match name {
        // A document: html and body, then the divs.
        "div" => (
            format!(
                "<!DOCTYPE html><html><body>{}</body></html>",
                wrap("<div>", "</div>")
            ),
            n + 2,
        ),
        // Never closed: the tree builder closes them all at the end.
        "unclosed" => (format!("<body>{}", "<span>".repeat(n)), n + 2),
        // Formatting elements, which the tree builder also tracks in its active list.
        "b" => (format!("<body>{}", wrap("<b>", "</b>")), n + 2),
        other => panic!("no shape named {other}"),
    }
}

const SHAPES: [&str; 3] = ["div", "unclosed", "b"];

fn door(doc: String) -> String {
    let kernel = Kernel::new(Arc::new(ikigai_html::space()));
    let request = Request::new(Verb::Source, Iri::parse("urn:iki:html:graph").unwrap())
        .with_arg("content", ArgRef::Inline(doc.into_bytes()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(rep) => format!("ok {}", rep.bytes.len()),
        Err(ikigai_core::Error::OverBudget { detail }) => format!("over-budget {detail}"),
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
    let (name, n) = spec.split_once(':').unwrap();
    let (name, n) = (name.to_string(), n.parse::<usize>().unwrap());
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || door(shape(&name, n).0))
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {}", outcome.replace('\n', " "));
    std::process::exit(0);
}

/// Run one probe in a child and return its outcome line, failing on a crash.
fn run(name: &str, n: usize) -> String {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{name}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    match stdout.lines().find_map(|l| l.strip_prefix("OUTCOME ")) {
        Some(outcome) => outcome.to_string(),
        None => panic!(
            "{name} at {n}: the child died ({:?}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn html_at_the_bound_is_answered_and_one_past_it_refused() {
    for name in SHAPES {
        let at = MAX_HTML_DEPTH - (shape(name, 0).1);
        assert_eq!(shape(name, at).1, MAX_HTML_DEPTH);
        let ok = run(name, at);
        assert!(ok.starts_with("ok "), "{name} at the bound: {ok}");
        let past = run(name, at + 1);
        assert!(
            past.starts_with("over-budget ") && past.contains("deeper than 256"),
            "{name} past the bound: {past}"
        );
    }
}

#[test]
fn very_deep_html_is_refused_not_crashed() {
    for name in SHAPES {
        let outcome = run(name, 700_000);
        assert!(
            outcome.starts_with("over-budget ") && outcome.contains("deeper than 256"),
            "{name}: {outcome}"
        );
    }
}
