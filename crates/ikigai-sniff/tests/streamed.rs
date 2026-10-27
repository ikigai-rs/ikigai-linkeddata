//! `urn:transrept:auto` over a streamed `content` (ledger #2278, part 3): it sniffs a prefix
//! and hands the body to the kernel's plan executor, so an incremental step reads the stream
//! as it comes instead of a copy of it held whole.
//!
//! The producer here counts the chunks polled from it, which is what makes "sniffs a
//! prefix" and "never held whole" visible rather than assumed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use futures::StreamExt;
use ikigai_core::{
    ArgRef, ArgSpec, Body, Bytes, Capability, Description, Endpoint, Error, Exact, FailureClass,
    FnEndpoint, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_sniff::SNIFF_PREFIX;

/// One chunk of the streamed input: a Turtle triple padded with a comment to 1 KiB.
const CHUNK: usize = 1024;
/// 1 MiB of input, sixteen times the byte budget the streaming tests run under.
const CHUNKS: usize = 1024;
/// The run's byte budget in the streaming tests.
const BUDGET: u64 = 64 * 1024;

fn turtle_chunk(n: usize) -> Bytes {
    let mut line = format!("<urn:demo:s{n}> <urn:demo:p> <urn:demo:o> . #").into_bytes();
    line.resize(CHUNK - 1, b'x');
    line.push(b'\n');
    line.into()
}

/// `urn:big`: `CHUNKS` chunks of `first` then Turtle, as a one-shot body, counting every
/// chunk polled from it.
struct Producer {
    polled: Arc<AtomicUsize>,
    first: Bytes,
}

#[async_trait]
impl Endpoint for Producer {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let body = self.invoke_body(inv).await?;
        let budget = inv.memory_budget().expect("the kernel mints one");
        body.collect_within(budget).await
    }

    async fn invoke_body(&self, _inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let (polled, first) = (self.polled.clone(), self.first.clone());
        let chunks = futures::stream::iter(0..CHUNKS).map(move |n| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok(if n == 0 {
                first.clone()
            } else {
                turtle_chunk(n)
            })
        });
        Ok(Representation::from_body(
            ReprType::new("application/octet-stream"),
            Body::once(chunks),
        ))
    }
}

/// An incremental turtle → text/plain transreptor that streams its input back, upper-cased,
/// chunk for chunk; it records whether its `content` came as a stream (by content id).
struct Shout {
    streamed: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for Shout {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let body = self.invoke_body(inv).await?;
        let budget = inv.memory_budget().expect("the kernel mints one");
        body.collect_within(budget).await
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        if matches!(inv.request.args.get("content"), Some(ArgRef::Content(_))) {
            self.streamed.fetch_add(1, Ordering::SeqCst);
        }
        let input = inv.content_body("content").await?.into_stream();
        let upper = input.map(|chunk| chunk.map(|c| Bytes::from(c.to_ascii_uppercase())));
        Ok(Representation::from_body(
            ReprType::new("text/plain"),
            Body::once(upper),
        ))
    }

    fn describe(&self) -> Description {
        Description::new("shout")
            .verb(Verb::Source)
            .input(ArgSpec::new("content"))
            .input(ArgSpec::new("as"))
            .transreptor(["text/turtle"], ["text/plain"])
            .incremental()
    }
}

/// A turtle → text/html transreptor that is NOT incremental: a plan collects for it.
fn whole_turtle_to_html() -> FnEndpoint {
    FnEndpoint::new("ttl2html", |inv: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/html"),
            inv.inline_arg("content")?.len().to_string().into_bytes(),
        ))
    })
    .with_description(
        Description::new("ttl2html")
            .verb(Verb::Source)
            .input(ArgSpec::new("content"))
            .input(ArgSpec::new("as"))
            .transreptor(["text/turtle"], ["text/html"]),
    )
}

struct Fixture {
    kernel: Kernel,
    polled: Arc<AtomicUsize>,
    streamed: Arc<AtomicUsize>,
}

