//! Nesting that is NOT a triple term and NOT JSON-LD does not abort a door (ledger #1326).
//!
//! The claim (ledger #1326, from ikigai-rs/ikigai-layers PR 10): a door runs requests on a
//! tokio worker with a 2 MiB stack, a size bound does not bound recursion depth, and a recursive
//! parser over deeply nested caller input aborts the whole process. This crate already bounds
//! the two recursions found earlier, RDF 1.2 triple terms (ledger #992,
//! `tests/triple_term_depth.rs`) and JSON-LD node objects (ledger #1038, the same file). This
//! file sweeps the OTHER ways the syntaxes this crate reads can nest, which no scan here
//! bounds, through every door that reads them, at a depth far past anything a bound would
//! admit:
//!
//! - Turtle (and so N-Triples, N-Quads and TriG, which reach the same parser): blank-node
//!   property lists `[ ex:p [ ex:p … ] ]`, collections `( ( ( … ) ) )`, and RDF 1.2 annotation
//!   blocks `{| ex:q ex:r {| … |} |}`. oxttl 0.2 parses with an explicit stack of states, and
//!   each of these builds blank nodes, never a nested term, so nothing recurses.
//! - RDF/XML: plain striped element nesting (`<ex:p><rdf:Description><ex:p>…`), an XML
//!   literal (`rdf:parseType="Literal"`) holding deeply nested elements, and nested
//!   `rdf:parseType="Resource"` property elements. oxrdfxml 0.2 keeps its open elements on a
//!   `Vec`, copies a literal's markup as text, and opens each `Resource` on a fresh blank node.
//!
//! Each probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in its
//! environment) on a 2 MiB thread, so an abort kills the child and reads here as a failure with
//! the signal named. The probe nests [`DEEP`] levels, so a pass is evidence the path is
//! iterative, not that it recurses shallowly enough. Every answer is checked to be a real one
//! (the graph came through), so a parse error cannot pass for survival.
//!
//! ⚠ RDF 1.2 is on (the `oxrdfio/rdf-12` dev-dependency), as it is in the CLI's build, so the
//! annotation syntax parses and the serializers run with triple-term support compiled in.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Verb,
};
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_RDF_STRUCTURAL_DEPTH_PROBE";

/// How deep each probe nests: 500 times the shared bound of 64, and far past where a
/// recursive descent dies on a 2 MiB thread (the JSON-LD expander died at 29 in a debug build).
const DEEP: usize = 32_000;

/// How deep the RDF/XML probes nest: 8,000, still 125 times the bound, and 256 bytes of a
/// 2 MiB stack a level, below any recursive frame of a debug build.
///
/// ⚠ Not [`DEEP`], because oxrdfxml 0.2.4 is QUADRATIC in element depth: for each element it
/// walks its whole stack of open elements, innermost first, looking for an inherited
/// `xml:lang`, base direction, `rdf:version` and base IRI (`current_language` and its
/// siblings in `parser.rs`), and finds none in a document that sets none. Measured through
/// `urn:rdf:transrept` in a debug build: 4,000 striped levels in 0.46 s, 8,000 in 1.8 s, so
/// 64,000 (2.6 MB) would take about two minutes. That is time, not stack, and nothing aborts;
/// an XML literal nests its markup as text and stays linear (8,000 levels in 0.02 s).
const DEEP_RDFXML: usize = 8_000;

