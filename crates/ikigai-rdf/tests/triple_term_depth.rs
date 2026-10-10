//! Caller RDF that nests RDF 1.2 triple terms deeply is refused before it is parsed, at every
//! door that parses it (ledger #992, the linkeddata half; ikigai-shacl's PR 17 was the first).
//!
//! The claim: oxrdf clones a nested triple term recursively, inside oxttl's Turtle parser, so
//! about 3000 levels of `<<( … )>>` (some 90 KB) overflow the stack of whatever thread parses
//! them, and a stack overflow aborts the WHOLE process, on any thread. The doors here are
//! `urn:rdf:union` and `urn:rdf:diff` (`content`, and `with=` both inline and resolved through
//! the kernel), `urn:rdf:transrept` (`content`, every face), and ikigai-sniff's
//! `urn:transrept:auto`, which routes Turtle to `urn:rdf:transrept`.
//!
//! What held, measured against the code before the fix (a debug build, 3000 levels): Turtle,
//! N-Triples, N-Quads and TriG text aborted the child at all ten doors below, and RDF/XML's
//! nested `rdf:parseType="Triple"` at the four that read RDF/XML. Reified triples (`<< … >>`)
//! did NOT abort: oxttl gives each one a blank-node reifier, so nothing nests in the term. The
//! scan refuses them past the bound anyway, as ikigai-shacl's does, since it counts every `<<`.
//!
//! Every probe runs in a CHILD PROCESS (this test binary re-executed with one probe named in
//! its environment) on a 2 MiB thread, the size of a tokio worker's, so an abort kills the
//! child and reads here as a failure with the signal named.
//!
//! ⚠ **RDF 1.2 is on in these tests, as it is in the CLI.** This crate does not enable
//! `oxrdfio/rdf-12` itself, but any crate that does turns it on for every crate in the same
//! build (feature unification): in ikigai-cli that is rudof, through ikigai-shacl. Without it
//! `<<(` is a syntax error and nothing recurses, so the dev-dependency on `oxrdfio` with
//! `rdf-12` (see Cargo.toml) is what makes these tests test the build that ships.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Verb,
};
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_RDF_TRIPLE_TERM_PROBE";
/// The bound, restated so these tests read the same against the code that predates it.
const BOUND: usize = 64;

/// The innermost triple, `levels` triple terms deep.
fn nested(levels: usize) -> String {
    format!(
        "{}<urn:ex:o>{}",
        "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " )>>".repeat(levels)
    )
}

/// The same depth as reified triples, `<< … >>`, which carry a triple term inside.
fn reified(levels: usize) -> String {
    format!(
        "{}<urn:ex:o>{}",
        "<< <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " >>".repeat(levels)
    )
}

