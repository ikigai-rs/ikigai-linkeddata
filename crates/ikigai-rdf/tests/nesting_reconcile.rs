#![cfg(feature = "transrept")]

//! Where ikigai-rdf's nesting scans read caller RDF differently from ikigai-store's own copy,
//! which reading is right, decided by reproduction (ledger #2056, from ikigai-rs/ikigai-store
//! PR 68, which switched the store to these scans and failed five of its tests).
//!
//! 1. **N-Triples and N-Quads are scanned as Turtle**, where `'` opens a string; oxttl reads them
//!    in its N-Triples mode, where it does not. So `<urn:a> <urn:q> 'x . <urn:a> <urn:p> <<( … )>>
//!    . # '` hides its triple term from the scan. The probes below show the strict parser stops
//!    at the `'` with an error, on the same line or with the term on the next one, so a consumer
//!    that stops at the first error (every door here, ikigai-sparql's, and oxigraph's
//!    `Store::load_from_reader`) builds nothing deep. The CONTROL shows the other half: the parser
//!    resumes at the next line after an error, and a consumer that reads on past it aborts.
//! 2. **UTF-16 RDF/XML is not refused as such** by `check_rdfxml_nesting` (the store refuses a NUL
//!    byte or a UTF-16 byte order mark). The probes show oxrdfxml fails at the first element of a
//!    UTF-16 document, with or without a byte order mark, so nothing nested is built. (With quick-xml's
//!    `encoding` feature on, measured outside this repo against oxrdfio 0.2.6 / oxrdfxml 0.2.4 /
//!    quick-xml 0.37.5, the parser fails at the first element too, 3,000 levels deep: "XML
//!    namespaces are required in RDF/XML" or "cannot decode input using UTF-16BE/LE".)
//! 3. **`rdf:parseType="Resource"`** is the third, and the only one where ikigai-rdf was wrong
//!    (too strict): see `tests/structural_depth.rs`, which now sweeps it 8,000 deep at every door.
//!
//! Every deep probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in
//! its environment) on a 2 MiB thread, a tokio worker's stack, so an abort kills the child and
//! reads here as a failure with its status. RDF 1.2 is on (the `oxrdfio/rdf-12`
//! dev-dependency), as it is in the CLI's build.

use ikigai_rdf::{check_rdfxml_nesting, check_turtle_nesting};
use oxrdfio::{RdfFormat, RdfParser};
use std::process::Command;

const PROBE: &str = "IKIGAI_RDF_NESTING_RECONCILE_PROBE";

/// Far past where the parser's recursion dies on a 2 MiB thread (3,000 levels aborted a debug
/// build, `tests/triple_term_depth.rs`), so a release build of these tests holds too.
const LETHAL: usize = 30_000;

/// RDF/XML at 3,000 levels, which aborted a debug build in UTF-8 (`tests/triple_term_depth.rs`).
/// oxrdfxml is quadratic in element depth, so not [`LETHAL`].
const LETHAL_RDFXML: usize = 3_000;

fn nested(levels: usize) -> String {
    format!(
        "{}<urn:ex:o>{}",
        "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " )>>".repeat(levels)
    )
}

/// The case-1 documents: a `'` that opens a string to the Turtle reading and is a syntax error
/// to the N-Triples one, then a deep triple term on the same line or the next, then the `'` that
/// closes the Turtle reading's string.
fn hidden(format: &str, line: &str, levels: usize) -> String {
    let graph = if format == "nq" { " <urn:ex:g>" } else { "" };
    let sep = if line == "same" { " " } else { "\n" };
    format!(
        "<urn:ex:s> <urn:ex:q> 'x{graph} .{sep}<urn:ex:s> <urn:ex:p> {}{graph} . # '\n",
        nested(levels)
    )
}