/// A document in `syntax` that nests `levels` deep in the way the name says.
fn document(syntax: &str, levels: usize) -> String {
    match syntax {
        "bnode" => format!(
            "@prefix ex: <urn:ex:> .\nex:s ex:p {}ex:o{} .\n",
            "[ ex:p ".repeat(levels),
            " ]".repeat(levels)
        ),
        "collection" => format!(
            "@prefix ex: <urn:ex:> .\nex:s ex:p {}ex:o{} .\n",
            "( ".repeat(levels),
            " )".repeat(levels)
        ),
        "annotation" => format!(
            "@prefix ex: <urn:ex:> .\nex:s ex:p ex:o {}{} .\n",
            "{| ex:q ex:r ".repeat(levels),
            " |}".repeat(levels)
        ),
        "rdfxml-striped" => format!(
            "<?xml version=\"1.0\"?>\n<rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:ex:\">\
             <rdf:Description rdf:about=\"urn:ex:s\">{}<ex:p rdf:resource=\"urn:ex:o\"/>{}\
             </rdf:Description></rdf:RDF>\n",
            "<ex:p><rdf:Description>".repeat(levels),
            "</rdf:Description></ex:p>".repeat(levels)
        ),
        "rdfxml-literal" => format!(
            "<?xml version=\"1.0\"?>\n<rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:ex:\">\
             <rdf:Description rdf:about=\"urn:ex:s\"><ex:p rdf:parseType=\"Literal\">{}x{}\
             </ex:p></rdf:Description></rdf:RDF>\n",
            "<ex:a>".repeat(levels),
            "</ex:a>".repeat(levels)
        ),
        // `rdf:parseType="Resource"` nested: each level a fresh blank node, never a triple term
        // (ledger #2056: the depth scan used to count these, and refused this past 64).
        "rdfxml-resource" => format!(
            "<?xml version=\"1.0\"?>\n<rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:ex:\">\
             <rdf:Description rdf:about=\"urn:ex:s\">{}<ex:p rdf:resource=\"urn:ex:o\"/>{}\
             </rdf:Description></rdf:RDF>\n",
            "<ex:p rdf:parseType=\"Resource\">".repeat(levels),
            "</ex:p>".repeat(levels)
        ),
        other => panic!("no syntax named {other}"),
    }
}

const TURTLE: [&str; 3] = ["bnode", "collection", "annotation"];
const RDFXML: [&str; 3] = ["rdfxml-striped", "rdfxml-literal", "rdfxml-resource"];

/// The rdf space, sniff's space, and `urn:test:deep`, a resource whose body is the deep
/// document, so `with=` can name it and have the kernel resolve it.
fn kernel(body: String) -> Kernel {
    let deep = FnEndpoint::new("deep", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            body.clone().into_bytes(),
        ))
    })
    .with_description(Description::new("deep").verb(Verb::Source));
    let test = EndpointSpace::new().bind(Exact::new("urn:test:deep"), deep);
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_rdf::space()),
        Arc::new(ikigai_sniff::space()),
        Arc::new(test),
    ])))
}

const SMALL: &str = "<urn:a> <urn:b> <urn:c> .\n";

