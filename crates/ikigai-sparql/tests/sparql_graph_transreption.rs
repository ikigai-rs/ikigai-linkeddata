//! A `graph=` source whose representation names no RDF syntax the loader parses is transrepted
//! to Turtle through the kernel's transreptors before it is loaded (ledger #1442).
//!
//! The claim: `urn:sparql:*` resolved each `graph=` source with a plain `inv.source` and took
//! the bytes it got. A source in an RDF syntax oxigraph reads (Turtle, N-Triples, N-Quads,
//! TriG, RDF/XML, JSON-LD) loads fine that way; anything else was SNIFFED, so an ikigai-log
//! segment (`text/x-ikigai-log`), a gzipped graph, or any module's own format went to the
//! Turtle parser and was refused, even with a transreptor to Turtle bound in the same kernel.
//! A consumer had to bind an explicit compose door per source (the devtools harness coverage
//! tool, ledger #1432, did exactly that per log segment).
//!
//! Measured against the code before the fix (`91cf4f3`): the first test below failed with the
//! Turtle parser's refusal of the source's first line.
//!
//! ⚠ The kernel does NOT transrept a `Source` answer because a request carries `as=`: `as=` is
//! an argument the target endpoint may or may not honor. Transreption is selected by the
//! caller (`Invocation::select_transreptor`) and each step issued through the kernel, which is
//! what the loader now does, and only for a source that needs it.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Error, Exact, Expiry, FnEndpoint, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Verb,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SOURCE: &str = "urn:test:pairs";
const PAIRS: &str = "text/x-test-pairs";
const TRANSREPTOR: &str = "urn:test:pairs-to-turtle";
/// The bound ikigai-rdf's Turtle depth scan holds, restated.
const BOUND: usize = 64;

/// What the source serves, and how the transreptor behaves.
#[derive(Clone, Copy)]
struct Fixture {
    /// The source's media type.
    media: &'static str,
    /// Whether the source's answer is cacheable.
    source_cacheable: bool,
    /// Whether the transreptor's answer is cacheable.
    transreptor_cacheable: bool,
    /// What the transreptor emits: `None` translates the pairs; `Some(text)` emits `text`.
    emits: Option<&'static str>,
    /// Whether the transreptor refuses every input.
    refuses: bool,
}

const PLAIN: Fixture = Fixture {
    media: PAIRS,
    source_cacheable: true,
    transreptor_cacheable: true,
    emits: None,
    refuses: false,
};

/// `subject predicate object` words, one statement a line: a syntax no RDF parser reads.
const BODY: &str = "alice knows bob\nbob knows carol\n";

/// The pairs as Turtle: `alice knows bob` is `<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .`.
fn pairs_to_turtle(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            format!("<urn:ex:{}> <urn:ex:{}> <urn:ex:{}> .\n", w[0], w[1], w[2])
        })
        .collect()
}

/// The per-query space, the source at [`SOURCE`], and a transreptor from `from` to Turtle at
/// [`TRANSREPTOR`] that counts its calls in `calls`.
fn kernel(fx: Fixture, body: String, from: &'static str, calls: Arc<AtomicUsize>) -> Kernel {
    let source = FnEndpoint::new("pairs", move |_: &Invocation<'_>| {
        let rep = Representation::new(ReprType::new(fx.media), body.clone().into_bytes());
        Ok(if fx.source_cacheable {
            rep.cacheable()
        } else {
            rep
        })
    })
    .with_description(Description::new("pairs").verb(Verb::Source));
    let transreptor = FnEndpoint::new("pairsToTurtle", move |inv: &Invocation<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        if fx.refuses {
            return Err(Error::Endpoint("these pairs are malformed".to_string()));
        }
        let content = inv.inline_str("content")?;
        let turtle = match fx.emits {
            Some(text) => text.to_string(),
            None => pairs_to_turtle(content),
        };
        let rep = Representation::new(ReprType::new("text/turtle"), turtle.into_bytes());
        Ok(if fx.transreptor_cacheable {
            rep.cacheable()
        } else {
            rep
        })
    })
    .with_description(
        Description::new("pairsToTurtle")
            .verb(Verb::Source)
            .input(ArgSpec::new("content"))
            .input(ArgSpec::new("as").optional())
            .transreptor([from], ["text/turtle"]),
    );
    Kernel::new(Arc::new(
        ikigai_sparql::space()
            .bind(Exact::new(SOURCE), source)
            .bind(Exact::new(TRANSREPTOR), transreptor),
    ))
}

fn select(query: &str) -> Request {
    Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap())
        .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
        .with_arg("graph", ArgRef::Inline(SOURCE.as_bytes().to_vec()))
        .with_arg("as", ArgRef::Inline(b"text/csv".to_vec()))
}

const WHO_ALICE_KNOWS: &str =
    "SELECT ?o WHERE { GRAPH <urn:test:pairs> { <urn:ex:alice> <urn:ex:knows> ?o } }";

fn run(k: &Kernel, query: &str) -> ikigai_core::Result<Representation> {
    block_on(k.issue(select(query), &Capability::root()))
}

fn text(rep: &Representation) -> String {
    String::from_utf8_lossy(&rep.bytes).into_owned()
}

/// The reproduction: a source in a type no RDF parser reads, with a transreptor to Turtle
/// bound, is queryable, as a named graph named by its IRI.
#[test]
fn a_source_in_a_non_rdf_type_is_transrepted_to_turtle() {
    let calls = Arc::new(AtomicUsize::new(0));
    let k = kernel(PLAIN, BODY.to_string(), PAIRS, Arc::clone(&calls));
    let rep = run(&k, WHO_ALICE_KNOWS).unwrap_or_else(|e| panic!("query failed: {e}"));
    assert!(text(&rep).contains("urn:ex:bob"), "answer: {}", text(&rep));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "transrepted once");
}

