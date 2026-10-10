//! The measurement behind `src/depth.rs`'s JSON bound (ledger #1038), kept so it can be re-run
//! when oxjsonld moves: how deep each JSON-LD shape goes before the process aborts, on a thread
//! of a given size, through the parser alone (`DOORS=parse`, oxrdfio directly, BENEATH this
//! crate's bound) and through the kernel doors (`transrept`, `auto`), which refuse past the
//! bound once it exists, so measure those against the code that predates it.
//!
//!     cargo test --release --test jsonld_depth_measure -- --ignored --nocapture
//!     SHAPES="nodes lists" DOORS="parse transrept" STACK_MIB=2 CAP=20000 \
//!         cargo test --test jsonld_depth_measure -- --ignored --nocapture
//!
//! ⚠ **Mind the memory before raising `STACK_MIB` and `CAP` together.** The expander re-buffers
//! a node's content once per level, so memory grows with the square of the depth: 8,000 levels
//! took 5.2 GB in a release build, and a 64 MiB run reached ~32,000 levels and passed 33 GB
//! before it was stopped. At 2 MiB the abort comes first, which is what keeps the defaults safe.
//!
//! Every probe runs in a child process, this binary re-executed, so an overflow aborts the
//! child and never the measurement. The search doubles `n` until a probe aborts (or `CAP`),
//! then bisects, and prints the largest `n` that survived and the JSON depth of that document
//! (`{` and `[` alike; no shape here puts a bracket in a string).
//!
//! Measured 2026-10-10, oxjsonld 0.2.6 (oxrdfio 0.2.6), aarch64-apple-darwin: the table in
//! `src/depth.rs` is this file's output. Through the doors, before the bound, a debug build
//! aborted two levels sooner than `parse` and a release one about five (the kernel's frames).

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Fallback, Iri, Kernel, Request, Verb};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use std::process::Command;
use std::sync::Arc;

const ENV: &str = "IKIGAI_RDF_JSONLD_MEASURE";

/// A JSON-LD document of shape `name`, `n` levels deep.
fn shape(name: &str, n: usize) -> String {
    let r = |s: &str, k: usize| s.repeat(k);
    match name {
        // Node objects as property values: `{"@id":…,"p":{"@id":…,"p":{…}}}`. One JSON level a
        // level. The shape the ledger item reports.
        "nodes" => format!(
            "{}{{\"@id\":\"urn:ex:o\"}}{}",
            r("{\"@id\":\"urn:ex:s\",\"urn:ex:p\":", n),
            r("}", n)
        ),
        // `nodes` around an innermost node carrying 50,000 values (~1.3 MB): what each level
        // costs in MEMORY, since the expander buffers a node's whole content once per level.
        "nodes-wide" => format!(
            "{}{{\"@id\":\"urn:ex:o\",\"urn:ex:q\":[{}]}}{}",
            r("{\"@id\":\"urn:ex:s\",\"urn:ex:p\":", n),
            vec!["\"abcdefghijklmnopqrstuvw\""; 50_000].join(","),
            r("}", n)
        ),
        // The same without `@id`: nested blank nodes.
        "bnodes" => format!(
            "{}{{\"urn:ex:q\":\"x\"}}{}",
            r("{\"urn:ex:p\":", n),
            r("}", n)
        ),
        // Node objects each wrapped in an array, as compacted JSON-LD often writes a property
        // value: two JSON levels a level.
        "node-arrays" => format!(
            "{}{{\"@id\":\"urn:ex:o\"}}{}",
            r("{\"@id\":\"urn:ex:s\",\"urn:ex:p\":[", n),
            r("]}", n)
        ),
        // Arrays of arrays as a property value (flattened by expansion).
        "arrays" => format!(
            "{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":{}\"x\"{}}}",
            r("[", n),
            r("]", n)
        ),
        // A top-level array of arrays around one node.
        "top-arrays" => format!(
            "{}{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":\"x\"}}{}",
            r("[", n),
            r("]", n)
        ),
        // Lists of lists (JSON-LD 1.1): arrays nested inside `@list`.
        "lists" => format!(
            "{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":{{\"@list\":{}\"x\"{}}}}}",
            r("[", n),
            r("]", n)
        ),
        // `@list` objects nested in `@list` objects.
        "list-objects" => format!(
            "{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":{}\"x\"{}}}",
            r("{\"@list\":[", n),
            r("]}", n)
        ),
        // Keys that are not IRIs and expand to nothing, so they are dropped: the expander still
        // reads every level.
        "dropped" => format!(
            "{{\"@id\":\"urn:ex:s\",{}\"k\":1{}}}",
            r("\"k\":{", n),
            r("}", n)
        ),
        // Scoped contexts nested in term definitions: `"p":{"@id":…,"@context":{"p":{…}}}`.
        // Two JSON levels a level.
        "contexts" => format!(
            "{{\"@context\":{}{{}}{},\"@id\":\"urn:ex:s\",\"p\":\"x\"}}",
            r("{\"p\":{\"@id\":\"urn:ex:p\",\"@context\":", n),
            r("}}", n)
        ),
        // Arrays nested inside `@context`.
        "context-arrays" => format!(
            "{{\"@context\":{}{{\"p\":\"urn:ex:p\"}}{},\"@id\":\"urn:ex:s\",\"p\":\"x\"}}",
            r("[", n),
            r("]", n)
        ),
        // A JSON literal (`@type: @json`) nesting arrays: canonicalized, not expanded.
        "json-literal" => format!(
            "{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":{{\"@type\":\"@json\",\"@value\":{}1{}}}}}",
            r("[", n),
            r("]", n)
        ),
        // `@reverse` objects nested: `{"@reverse":{"p":{"@reverse":{…}}}}`.
        "reverse" => format!(
            "{}{{\"@id\":\"urn:ex:o\"}}{}",
            r("{\"@id\":\"urn:ex:s\",\"@reverse\":{\"urn:ex:p\":", n),
            r("}}", n)
        ),
        // Named graphs nested: `{"@id":…,"@graph":[{"@id":…,"@graph":[…]}]}`.
        "graphs" => format!(
            "{}{{\"@id\":\"urn:ex:o\",\"urn:ex:p\":\"x\"}}{}",
            r("{\"@id\":\"urn:ex:g\",\"@graph\":[", n),
            r("]}", n)
        ),
        other => panic!("no shape named {other}"),
    }
}

const SHAPES: [&str; 13] = [
    "nodes",
    "bnodes",
    "node-arrays",
    "arrays",
    "top-arrays",
    "lists",
    "list-objects",
    "dropped",
    "contexts",
    "context-arrays",
    "json-literal",
    "reverse",
    "graphs",
];

/// Parse `doc` as JSON-LD and serialize it as N-Quads, as `urn:rdf:transrept` does, with no
/// bound in front: what the parser does with it.
fn parse(doc: &str) -> String {
    let mut out = Vec::new();
    let mut serializer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    let format = RdfFormat::from_media_type("application/ld+json").unwrap();
    for quad in RdfParser::from_format(format).for_slice(doc.as_bytes()) {
        match quad {
            Ok(quad) => {
                if serializer.serialize_quad(&quad).is_err() {
                    return "err serialize".to_string();
                }
            }
            Err(e) => return format!("err {e}"),
        }
    }
    let _ = serializer.finish();
    format!("ok {} bytes", out.len())
}

/// Through a kernel door, as a caller reaches it.
fn door(name: &str, doc: &str) -> String {
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_rdf::space()),
        Arc::new(ikigai_sniff::space()),
    ])));
    let iri = match name {
        "transrept" => "urn:rdf:transrept",
        "auto" => "urn:transrept:auto",
        other => panic!("no door named {other}"),
    };
    let request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
        .with_arg("content", ArgRef::Inline(doc.as_bytes().to_vec()))
        .with_arg("as", ArgRef::Inline(b"application/n-quads".to_vec()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(rep) => format!("ok {} bytes", rep.bytes.len()),
        Err(e) => format!(
            "err {}",
            e.to_string().chars().take(160).collect::<String>()
        ),
    }
}

