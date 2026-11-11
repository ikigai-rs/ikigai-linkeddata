#![cfg(feature = "transrept")]

//! `urn:rdf:transrept`'s body path (ledger #2278, part 2): Turtle (and N-Triples, which it
//! reads as Turtle) written as N-Triples or N-Quads is parsed as it arrives, with the nesting
//! bound counted on each chunk BEFORE the parser sees it.
//!
//! The producer counts the chunks polled from it, so "as it arrives", "never held whole" and
//! "refused where the nesting is, not after the parse" are visible rather than assumed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use futures::StreamExt;
use ikigai_core::{
    ArgRef, Body, Bytes, Capability, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result, Space, Verb,
};

/// `urn:doc`: the chunks it was given, as a one-shot body, counting every chunk polled.
struct Producer {
    chunks: Arc<Mutex<Vec<Bytes>>>,
    polled: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for Producer {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let body = self.invoke_body(inv).await?;
        body.collect_within(inv.memory_budget().expect("minted"))
            .await
    }

    async fn invoke_body(&self, _inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let chunks = self.chunks.lock().unwrap().clone();
        let polled = self.polled.clone();
        let stream = futures::stream::iter(chunks).map(move |chunk| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok(chunk)
        });
        Ok(Representation::from_body(
            ReprType::new("application/octet-stream"),
            Body::once(stream),
        ))
    }
}

struct Fixture {
    kernel: Kernel,
    polled: Arc<AtomicUsize>,
}

fn fixture(chunks: Vec<Bytes>, budget: u64) -> Fixture {
    let polled = Arc::new(AtomicUsize::new(0));
    let producer = Producer {
        chunks: Arc::new(Mutex::new(chunks)),
        polled: polled.clone(),
    };
    Fixture {
        kernel: kernel_over(producer, budget),
        polled,
    }
}

/// The rdf space, sniff's space (`urn:transrept:auto`) and `urn:doc`, under a byte budget.
fn kernel_over(producer: Producer, budget: u64) -> Kernel {
    let doc = EndpointSpace::new().bind(Exact::new("urn:doc"), producer);
    let space = Fallback::new(vec![
        Arc::new(ikigai_rdf::space()) as Arc<dyn Space>,
        Arc::new(ikigai_sniff::space()),
        Arc::new(doc),
    ]);
    Kernel::new(Arc::new(space)).with_stream_memory(budget, budget)
}

fn split(text: &[u8], size: usize) -> Vec<Bytes> {
    text.chunks(size).map(Bytes::copy_from_slice).collect()
}

fn transrept(content: ArgRef, as_type: &str) -> Request {
    Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
        .with_arg("content", content)
        .with_arg("as", ArgRef::Inline(as_type.as_bytes().to_vec()))
}

fn doc() -> ArgRef {
    ArgRef::Reference(Iri::parse("urn:doc").unwrap())
}

const TARGETS: [&str; 3] = [
    "application/n-triples",
    "application/n-quads",
    "nt", // an alias: both paths answer the canonical application/n-triples media type
];

const DOCS: [&str; 5] = [
    "@prefix ex: <http://example.org/> .\n\
     ex:a ex:p \"a literal with \\\" a quote and # no comment\" ;\n\
     \x20   ex:q [ ex:r ex:s ; ex:t ( 1 2 3 ) ] .\n\
     # a comment with << inside\n\
     ex:b ex:p \"\"\"long\nstring \"\" with quotes\"\"\"@en , _:x .\n\
     _:x ex:p <http://example.org/iri#with<<> .\n",
    "<urn:demo:a> <urn:demo:p> <urn:demo:o> .\n<urn:demo:a> <urn:demo:p> \"x\"^^<urn:demo:t> .\n",
    "   \n\n  <urn:demo:a> <urn:demo:p> [ <urn:demo:q> [] ] .\n",
    "@prefix ex: <http://example.org/> . ex:a ex:p ex:o . ex:a ex:p \"unterminated",
    "",
];