/// The case-2 document: triple terms nested `levels` deep in RDF/XML, the outermost annotated so
/// the parser copies the whole term, in `encoding`.
fn rdfxml(levels: usize, encoding: &str) -> Vec<u8> {
    let element = |extra: &str| {
        format!("<ex:p rdf:parseType=\"Triple\"{extra}><rdf:Description rdf:about=\"urn:ex:s\">")
    };
    let open = if levels == 0 {
        String::new()
    } else {
        element(" rdf:annotation=\"urn:ex:a\"") + &element("").repeat(levels - 1)
    };
    let text = format!(
        "<?xml version=\"1.0\"?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
         xmlns:ex=\"urn:ex:\" rdf:version=\"1.2\"><rdf:Description rdf:about=\"urn:ex:s\">{open}\
         <ex:p rdf:resource=\"urn:ex:o\"/>{}</rdf:Description></rdf:RDF>\n",
        "</rdf:Description></ex:p>".repeat(levels)
    );
    let units = |le: bool| -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|u| if le { u.to_le_bytes() } else { u.to_be_bytes() })
            .collect()
    };
    match encoding {
        "utf8" => text.into_bytes(),
        "utf16le" => units(true),
        "utf16le-bom" => [vec![0xFF, 0xFE], units(true)].concat(),
        "utf16be-bom" => [vec![0xFE, 0xFF], units(false)].concat(),
        other => panic!("no encoding named {other}"),
    }
}

/// Parse `bytes` strict, as every door here does. `stop` stops at the first error, as the doors
/// do; otherwise errors are skipped and parsing goes on. Answers how many quads came out and the
/// first error.
fn parse(format: RdfFormat, bytes: &[u8], stop: bool) -> (usize, Option<String>) {
    let mut quads = 0;
    let mut first = None;
    for quad in RdfParser::from_format(format).for_slice(bytes) {
        match quad {
            Ok(_) => quads += 1,
            Err(e) => {
                first.get_or_insert_with(|| e.to_string().replace('\n', " "));
                if stop {
                    break;
                }
            }
        }
    }
    (quads, first)
}

/// One probe: the scan's verdict and the parser's, as one line.
fn run(probe: &str) -> String {
    let parts: Vec<&str> = probe.split(':').collect();
    let scan = |r: ikigai_core::Result<()>| if r.is_ok() { "admitted" } else { "refused" };
    match parts.as_slice() {
        // case 1: `nt|nq`, `same|next`, `stop|resume`, levels
        [fmt @ ("nt" | "nq"), line, mode, levels] => {
            let text = hidden(fmt, line, levels.parse().unwrap());
            let format = if *fmt == "nt" {
                RdfFormat::NTriples
            } else {
                RdfFormat::NQuads
            };
            let verdict = scan(check_turtle_nesting(&text, "content"));
            let (quads, first) = parse(format, text.as_bytes(), *mode == "stop");
            format!("scan {verdict}; parser {quads} quads, first error {first:?}")
        }
        // case 2: `rdfxml`, encoding, levels
        ["rdfxml", encoding, levels] => {
            let bytes = rdfxml(levels.parse().unwrap(), encoding);
            let verdict = scan(check_rdfxml_nesting(&bytes, "content"));
            let (quads, first) = parse(RdfFormat::RdfXml, &bytes, true);
            format!("scan {verdict}; parser {quads} quads, first error {first:?}")
        }
        _ => panic!("no probe named {probe}"),
    }
}

/// The child's half: inert unless a parent named a probe. Runs it on a 2 MiB thread, prints the
/// outcome, and exits before the harness can.
#[test]
fn probe_child() {
    let Ok(probe) = std::env::var(PROBE) else {
        return;
    };
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || run(&probe))
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: run one probe in a child and return what it said, or `Err` naming how it
/// died.
fn try_probe(probe: &str) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, probe)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "`{probe}` did not survive a 2 MiB thread: {} — {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("`{probe}` reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

fn probe(probe: &str) -> String {
    try_probe(probe).unwrap_or_else(|died| panic!("{died}"))
}

// ------------------------------------------------------------------ case 1