#[test]
fn measure_child() {
    let Ok(spec) = std::env::var(ENV) else {
        return;
    };
    let mut parts = spec.split(':');
    let which = parts.next().unwrap().to_string();
    let name = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let mib = parts.next().unwrap().parse::<usize>().unwrap();
    let doc = shape(&name, n);
    let outcome = std::thread::Builder::new()
        .stack_size(mib << 20)
        .spawn(move || {
            if which == "parse" {
                parse(&doc)
            } else {
                door(&which, &doc)
            }
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {}", outcome.replace('\n', " "));
    std::process::exit(0);
}

/// `Some(outcome)` if the child survived, `None` if it aborted.
fn child(which: &str, name: &str, n: usize, mib: usize) -> Option<String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "measure_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV, format!("{which}:{name}:{n}:{mib}"))
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let at = stdout.find("\nOUTCOME ")?;
    Some(
        stdout[at + "\nOUTCOME ".len()..]
            .lines()
            .next()
            .unwrap_or("")
            .to_string(),
    )
}

/// How deep `doc` nests `{` and `[`. The shapes carry no bracket inside a string.
fn json_depth(doc: &[u8]) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    for &b in doc {
        match b {
            b'{' | b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b'}' | b']' => depth -= 1,
            _ => {}
        }
    }
    deepest
}

fn env_list(key: &str, default: &[&str]) -> Vec<String> {
    std::env::var(key)
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|_| default.iter().map(|s| s.to_string()).collect())
}

#[test]
#[ignore = "a measurement, run by hand: see the header"]
fn measure_jsonld_overflow_depths() {
    let shapes = env_list("SHAPES", &SHAPES);
    let doors = env_list("DOORS", &["parse"]);
    let mib: usize = std::env::var("STACK_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let cap: usize = std::env::var("CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("\n{build}, {mib} MiB thread, cap {cap}");
    for which in &doors {
        for name in &shapes {
            // Double until a probe aborts or the cap, then bisect between the last survivor
            // and the first abort.
            let mut good = 0usize;
            let mut last = String::new();
            let mut n = 1usize;
            let mut bad = None;
            while n <= cap {
                match child(which, name, n, mib) {
                    Some(outcome) => {
                        good = n;
                        last = outcome;
                        n *= 2;
                    }
                    None => {
                        bad = Some(n);
                        break;
                    }
                }
            }
            if let Some(mut hi) = bad {
                while hi - good > 1 {
                    let mid = (good + hi) / 2;
                    match child(which, name, mid, mib) {
                        Some(outcome) => {
                            good = mid;
                            last = outcome;
                        }
                        None => hi = mid,
                    }
                }
            }
            let depth = json_depth(shape(name, good.max(1)).as_bytes());
            let verdict = match bad {
                Some(_) => format!("survives {good}, aborts {}", good + 1),
                None => format!("survives ≥ {good} (no abort to the cap)"),
            };
            println!(
                "{which:9} {name:14} {verdict:34} json depth {depth:6}  {}",
                last.chars().take(90).collect::<String>()
            );
        }
    }
}