/// A document in `syntax` whose one statement's object nests `levels` deep.
fn document(syntax: &str, levels: usize) -> String {
    match syntax {
        "turtle" => format!("@prefix ex: <urn:ex:> .\nex:s ex:p {} .\n", nested(levels)),
        "reified" => format!("<urn:ex:s> <urn:ex:p> {} .\n", reified(levels)),
        // N-Triples is a subset of Turtle, and these doors read it with the Turtle parser.
        "ntriples" => format!("<urn:ex:s> <urn:ex:p> {} .\n", nested(levels)),
        // An N-Quads line goes to the same parser (sniffed as Turtle), which builds the object
        // before it meets the graph name.
        "nquads" => format!("<urn:ex:s> <urn:ex:p> {} <urn:ex:g> .\n", nested(levels)),
        // TriG's default-graph statements are Turtle.
        "trig" => format!(
            "<urn:ex:s> <urn:ex:p> {} .\n<urn:ex:g> {{ }}\n",
            nested(levels)
        ),
        // `rdf:version="1.2"`: without it oxrdfxml drops a `parseType="Triple"` silently.
        "rdfxml" | "rdfxml-annotated" => {
            // `rdf:annotation` on the OUTERMOST element makes the PARSER itself copy the whole
            // nested term (it reifies it), so the recursion starts inside oxrdfxml rather than
            // after it. (On an inner one the reifying triple would land inside the enclosing
            // `parseType="Triple"`, which may hold only one, and the parser refuses that.)
            let annotation = if syntax == "rdfxml" {
                ""
            } else {
                " rdf:annotation=\"urn:ex:a\""
            };
            let element = |extra: &str| {
                format!(
                    "<ex:p rdf:parseType=\"Triple\"{extra}><rdf:Description rdf:about=\"urn:s\">"
                )
            };
            let open = if levels == 0 {
                String::new()
            } else {
                element(annotation) + &element("").repeat(levels - 1)
            };
            let close = "</rdf:Description></ex:p>".repeat(levels);
            format!(
                "<?xml version=\"1.0\"?>\n<rdf:RDF \
                 xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
                 xmlns:ex=\"urn:ex:\" rdf:version=\"1.2\"><rdf:Description rdf:about=\"urn:s\">{open}\
                 <ex:p rdf:resource=\"urn:o\"/>{close}</rdf:Description></rdf:RDF>\n"
            )
        }
        // Not a triple term (oxjsonld has none): node objects nested `levels` deep, `levels + 1`
        // JSON levels. See the JSON-LD tests at the end (ledger #1038).
        "jsonld" => format!(
            "{}{{\"@id\":\"urn:ex:o\"}}{}",
            "{\"@id\":\"urn:ex:s\",\"urn:ex:p\":".repeat(levels),
            "}".repeat(levels)
        ),
        other => panic!("no syntax named {other}"),
    }
}

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
            .with_arg("with", inline("urn:test:deep")),
        "transrept" => req("urn:rdf:transrept").with_arg("content", inline(&text)),
        "transrept-nt" => req("urn:rdf:transrept")
            .with_arg("content", inline(&text))
            .with_arg("as", inline("application/n-triples")),
        "transrept-html" => req("urn:rdf:transrept")
            .with_arg("content", inline(&text))
            .with_arg("as", inline("text/html")),
        "auto" => req("urn:transrept:auto")
            .with_arg("content", inline(&text))
            .with_arg("as", inline("application/n-triples")),
        other => panic!("no door named {other}"),
    };
    block_on(kernel.issue(request, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned())
}

/// The argument a door's deep document arrives in, which a refusal must name.
fn carrier(door: &str) -> &'static str {
    if door.contains("-with") {
        "with"
    } else {
        "content"
    }
}

const DOORS: [&str; 10] = [
    "union-content",
    "union-with",
    "union-with-iri",
    "diff-content",
    "diff-with",
    "diff-with-iri",
    "transrept",
    "transrept-nt",
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
            Ok(text) => format!("ok {}", text.chars().take(200).collect::<String>()),
            Err(e) => format!("err {}", e.to_string().replace('\n', " ")),
        })
        .unwrap()
        .join()
        .unwrap();
    println!("\nOUTCOME {}", outcome.replace('\n', " "));
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

fn probe(door: &str, syntax: &str, n: usize) -> String {
    try_probe(door, syntax, n).unwrap_or_else(|died| panic!("{died}"))
}

fn assert_refused(outcome: &str, arg: &str, what: &str) {
    assert!(
        outcome.starts_with(&format!("err invalid argument `{arg}`")) && outcome.contains("64"),
        "{what}: {outcome}"
    );
}

// ------------------------------------------------------------------ the reproduction

#[test]
fn three_thousand_triple_terms_are_refused_at_every_door_and_abort_nothing() {
    let mut died = Vec::new();
    for door in DOORS {
        match try_probe(door, "turtle", 3000) {
            Ok(outcome) => assert_refused(&outcome, carrier(door), door),
            Err(e) => died.push(e),
        }
    }
    assert!(died.is_empty(), "{}", died.join("\n"));
}

#[test]
fn every_turtle_family_syntax_and_reified_triples_are_refused_at_every_door() {
    // N-Triples, N-Quads and TriG all reach the same Turtle parser here (union and diff read
    // only Turtle; transrept sniffs, and a leading IRI or `@prefix` reads as Turtle).
    let mut died = Vec::new();
    for syntax in ["reified", "ntriples", "nquads", "trig"] {
        for door in DOORS {
            match try_probe(door, syntax, 3000) {
                Ok(outcome) => assert_refused(&outcome, carrier(door), &format!("{syntax} {door}")),
                Err(e) => died.push(e),
            }
        }
    }
    assert!(died.is_empty(), "{}", died.join("\n"));
}