/// A source already in a syntax the loader parses pays no transreption, even with a
/// transreptor from its type bound.
#[test]
fn a_source_already_in_rdf_is_not_transrepted() {
    for (media, body) in [
        (
            "application/n-triples",
            "<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .\n",
        ),
        (
            "text/turtle",
            "<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .\n",
        ),
        (
            "application/ld+json",
            r#"{"@id":"urn:ex:alice","urn:ex:knows":{"@id":"urn:ex:bob"}}"#,
        ),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let fx = Fixture {
            media,
            // A transreptor that would break the answer, so running it cannot pass unseen.
            emits: Some("<urn:ex:alice> <urn:ex:knows> <urn:ex:WRONG> .\n"),
            ..PLAIN
        };
        let k = kernel(fx, body.to_string(), media, Arc::clone(&calls));
        let rep = run(&k, WHO_ALICE_KNOWS).unwrap_or_else(|e| panic!("{media}: {e}"));
        assert!(text(&rep).contains("urn:ex:bob"), "{media}: {}", text(&rep));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{media} was transrepted");
    }
}

/// A source whose type says nothing about its syntax (`application/octet-stream`) is still
/// sniffed, as it was: a plan from "any bytes" to Turtle would be a guess, and the sniff is the
/// loader's own guess, which costs no request.
#[test]
fn an_untyped_source_is_sniffed_not_transrepted() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = Fixture {
        media: "application/octet-stream",
        emits: Some("<urn:ex:alice> <urn:ex:knows> <urn:ex:WRONG> .\n"),
        ..PLAIN
    };
    let body = "<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .\n".to_string();
    let k = kernel(fx, body, "application/octet-stream", Arc::clone(&calls));
    let rep = run(&k, WHO_ALICE_KNOWS).unwrap();
    assert!(text(&rep).contains("urn:ex:bob"), "{}", text(&rep));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A source in a type no transreptor converts is sniffed, as it was before: the fix adds a
/// route, it takes none away.
#[test]
fn a_source_with_no_route_to_rdf_is_still_sniffed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = Fixture {
        media: "text/x-no-route",
        ..PLAIN
    };
    let body = "<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .\n".to_string();
    // The transreptor converts PAIRS, not this source's type.
    let k = kernel(fx, body, PAIRS, Arc::clone(&calls));
    let rep = run(&k, WHO_ALICE_KNOWS).unwrap();
    assert!(text(&rep).contains("urn:ex:bob"), "{}", text(&rep));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Cacheable source and cacheable transreptor: the answer stays cacheable, and a second read
/// is served without running the transreptor again.
#[test]
fn a_transrepted_source_keeps_the_answer_cacheable() {
    let calls = Arc::new(AtomicUsize::new(0));
    let k = kernel(PLAIN, BODY.to_string(), PAIRS, Arc::clone(&calls));
    let first = run(&k, WHO_ALICE_KNOWS).unwrap();
    assert_ne!(
        first.expiry,
        Expiry::Always,
        "the answer should be cacheable"
    );
    let second = run(&k, WHO_ALICE_KNOWS).unwrap();
    assert_eq!(text(&first), text(&second));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second read was a cache hit"
    );
}

/// An uncacheable transreptor makes the answer uncacheable, and `urn:kernel:uncached` names it:
/// the least cacheable dependency decides, and the transreption is a dependency.
#[test]
fn an_uncacheable_transreptor_is_named_by_kernel_uncached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = Fixture {
        transreptor_cacheable: false,
        ..PLAIN
    };
    let k = kernel(fx, BODY.to_string(), PAIRS, Arc::clone(&calls));
    let rep = run(&k, WHO_ALICE_KNOWS).unwrap();
    assert_eq!(rep.expiry, Expiry::Always);
    let readout = block_on(k.issue(
        Request::new(Verb::Source, Iri::parse("urn:kernel:uncached").unwrap()),
        &Capability::root(),
    ))
    .unwrap();
    let readout = text(&readout);
    assert!(
        readout.contains("urn:sparql:select") && readout.contains(TRANSREPTOR),
        "urn:kernel:uncached should name the transreptor: {readout}"
    );
}

/// A transreptor's refusal reaches the caller, naming the source it was converting.
#[test]
fn a_refused_transreption_names_the_source() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = Fixture {
        refuses: true,
        ..PLAIN
    };
    let k = kernel(fx, BODY.to_string(), PAIRS, Arc::clone(&calls));
    let err = run(&k, WHO_ALICE_KNOWS).unwrap_err().to_string();
    assert!(
        err.contains(SOURCE) && err.contains("these pairs are malformed"),
        "{err}"
    );
}

/// The transreptor's OUTPUT is scanned for depth like any other source before it is parsed: a
/// transreption is not a way around the bound (ledger #1043).
#[test]
fn a_transrepted_source_is_scanned_for_depth() {
    let levels = BOUND + 1;
    let deep: &'static str = Box::leak(
        format!(
            "<urn:ex:s> <urn:ex:p> {}<urn:ex:o>{} .\n",
            "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
            " )>>".repeat(levels)
        )
        .into_boxed_str(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = Fixture {
        emits: Some(deep),
        ..PLAIN
    };
    let k = kernel(fx, BODY.to_string(), PAIRS, Arc::clone(&calls));
    match run(&k, "SELECT * WHERE { ?s ?p ?o }") {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "graph");
            assert!(detail.contains(SOURCE), "{detail}");
        }
        other => panic!("expected a depth refusal on `graph`, got {other:?}"),
    }
}