fn fixture(first: Bytes, budget: u64) -> Fixture {
    let (polled, streamed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let space = ikigai_sniff::space()
        .bind(
            Exact::new("urn:big"),
            Producer {
                polled: polled.clone(),
                first,
            },
        )
        .bind(
            Exact::new("urn:demo:shout"),
            Shout {
                streamed: streamed.clone(),
            },
        )
        .bind(Exact::new("urn:demo:ttl2html"), whole_turtle_to_html());
    Fixture {
        kernel: Kernel::new(Arc::new(space)).with_stream_memory(budget, budget),
        polled,
        streamed,
    }
}

fn auto(content: ArgRef, as_type: &str) -> Request {
    Request::new(Verb::Source, Iri::parse("urn:transrept:auto").unwrap())
        .with_arg("content", content)
        .with_arg("as", ArgRef::Inline(as_type.as_bytes().to_vec()))
}

fn big() -> ArgRef {
    ArgRef::Reference(Iri::parse("urn:big").unwrap())
}

/// The chunks a prefix sniff may read: enough for `SNIFF_PREFIX + 1` bytes.
const PREFIX_CHUNKS: usize = SNIFF_PREFIX / CHUNK + 1;

#[test]
fn a_streamed_content_reaches_an_incremental_step_unread_and_never_held_whole() {
    let f = fixture(turtle_chunk(0), BUDGET);
    let answer = block_on(
        f.kernel
            .issue_body(auto(big(), "text/plain"), &Capability::root()),
    )
    .expect("a 1 MiB stream under a 64 KiB budget, never collected");
    assert_eq!(answer.repr_type.media_type, "text/plain");
    assert_eq!(
        f.streamed.load(Ordering::SeqCst),
        1,
        "the step got the stream"
    );
    // The answer arrives having read only the sniffed prefix of the input.
    let polled = f.polled.load(Ordering::SeqCst);
    assert!(
        polled <= PREFIX_CHUNKS,
        "{polled} chunks read before the answer"
    );

    // Read it chunk by chunk: every byte comes through, sixteen times the run's budget.
    let mut stream = answer.bytes.into_stream();
    let (mut total, mut first) = (0, None);
    while let Some(chunk) = block_on(stream.next()) {
        let chunk = chunk.unwrap();
        first.get_or_insert_with(|| chunk.clone());
        total += chunk.len();
    }
    assert_eq!(total, CHUNK * CHUNKS);
    assert!(total as u64 > 16 * BUDGET - 1);
    assert!(first.unwrap().starts_with(b"<URN:DEMO:S0>"));
    assert_eq!(f.polled.load(Ordering::SeqCst), CHUNKS);
}

#[test]
fn a_blob_nothing_converts_is_refused_after_the_prefix_alone() {
    let png = Bytes::from_static(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    let f = fixture(png, BUDGET);
    let err = block_on(
        f.kernel
            .issue_body(auto(big(), "text/turtle"), &Capability::root()),
    )
    .map(|_| ())
    .expect_err("no transreptor from image/png");
    assert!(format!("{err}").contains("image/png"), "{err}");
    let polled = f.polled.load(Ordering::SeqCst);
    assert!(
        polled <= PREFIX_CHUNKS,
        "stopped after the prefix: {polled} of {CHUNKS} chunks read"
    );
}

#[test]
fn past_the_byte_budget_the_answer_is_over_budget() {
    // A step that needs its input whole: collected under the budget, refused past it.
    let f = fixture(turtle_chunk(0), BUDGET);
    let err = block_on(
        f.kernel
            .issue_body(auto(big(), "text/html"), &Capability::root()),
    )
    .map(|_| ())
    .expect_err("1 MiB collected under 64 KiB");
    assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
    assert_eq!(err.class(), FailureClass::OverBudget);

    // The bytes path collects the whole answer under the same budget: refused, never a
    // shorter answer.
    let f = fixture(turtle_chunk(0), BUDGET);
    let err = block_on(
        f.kernel
            .issue(auto(big(), "text/plain"), &Capability::root()),
    )
    .expect_err("a 1 MiB answer under 64 KiB");
    assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
}

#[test]
fn the_body_answer_collected_equals_the_bytes_answer() {
    let ttl = b"@prefix ex: <http://e/> . ex:a ex:b ex:c .\n".to_vec();
    for as_type in ["text/plain", "text/html", "text/turtle"] {
        let f = fixture(turtle_chunk(0), BUDGET);
        let request = || auto(ArgRef::Inline(ttl.clone()), as_type);
        let bytes = block_on(f.kernel.issue(request(), &Capability::root())).unwrap();
        let body = block_on(f.kernel.issue_body(request(), &Capability::root())).unwrap();
        let body = block_on(body.collect(1 << 20)).unwrap();
        assert_eq!(body, bytes, "as={as_type}");
    }
    // And over the stream, under a budget that holds it.
    let f = fixture(turtle_chunk(0), 4 << 20);
    let bytes = block_on(
        f.kernel
            .issue(auto(big(), "text/html"), &Capability::root()),
    )
    .unwrap();
    let body = block_on(
        f.kernel
            .issue_body(auto(big(), "text/html"), &Capability::root()),
    )
    .unwrap();
    assert_eq!(block_on(body.collect(4 << 20)).unwrap(), bytes);
    assert_eq!(bytes.bytes, (CHUNK * CHUNKS).to_string().into_bytes());
}

#[test]
fn a_stream_that_fails_after_the_prefix_ends_with_its_error() {
    // A producer whose stream fails part-way: the answer ends with the error, never cleanly.
    struct Failing;
    #[async_trait]
    impl Endpoint for Failing {
        async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
            unreachable!("read as a body")
        }
        async fn invoke_body(&self, _inv: &Invocation<'_>) -> Result<Representation<Body>> {
            let chunks = (0..CHUNKS).map(|n| match n {
                n if n < CHUNKS / 2 => Ok(turtle_chunk(n)),
                _ => Err(Error::Endpoint("the source went away".into())),
            });
            Ok(Representation::from_body(
                ReprType::new("application/octet-stream"),
                Body::once(futures::stream::iter(chunks)),
            ))
        }
    }
    let f = fixture(turtle_chunk(0), BUDGET);
    let space = ikigai_sniff::space()
        .bind(Exact::new("urn:failing"), Failing)
        .bind(
            Exact::new("urn:demo:shout"),
            Shout {
                streamed: f.streamed.clone(),
            },
        );
    let kernel = Kernel::new(Arc::new(space));
    let request = auto(
        ArgRef::Reference(Iri::parse("urn:failing").unwrap()),
        "text/plain",
    );
    let answer = block_on(kernel.issue_body(request, &Capability::root())).unwrap();
    let items: Vec<_> = block_on(answer.bytes.into_stream().collect());
    assert!(
        items.last().unwrap().is_err(),
        "the stream ends with its error"
    );
}