#[test]
fn rdfxml_triple_terms_are_refused_by_the_transreptor() {
    // RDF/XML says a triple term with `rdf:parseType="Triple"`, one element pair a level.
    // Only `urn:rdf:transrept` (and so `urn:transrept:auto`) reads RDF/XML.
    for syntax in ["rdfxml", "rdfxml-annotated"] {
        for door in ["transrept", "transrept-nt", "transrept-html", "auto"] {
            assert_refused(
                &probe(door, syntax, 3000),
                "content",
                &format!("{syntax} {door}"),
            );
        }
    }
}

// ------------------------------------------------------------------ at and under the bound

#[test]
fn a_document_at_the_bound_is_answered_and_one_past_it_is_refused() {
    for syntax in ["turtle", "reified", "rdfxml", "rdfxml-annotated"] {
        for door in DOORS {
            if syntax.starts_with("rdfxml") && !door.starts_with("transrept") && door != "auto" {
                continue;
            }
            let ok = issue(door, document(syntax, BOUND));
            assert!(ok.is_ok(), "{syntax} {door} at the bound: {ok:?}");
            let over = issue(door, document(syntax, BOUND + 1))
                .unwrap_err()
                .to_string();
            assert!(
                over.starts_with(&format!("invalid argument `{}`", carrier(door)))
                    && over.contains("deeper than 64"),
                "{syntax} {door} past the bound: {over}"
            );
        }
    }
}

#[test]
fn brackets_in_literals_and_comments_are_not_nesting() {
    let deep = "<<( ".repeat(500);
    // `@prefix` first, so `with=` reads it as a document and not as an IRI to resolve.
    let text = format!(
        "@prefix ex: <urn:ex:> . # {deep}\n\
         ex:s ex:p \"{deep}\", '''{deep}''', \"a\\\"{deep}\", '''a\n{deep}''' .\n"
    );
    for door in DOORS {
        let answer = issue(door, text.clone()).unwrap_or_else(|e| panic!("{door}: {e}"));
        assert!(!answer.is_empty(), "{door}");
    }
}

// ------------------------------------------------------------------ JSON-LD (ledger #1038)

/// A DIFFERENT recursion at the same doors, found by this file's syntax sweep (ledger #992 is
/// triple terms; oxjsonld builds none). oxjsonld's expansion recurses over nested node objects,
/// so `{"@id":…,"urn:ex:p":{"@id":…,"urn:ex:p":{…}}}` aborted the child through
/// `urn:rdf:transrept` and `urn:transrept:auto` at 29 levels (about 1 KB) in a debug build and
/// at 1,021 in a release one (2 MiB thread; `tests/jsonld_depth_measure.rs`). Now refused past
/// `MAX_JSON_NESTING`, counted in JSON levels.
#[test]
fn json_ld_nesting_is_refused_and_aborts_nothing() {
    for door in ["transrept", "transrept-nt", "transrept-html", "auto"] {
        assert_refused(&probe(door, "jsonld", 3000), "content", door);
    }
}

/// At the bound the doors answer, on the 2 MiB thread the probe gives them: a debug build needs
/// ~4.5 MiB for 64 levels, so this passes only because the parse runs on its own sized thread.
/// `document("jsonld", n)` nests `n + 1` JSON levels (the innermost node is one more).
#[test]
fn json_ld_at_the_bound_is_answered_on_a_small_thread_and_one_past_is_refused() {
    for door in ["transrept", "transrept-nt", "transrept-html", "auto"] {
        let ok = probe(door, "jsonld", BOUND - 1);
        assert!(ok.starts_with("ok "), "{door} at the bound: {ok}");
        let over = probe(door, "jsonld", BOUND);
        assert!(
            over.starts_with("err invalid argument `content`")
                && over.contains("deeper than 64 (MAX_JSON_NESTING)"),
            "{door} past the bound: {over}"
        );
    }
}

#[test]
fn brackets_in_json_ld_strings_are_not_nesting() {
    let deep = "[{".repeat(500);
    let text =
        format!("{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":[\"{deep}\",\"a\\\"{deep}\",\"\\\\\"]}}");
    for door in ["transrept", "auto"] {
        let answer = issue(door, text.clone()).unwrap_or_else(|e| panic!("{door}: {e}"));
        assert!(answer.contains("urn:ex:s"), "{door}: {answer}");
    }
}