#[test]
fn the_body_answer_collected_equals_the_bytes_answer_for_every_chunking() {
    for doc_text in DOCS {
        for as_type in TARGETS {
            let inline = || transrept(ArgRef::Inline(doc_text.as_bytes().to_vec()), as_type);
            let f = fixture(vec![], 1 << 20);
            let bytes = block_on(f.kernel.issue(inline(), &Capability::root()));
            for size in [1, 2, 3, 7, 64, 4096] {
                let f = fixture(split(doc_text.as_bytes(), size), 1 << 20);
                let body = block_on(
                    f.kernel
                        .issue_body(transrept(doc(), as_type), &Capability::root()),
                )
                .and_then(|answer| {
                    let repr_type = answer.repr_type.clone();
                    block_on(answer.bytes.collect(1 << 20))
                        .map(|bytes| Representation::new(repr_type, bytes.to_vec()))
                });
                match (&bytes, &body) {
                    (Ok(bytes), Ok(body)) => {
                        // The bytes and their type; not the expiry, since a one-shot
                        // answer is never stored.
                        assert_eq!(
                            String::from_utf8_lossy(&body.bytes),
                            String::from_utf8_lossy(&bytes.bytes),
                            "{as_type}, chunks of {size}: {doc_text:?}"
                        );
                        assert_eq!(body.repr_type, bytes.repr_type);
                    }
                    (Err(bytes), Err(body)) => assert_eq!(
                        format!("{body}"),
                        format!("{bytes}"),
                        "{as_type}, chunks of {size}: {doc_text:?}"
                    ),
                    _ => panic!("{as_type}, chunks of {size}: {bytes:?} vs {body:?}"),
                }
            }
        }
    }
}

/// 4 MiB of N-Triples in 4 KiB chunks, under a 64 KiB byte budget.
fn large() -> Vec<Bytes> {
    (0..1024)
        .map(|n| {
            let mut chunk = String::new();
            for m in 0.. {
                let line = format!("<urn:demo:s{n}-{m}> <urn:demo:p> \"{m:0>40}\" .\n");
                if chunk.len() + line.len() > 4096 {
                    break;
                }
                chunk.push_str(&line);
            }
            Bytes::from(format!("{chunk:<4095}\n"))
        })
        .collect()
}

#[test]
fn a_large_input_is_parsed_as_it_arrives_and_never_held_whole() {
    let input = large();
    let size: usize = input.iter().map(Bytes::len).sum();
    let f = fixture(input, 64 * 1024);
    let answer = block_on(f.kernel.issue_body(
        transrept(doc(), "application/n-triples"),
        &Capability::root(),
    ))
    .expect("4 MiB under a 64 KiB budget: never collected");
    assert!(answer.bytes.is_one_shot());
    let before = f.polled.load(Ordering::SeqCst);
    assert!(before <= 1, "{before} chunks read before the answer");

    let mut stream = answer.bytes.into_stream();
    let (mut read, mut lines, mut most_polled_ahead) = (0, 0, 0);
    while let Some(chunk) = block_on(stream.next()) {
        let chunk = chunk.unwrap();
        read += 1;
        lines += chunk.iter().filter(|&&b| b == b'\n').count();
        // Each chunk out is answered from the chunks read so far: the input is never read
        // ahead of the output.
        most_polled_ahead = most_polled_ahead.max(f.polled.load(Ordering::SeqCst) - read);
    }
    assert!(size > 64 * 64 * 1024 - 1, "{size}");
    assert_eq!(f.polled.load(Ordering::SeqCst), 1024);
    assert!(most_polled_ahead <= 1, "{most_polled_ahead}");
    assert!(lines > 1024 * 40, "{lines} lines");
}

/// `<<( ` nested past the bound, inside an otherwise valid document.
fn too_deep() -> Bytes {
    let depth = ikigai_rdf::MAX_TURTLE_NESTING + 1;
    let mut text = String::from("<urn:demo:a> <urn:demo:p> ");
    text.push_str(&"<<( <urn:demo:a> <urn:demo:p> ".repeat(depth));
    text.push_str("<urn:demo:o>");
    text.push_str(&" )>>".repeat(depth));
    text.push_str(" .\n");
    Bytes::from(text)
}