/// The claim's premise holds (the Turtle reading admits the document) and is harmless: the
/// N-Triples parser errors at the `'`, before the term, and a door stops there.
#[test]
fn a_term_hidden_by_a_quote_is_admitted_and_the_parser_stops_at_the_quote() {
    for fmt in ["nt", "nq"] {
        for line in ["same", "next"] {
            let outcome = probe(&format!("{fmt}:{line}:stop:{LETHAL}"));
            assert!(
                outcome.starts_with("scan admitted; parser 0 quads, first error Some("),
                "{fmt} {line}: {outcome}"
            );
        }
    }
}

/// The parser RESUMES after an error at the next line, so the hidden statement is built by a
/// consumer that reads on: shallow here, so it is seen rather than fatal.
#[test]
fn the_ntriples_parser_resumes_at_the_next_line_after_an_error() {
    for fmt in ["nt", "nq"] {
        let outcome = probe(&format!("{fmt}:next:resume:3"));
        assert!(
            outcome.starts_with("scan admitted; parser 1 quads, first error Some("),
            "{fmt}: {outcome}"
        );
        // On the same line, the recovery skips the rest of the line, term and all.
        let outcome = probe(&format!("{fmt}:same:resume:3"));
        assert!(
            outcome.starts_with("scan admitted; parser 0 quads, first error Some("),
            "{fmt}: {outcome}"
        );
    }
}

/// The control: the same document, read on past the error, aborts. So the scan's reading of
/// N-Triples is safe only for a consumer that stops at the first error, which is what
/// `check_turtle_nesting` asks of its callers.
#[test]
fn reading_on_past_the_error_builds_the_hidden_term_and_aborts() {
    let died = try_probe(&format!("nt:next:resume:{LETHAL}"));
    assert!(died.is_err(), "expected an abort, got {died:?}");
}

// ------------------------------------------------------------------ case 2

/// oxrdfxml fails at the first element of a UTF-16 document, so its nesting is never built,
/// whatever the scan made of it. (The scan reads UTF-16 as UTF-8 bytes: it admits the claim's
/// `<r/>`, and here it refuses, because the NUL bytes inside tags make attributes it cannot
/// parse, and it counts those.) The control: the same document in UTF-8, parsed unscanned,
/// aborts, and the scan refuses it one level past the bound.
#[test]
fn utf16_rdfxml_reaches_the_parser_as_an_error_at_the_first_element() {
    for encoding in ["utf16le", "utf16le-bom", "utf16be-bom"] {
        let outcome = probe(&format!("rdfxml:{encoding}:{LETHAL_RDFXML}"));
        assert!(
            outcome.contains("; parser 0 quads, first error Some("),
            "{encoding}: {outcome}"
        );
    }
    let died = try_probe(&format!("rdfxml:utf8:{LETHAL_RDFXML}"));
    assert!(died.is_err(), "expected an abort, got {died:?}");
    assert!(check_rdfxml_nesting(&rdfxml(65, "utf8"), "content").is_err());
}

/// The claim's document: `<r/>` in UTF-16LE without a byte order mark is admitted by the scan
/// and is a parse error, never RDF.
#[test]
fn the_claims_utf16_document_is_admitted_and_fails_to_parse() {
    let bytes: Vec<u8> = "<r/>".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert!(check_rdfxml_nesting(&bytes, "content").is_ok());
    let (quads, first) = parse(RdfFormat::RdfXml, &bytes, true);
    assert!(quads == 0 && first.is_some(), "{quads} {first:?}");
}

/// The documents are what they say: shallow, each parses in UTF-8 and through the N-Triples
/// parser once the stray quote is gone.
#[test]
fn the_documents_parse_when_shallow_and_well_formed() {
    let (quads, first) = parse(RdfFormat::RdfXml, &rdfxml(3, "utf8"), true);
    assert!(first.is_none() && quads > 0, "{quads} {first:?}");
    for (fmt, format) in [("nt", RdfFormat::NTriples), ("nq", RdfFormat::NQuads)] {
        let text = hidden(fmt, "next", 3)
            .replace("'x", "\"x\"")
            .replace(" # '", "");
        let (quads, first) = parse(format, text.as_bytes(), true);
        assert_eq!((quads, first), (2, None), "{fmt}: {text}");
    }
}