/// Issue one request at `door` carrying `text` where that door reads caller RDF.
fn issue(door: &str, text: String) -> ikigai_core::Result<String> {
    let kernel = kernel(text.clone());
    let req = |iri: &str| Request::new(Verb::Source, Iri::parse(iri).unwrap());
    let inline = |s: &str| ArgRef::Inline(s.as_bytes().to_vec());
    let transrept = |to: &str| {
        req("urn:rdf:transrept")
            .with_arg("content", inline(&text))
            .with_arg("as", inline(to))
    };
    let request = match door {
        "union-content" => req("urn:rdf:union")
            .with_arg("content", inline(&text))
            .with_arg("with", inline(SMALL)),
        "union-with" => req("urn:rdf:union")
            .with_arg("content", inline(SMALL))
            .with_arg("with", inline(&text)),
        "union-with-iri" => req("urn:rdf:union")
            .with_arg("content", inline(SMALL))
            .with_arg("with", inline("urn:test:deep")),
        "diff-content" => req("urn:rdf:diff")
            .with_arg("content", inline(&text))
            .with_arg("with", inline(SMALL)),
        "diff-with" => req("urn:rdf:diff")
            .with_arg("content", inline(SMALL))
            .with_arg("with", inline(&text))
            .with_arg("mode", inline("removed")),
        "diff-with-iri" => req("urn:rdf:diff")
            .with_arg("content", inline(SMALL))
            .with_arg("with", inline("urn:test:deep"))
            .with_arg("mode", inline("removed")),
        "transrept-turtle" => transrept("text/turtle"),
        "transrept-nt" => transrept("application/n-triples"),
        "transrept-trig" => transrept("application/trig"),
        "transrept-rdfxml" => transrept("application/rdf+xml"),
        "transrept-jsonld" => transrept("application/ld+json"),
        "transrept-html" => transrept("text/html"),
        "auto" => req("urn:transrept:auto")
            .with_arg("content", inline(&text))
            .with_arg("as", inline("application/n-triples")),
        other => panic!("no door named {other}"),
    };
    block_on(kernel.issue(request, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// Every door that reads Turtle (union and diff read only Turtle).
const TURTLE_DOORS: [&str; 13] = [
    "union-content",
    "union-with",
    "union-with-iri",
    "diff-content",
    "diff-with",
    "diff-with-iri",
    "transrept-turtle",
    "transrept-nt",
    "transrept-trig",
    "transrept-rdfxml",
    "transrept-jsonld",
    "transrept-html",
    "auto",
];

/// Every door that reads RDF/XML: the transreptor, every face, and sniff's auto chain.
const RDFXML_DOORS: [&str; 7] = [
    "transrept-turtle",
    "transrept-nt",
    "transrept-trig",
    "transrept-rdfxml",
    "transrept-jsonld",
    "transrept-html",
    "auto",
];

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, prints
/// the outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = spec.split(':');
    let door = parts.next().unwrap().to_string();
    let syntax = parts.next().unwrap().to_string();
    let n = parts.next().unwrap().parse::<usize>().unwrap();
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || match issue(&door, document(&syntax, n)) {
            // The answer's size, and whether the outermost subject came through, which is the
            // proof the parse finished (a syntax error would be `err`).
            Ok(text) => format!(
                "ok {} bytes, subject {}",
                text.len(),
                text.contains("urn:ex:s")
            ),
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
fn try_probe(door: &str, syntax: &str, n: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{door}:{syntax}:{n}"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "{syntax} nested {n} deep through `{door}` did not survive a 2 MiB thread: {} — {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("the `{door}` {syntax} probe reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

/// Whether `outcome` is the answer `syntax` at `door` should give: the graph, or, for an
/// annotation (which the parser turns into RDF 1.2 triple terms) written as JSON-LD, the
/// serializer's typed refusal, since oxjsonld 0.2 writes no RDF 1.2. A refusal is not an abort.
fn answered(syntax: &str, door: &str, outcome: &str) -> bool {
    if syntax == "annotation" && door == "transrept-jsonld" {
        return outcome.contains("JSON-LD does not support RDF 1.2");
    }
    outcome.starts_with("ok ") && outcome.ends_with("subject true")
}

/// Run every `syntax` × `door` probe at `levels` and fail naming each one that died or did not
/// answer.
fn sweep(syntaxes: &[&str], doors: &[&str], levels: usize) {
    let mut failed = Vec::new();
    for syntax in syntaxes {
        for door in doors {
            match try_probe(door, syntax, levels) {
                Ok(outcome) if answered(syntax, door, &outcome) => {}
                Ok(outcome) => failed.push(format!("{syntax} {door}: {outcome}")),
                Err(died) => failed.push(died),
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn deep_turtle_blank_nodes_collections_and_annotations_are_answered_at_every_door() {
    sweep(&TURTLE, &TURTLE_DOORS, DEEP);
}

/// At [`DEEP_RDFXML`], not [`DEEP`]: see its note on time.
#[test]
fn deep_rdfxml_elements_and_xml_literals_are_answered_by_the_transreptor() {
    sweep(&RDFXML, &RDFXML_DOORS, DEEP_RDFXML);
}

/// The shapes are what they say: each parses, at a small depth, into the graph expected of it.
/// Without this a typo in [`document`] could make every probe "survive" a syntax error, which
/// the sweep's `subject true` check also guards from the other side.
#[test]
fn every_shape_parses_at_a_small_depth() {
    for syntax in TURTLE.iter().chain(RDFXML.iter()) {
        let nt =
            issue("transrept-nt", document(syntax, 3)).unwrap_or_else(|e| panic!("{syntax}: {e}"));
        let triples = nt.lines().filter(|l| !l.trim().is_empty()).count();
        let expected = match *syntax {
            // ex:s ex:p _:1, _:1 ex:p _:2, _:2 ex:p _:3, _:3 ex:p ex:o
            "bnode" => 4,
            // ex:s ex:p _:l1; each list node has rdf:first and rdf:rest: 3 levels of 1-item
            // lists, the innermost holding ex:o
            "collection" => 7,
            // the asserted triple, then per level a reifier's rdf:reifies and its ex:q ex:r
            "annotation" => 1 + 3 * 2,
            // per level ex:p to a blank node, then the innermost ex:p ex:o
            "rdfxml-striped" => 4,
            "rdfxml-literal" => 1,
            // the same graph as striped nesting, each blank node opened by the property
            "rdfxml-resource" => 4,
            other => panic!("no syntax named {other}"),
        };
        assert_eq!(triples, expected, "{syntax}: {nt}");
    }
}