#[test]
fn nesting_past_the_bound_is_refused_where_it_arrives_never_after_the_parse() {
    let mut input = large();
    input.insert(10, too_deep());
    let f = fixture(input, 64 * 1024);
    let answer = block_on(f.kernel.issue_body(
        transrept(doc(), "application/n-triples"),
        &Capability::root(),
    ))
    .expect("the answer starts streaming");
    let items: Vec<_> = block_on(answer.bytes.into_stream().collect());
    let (last, before) = items.split_last().unwrap();
    assert!(before.iter().all(Result::is_ok), "the lines before it");
    match last {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "content");
            assert!(detail.contains("nest"), "{detail}");
        }
        other => panic!("expected the depth refusal, got {other:?}"),
    }
    // Refused at the chunk that nests too deep: nothing after it was read, let alone parsed.
    assert_eq!(f.polled.load(Ordering::SeqCst), 11);

    // The bytes path refuses the same document the same way.
    let whole: Vec<u8> = large()[..10]
        .iter()
        .chain([&too_deep()])
        .flat_map(|c| c.to_vec())
        .collect();
    let f = fixture(vec![], 64 << 20);
    let err = block_on(f.kernel.issue(
        transrept(ArgRef::Inline(whole), "application/n-triples"),
        &Capability::root(),
    ))
    .expect_err("too deep");
    assert_eq!(format!("{err}"), format!("{}", last.as_ref().unwrap_err()));
}

#[test]
fn past_the_byte_budget_the_answer_is_over_budget() {
    // A target that needs the document whole: collected under the budget, refused past it.
    let f = fixture(large(), 64 * 1024);
    let err = block_on(
        f.kernel
            .issue_body(transrept(doc(), "text/turtle"), &Capability::root()),
    )
    .map(|_| ())
    .expect_err("4 MiB collected under 64 KiB");
    assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
    // The bytes API collects the streamed answer under the same budget: refused too, never
    // a shorter answer.
    let f = fixture(large(), 64 * 1024);
    let err = block_on(f.kernel.issue(
        transrept(doc(), "application/n-triples"),
        &Capability::root(),
    ))
    .expect_err("a 4 MiB answer under 64 KiB");
    assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
}

#[test]
fn a_plan_hands_the_transreptor_the_stream_uncollected() {
    // `urn:transrept:auto` sniffs the stream's prefix and plans `urn:rdf:transrept`, which
    // declares itself incremental: the plan passes the body through by content id.
    let polled = Arc::new(AtomicUsize::new(0));
    let producer = Producer {
        chunks: Arc::new(Mutex::new(large())),
        polled: polled.clone(),
    };
    let kernel = kernel_over(producer, 64 * 1024);
    let request = Request::new(Verb::Source, Iri::parse("urn:transrept:auto").unwrap())
        .with_arg("content", doc())
        .with_arg("as", ArgRef::Inline(b"application/n-triples".to_vec()));
    let answer = block_on(kernel.issue_body(request, &Capability::root()))
        .expect("4 MiB through a plan under 64 KiB");
    assert_eq!(answer.repr_type.media_type, "application/n-triples");
    let before = polled.load(Ordering::SeqCst);
    assert!(
        before <= 17,
        "only the sniffed prefix before the answer: {before}"
    );
    let lines: usize = block_on(answer.bytes.into_stream().collect::<Vec<_>>())
        .into_iter()
        .map(|chunk| chunk.unwrap().iter().filter(|&&b| b == b'\n').count())
        .sum();
    assert!(lines > 1024 * 40, "{lines}");
}

#[test]
fn dataset_syntax_survives_every_chunk_boundary_and_late_disambiguation() {
    let common = "<urn:test:s> <urn:test:p> _:a .\n".repeat(100);
    let docs = [
        format!("{common}<urn:test:s> <urn:test:p> _:a <urn:test:g> .\n_:a <urn:test:p> <urn:test:o> _:a ."),
        format!("{common}<urn:test:g> {{ _:a <urn:test:p> <urn:test:o> . }}"),
        "{ <urn:test:s> <urn:test:p> <urn:test:o> . }".to_string(),
        "{} <urn:test:g> { <urn:test:s> <urn:test:p> <urn:test:o> . }".to_string(),
        "[] <urn:test:p> <urn:test:o> .".to_string(),
        "[ a <urn:test:Class> ] <urn:test:p> <urn:test:o> .".to_string(),
        r"<http://example.org/\u00E9> <urn:test:p> <urn:test:o> .".to_string(),
    ];
    for text in docs {
        let f = fixture(vec![], 1 << 20);
        let inline = transrept(ArgRef::Inline(text.as_bytes().to_vec()), "nq");
        let expected = block_on(f.kernel.issue(inline, &Capability::root())).unwrap();
        assert_eq!(expected.repr_type.media_type, "application/n-quads");
        for size in [1, 2, 3, 7, 64, 4096] {
            let f = fixture(split(text.as_bytes(), size), 1 << 20);
            let result = block_on(
                f.kernel
                    .issue_body(transrept(doc(), "nq"), &Capability::root()),
            )
            .unwrap();
            assert_eq!(result.repr_type, expected.repr_type);
            let actual = block_on(result.bytes.collect(1 << 20)).unwrap();
            assert_eq!(actual.as_ref(), expected.bytes, "chunks of {size}: {text}");
        }
    }
}

#[test]
fn an_iri_longer_than_the_sniff_hold_uses_the_buffered_fallback() {
    let text = format!(
        "<http://example.org/{}> <urn:test:p> <urn:test:o> .",
        "a".repeat(80_000)
    );
    let f = fixture(split(text.as_bytes(), 4096), 1 << 20);
    let result = block_on(
        f.kernel
            .issue_body(transrept(doc(), "nq"), &Capability::root()),
    )
    .unwrap();
    let actual = block_on(result.bytes.collect(1 << 20)).unwrap();
    assert_eq!(actual.as_ref(), format!("{text}\n").as_bytes());
}

#[test]
fn streamed_src_resolves_relative_iris_against_retrieval_and_document_bases() {
    for (text, expected) in [
        ("<s> <http://example.org/p> <o> .", "<https://example.org/docs/s> <http://example.org/p> <https://example.org/docs/o> .\n"),
        ("@base <https://example.org/override/> . <s> <http://example.org/p> <o> .", "<https://example.org/override/s> <http://example.org/p> <https://example.org/override/o> .\n"),
    ] {
        // A synthetic HTTP source with a declared Turtle type and a one-shot body.
        struct TurtleSource(Producer);
        #[async_trait]
        impl Endpoint for TurtleSource {
            async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
                self.invoke_body(inv).await?.collect_within(inv.memory_budget().unwrap()).await
            }
            async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
                let mut result = self.0.invoke_body(inv).await?;
                result.repr_type = ReprType::new("text/turtle");
                Ok(result)
            }
        }
        let source = TurtleSource(Producer {
            chunks: Arc::new(Mutex::new(split(text.as_bytes(), 1))),
            polled: Arc::new(AtomicUsize::new(0)),
        });
        let space = Fallback::new(vec![
            Arc::new(ikigai_rdf::space()) as Arc<dyn Space>,
            Arc::new(EndpointSpace::new().bind(Exact::new("urn:http:get"), source)),
        ]);
        let kernel = Kernel::new(Arc::new(space));
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg("src", ArgRef::Inline(b"https://example.org/docs/source".to_vec()))
            .with_arg("as", ArgRef::Inline(b"nq".to_vec()));
        let result = block_on(kernel.issue_body(request, &Capability::root())).unwrap();
        let actual = block_on(result.bytes.collect(1 << 20)).unwrap();
        assert_eq!(actual.as_ref(), expected.as_bytes());
    }
}
