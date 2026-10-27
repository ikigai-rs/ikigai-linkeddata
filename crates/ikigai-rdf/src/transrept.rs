use crate::depth::TurtleNesting;
use crate::{
    canonical_triples, check_json_nesting, check_rdfxml_nesting, check_turtle_nesting, depth,
    BlankNodeLabels,
};

use futures_util::StreamExt;
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Body, ByteStream, Bytes, Description, Endpoint, EndpointSpace,
    Error, Exact, FailureClass, InputGroup, Invocation, Iri, ReprType, Representation, Result,
    Verb,
};
use oxrdf::{Graph, GraphName, Quad, Triple};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use oxttl::turtle::LowLevelTurtleParser;
use oxttl::TurtleParser;

/// The XSD `string` datatype IRI — the `class` of every by-value input here. An RDF
/// document, a graph reference-or-document, a media type and a mode token are all
/// scalars an agent supplies as text; `ArgSpec` has no narrower class for a document.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The XSD `anyURI` datatype IRI — the `class` of `urn:rdf:transrept`'s `src=`, one IRI.
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

/// The space binding `urn:rdf:transrept`. Mount it in any kernel (the CLI's embedded
/// space, the in-browser kernel) to give it RDF content negotiation.
/// Parse Turtle into an [`oxrdf::Graph`] — a triple SET, so union is `extend`
/// and difference is a filter. Works because our event graphs are SKOLEMIZED
/// (no blank nodes): graph equality is set equality, never isomorphism.
///
/// `arg` names the argument the text arrived in, which a depth refusal names (ledger #992):
/// the nesting is bounded BEFORE the parser sees the text, since the parser itself recurses.
///
/// Blank nodes are relabeled through `labels` in document order (ledger #1566): the parser
/// labels an anonymous node at random, and a cacheable answer must not. One `labels` across
/// both of an operation's documents keeps a label the two documents share naming one node, as
/// the parser's own labels did.
fn parse_graph(turtle: &str, who: &str, arg: &str, labels: &mut BlankNodeLabels) -> Result<Graph> {
    check_turtle_nesting(turtle, arg)?;
    let mut graph = Graph::default();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let quad = quad.map_err(|e| Error::Endpoint(format!("{who}: RDF parse error: {e}")))?;
        graph.insert(&labels.triple(quad.into()));
    }
    Ok(graph)
}

/// Serialize `graph` as Turtle in ONE order (ledger #1566): a `Graph` iterates in the order of
/// its interner's hashes, which a per-graph `RandomState` makes different on every run, so the
/// triples are sorted. Its blank nodes were relabeled as they were parsed, so all are stable.
fn serialize_graph(graph: &Graph) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle).for_writer(&mut out);
    for triple in canonical_triples(graph.iter().map(Triple::from), |_| true) {
        serializer
            .serialize_triple(&triple)
            .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
    }
    serializer
        .finish()
        .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
    Ok(out)
}

/// The second graph: the `with=` argument — inline Turtle when it starts like a
/// document, else an IRI sourced through the kernel (the compact-context
/// pattern: inline literal OR resolve the reference).
async fn with_graph(
    inv: &Invocation<'_>,
    who: &str,
    labels: &mut BlankNodeLabels,
) -> Result<Graph> {
    // Core's typed refusal, unchanged: `MissingArgument` when absent, `InvalidArgument` when
    // not inline or not UTF-8, so a caller can tell its own omission from a failure here.
    let with = inv.inline_str("with")?;
    if with.trim_start().starts_with('@') || with.trim_start().starts_with('<') {
        return parse_graph(with, who, "with", labels);
    }
    // Compose-marker convention: `with=<iri>?k=v&…` carries request arguments
    // (e.g. urn:org:agenda:week?as=text/turtle asks for the Turtle face).
    let (iri_str, query) = match with.split_once('?') {
        Some((iri, query)) => (iri, Some(query)),
        None => (with, None),
    };
    let iri = Iri::parse(iri_str)
        .map_err(|e| Error::Endpoint(format!("{who}: with= is neither Turtle nor an IRI: {e}")))?;
    let mut request = ikigai_core::Request::new(Verb::Source, iri);
    if let Some(query) = query {
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                request =
                    request.with_arg(key, ikigai_core::ArgRef::Inline(value.as_bytes().to_vec()));
            }
        }
    }
    let repr = inv.issue(request).await?;
    let text = String::from_utf8(repr.bytes)
        .map_err(|_| Error::Endpoint(format!("{who}: {with} is not UTF-8 Turtle")))?;
    // A resolved graph is caller text too: `with=` named it, so a refusal names `with`.
    parse_graph(&text, who, "with", labels)
}

/// `urn:rdf:union` — the piped graph ∪ the `with=` graph, as Turtle. Set
/// semantics: identical triples merge; skolemized IRIs make cross-source
/// identity literal (the org∪calendar merged view).
struct UnionEndpoint;

#[async_trait::async_trait]
impl Endpoint for UnionEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // Typed refusals from core (`MissingArgument`/`InvalidArgument`), as in `with_graph`.
        let content = inv.inline_str("content")?;
        let mut labels = BlankNodeLabels::new();
        let mut graph = parse_graph(content, "urn:rdf:union", "content", &mut labels)?;
        for triple in with_graph(inv, "urn:rdf:union", &mut labels).await?.iter() {
            graph.insert(triple);
        }
        // A set operation over its inputs: cacheable, and — as with transrept — only
        // as cacheable as the `with=` graph when that was resolved through the kernel
        // (the kernel folds the dependency's expiry and threads in). Inline inputs
        // carry no thread, and need none: the result is a function of the bytes.
        Ok(Representation::new(
            ReprType::new("text/turtle").with_param("charset", "utf-8"),
            serialize_graph(&graph)?,
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "rdf-union"
    }

    fn describe(&self) -> Description {
        Description::new("rdf-union")
            .title("Graph union")
            .summary(
                "The piped Turtle graph ∪ the with= graph (an IRI resolved through the                  kernel, or inline Turtle) — set semantics over skolemized triples.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary("the base Turtle graph — usually piped in")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("with")
                    .summary("the graph to union in: an IRI or inline Turtle")
                    .class(XSD_STRING),
            )
            .output("text/turtle")
    }
}

/// `urn:rdf:diff` — the triples on one side only. `mode=added` (default): in the
/// piped graph but not `with=` — what a sync must CREATE; `mode=removed`: in
/// `with=` but not the piped graph — what it must DELETE. Two calls, two
/// resources: the delta a materialized view applies.
struct DiffEndpoint;

#[async_trait::async_trait]
impl Endpoint for DiffEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // Typed refusals from core (`MissingArgument`/`InvalidArgument`), as in `with_graph`.
        let content = inv.inline_str("content")?;
        let mode = inv.inline_str("mode").unwrap_or("added");
        let mut labels = BlankNodeLabels::new();
        let ours = parse_graph(content, "urn:rdf:diff", "content", &mut labels)?;
        let theirs = with_graph(inv, "urn:rdf:diff", &mut labels).await?;
        let (keep, exclude) = match mode {
            "added" => (&ours, &theirs),
            "removed" => (&theirs, &ours),
            other => {
                return Err(Error::Endpoint(format!(
                    "urn:rdf:diff: mode must be added or removed, not `{other}`"
                )))
            }
        };
        let mut delta = Graph::default();
        for triple in keep.iter() {
            if !exclude.contains(triple) {
                delta.insert(triple);
            }
        }
        // Cacheable for the same reason union is: a set difference of its inputs,
        // inheriting the `with=` resolution's expiry when that was a resource.
        Ok(Representation::new(
            ReprType::new("text/turtle").with_param("charset", "utf-8"),
            serialize_graph(&delta)?,
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "rdf-diff"
    }

    fn describe(&self) -> Description {
        Description::new("rdf-diff")
            .title("Graph difference")
            .summary(
                "Triples on one side only. mode=added (default): in the piped graph, not                  in with= — what a sync creates. mode=removed: in with=, not in the piped                  graph — what it deletes.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary("the desired Turtle graph — usually piped in")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("with")
                    .summary("the current graph: an IRI or inline Turtle")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("mode")
                    .summary("which side of the diff to keep")
                    .class(XSD_STRING)
                    .one_of(["added", "removed"])
                    .default_value("added"),
            )
            .output("text/turtle")
    }
}

/// The name [`space`] claims: `urn:iki:space:rdf`.
pub const SPACE_ID: &str = "urn:iki:space:rdf";

/// The space binding `urn:rdf:union`, `urn:rdf:diff` and `urn:rdf:transrept`. It is
/// configuration-free, so it names itself [`SPACE_ID`]; a host that binds more doors onto it
/// drops that name (core 0.1.89) and names its own composition.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:rdf:union"), UnionEndpoint)
        .bind(Exact::new("urn:rdf:diff"), DiffEndpoint)
        .bind(Exact::new("urn:rdf:transrept"), TransreptEndpoint)
        .named(space_iri("rdf"))
}

/// `urn:rdf:transrept` — the RDF document in `content` (piped or named), or the resource
/// `src=` names, re-serialized as `as` (default Turtle).
struct TransreptEndpoint;

#[async_trait::async_trait]
impl Endpoint for TransreptEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let as_type = inv.inline_str("as").unwrap_or("text/turtle");
        // A member of the `content`/`src` group is GIVEN when it is non-empty (`InputGroup`):
        // a Sink-style engine sends an empty `content` when there is no body.
        let given = |name: &str| given(inv, name);
        let (media, bytes) = match given("src") {
            false => {
                // Core's typed refusal, unchanged: `MissingArgument` when absent (pipe a
                // resource in, or pass `content=…` or `src=…`), `InvalidArgument` when not
                // inline or not UTF-8.
                let input = inv.inline_str("content")?;
                transrept_bytes(input.as_bytes(), "content", as_type)?
            }
            true => {
                let src = inv.inline_str("src")?;
                if given("content") {
                    return Err(both_given());
                }
                let source = resolve_src(inv, src).await?;
                transrept_bytes(&source.bytes, "src", as_type)?
            }
        };
        // Transreption is a pure function of its input bytes, so its output is *as
        // cacheable as its input*. Mark it cacheable here; the kernel folds in the
        // expiry of whatever was piped in (`source <X> | urn:rdf:transrept …`), so a
        // stable source (e.g. urn:kernel:catalog) yields a cacheable result while a live
        // fetch (no Cache-Control) yields an uncacheable one — cacheability flows down
        // the pipe rather than being asserted unconditionally here. A `src=` read is a
        // sub-request, so the kernel folds its expiry and golden threads in the same way.
        Ok(transrepted(&media, bytes))
    }

    /// The body path (ledger #2278): Turtle (N-Triples with it) written as N-Triples or
    /// N-Quads is parsed AS IT ARRIVES and answered as a stream, one chunk of lines per chunk
    /// read, with the nesting bound counted on each chunk before the parser sees it. Every
    /// other pair, and every input that arrives as a buffer, is answered as [`invoke`]
    /// answers it, a streamed input collected first under the run's byte budget.
    ///
    /// [`invoke`]: Endpoint::invoke
    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let as_type = inv.inline_str("as").unwrap_or("text/turtle");
        let (arg, body) = match inv.request.args.get("src") {
            Some(ArgRef::Inline(src)) if !src.is_empty() => {
                if streams_to(as_type).is_none() {
                    return self.invoke(inv).await.map(Representation::into_body);
                }
                if given(inv, "content") {
                    return Err(both_given());
                }
                let src = inv.inline_str("src")?;
                ("src", resolve_src_body(inv, src).await?)
            }
            _ => match inv.request.args.get("content") {
                // A buffer (or nothing) is the bytes path's, unchanged.
                Some(ArgRef::Inline(_)) | None => {
                    return self.invoke(inv).await.map(Representation::into_body)
                }
                Some(_) if given(inv, "src") => return Err(both_given()),
                Some(_) => ("content", inv.content_body("content").await?),
            },
        };
        let held = match (streams_to(as_type), body) {
            (Some(to), Body::Once(stream)) => sniff_stream(stream).await?.lines(arg, to),
            (Some(to), Body::Replay(replay)) => sniff_stream(replay.open()).await?.lines(arg, to),
            (_, body) => Err(body),
        };
        let stream = match held {
            Ok(stream) => stream,
            Err(body) => {
                let bytes = inv.collect(body).await?;
                let (media, bytes) = transrept_bytes(&bytes, arg, as_type)?;
                return Ok(transrepted(&media, bytes).into_body());
            }
        };
        Ok(Representation::from_body(
            ReprType::new(media_base(as_type)).with_param("charset", "utf-8"),
            Body::Once(stream),
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "rdf-transrept"
    }

    fn describe(&self) -> Description {
        Description::new("rdf-transrept")
            .title("RDF transreption")
            .summary(
                "Re-serialize an RDF graph into another syntax — client-side content \
                 negotiation. Pipe a resource in (or name one with `src`) and choose `as`.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary(
                        "the RDF document to transrept — usually piped in (e.g. from \
                         urn:http:get). Give this or src, not both",
                    )
                    .class(XSD_STRING)
                    .optional(),
            )
            .input(
                ArgSpec::new("src")
                    .summary(
                        "the IRI of an RDF resource to transrept instead of content: resolved \
                         through the kernel under the caller's capability (an http(s) URL via \
                         urn:http:get), so the answer follows the source's golden thread",
                    )
                    .class(XSD_ANY_URI)
                    .optional(),
            )
            .group(InputGroup::exactly_one(["content", "src"]).pipe("content"))
            .input(
                ArgSpec::new("as")
                    .summary(
                        "target representation: text/turtle (default), \
                         application/n-triples, application/n-quads, application/trig, \
                         application/rdf+xml, application/ld+json, or text/html",
                    )
                    .class(XSD_STRING)
                    .default_value("text/turtle"),
            )
            // Every face `as=` can select is a declared output — the same list as
            // the transreptor's `to` below (a test pins that). Declaring them all is
            // what lets a conformance walk resolve and parse each RDF face in turn.
            .output("text/turtle")
            .output("application/n-triples")
            .output("application/n-quads")
            .output("application/trig")
            .output("application/rdf+xml")
            .output("application/ld+json")
            .output("text/html")
            // First-class `ik:Transreptor`: the media types it converts between. The
            // input syntax is *sniffed* (see `sniff`), so it accepts opaque
            // `application/octet-stream` too — the universal "raw, not-yet-typed
            // bytes" a fetch or file read delivers. `text/html` is output-only (the
            // human subject/predicate/object table). Drives selection: a single
            // auto-invocable hop (`content` + `as`) over any of these.
            .transreptor(
                [
                    "text/turtle",
                    "application/n-triples",
                    "application/n-quads",
                    "application/trig",
                    "application/rdf+xml",
                    "application/ld+json",
                    "application/octet-stream",
                ],
                [
                    "text/turtle",
                    "application/n-triples",
                    "application/n-quads",
                    "application/trig",
                    "application/rdf+xml",
                    "application/ld+json",
                    "text/html",
                ],
            )
            // Reads `content` as a body, so a plan hands it a stream uncollected: Turtle
            // (N-Triples with it) to N-Triples or N-Quads is parsed as it arrives; any
            // other pair is collected here, under the run's byte budget.
            .incremental()
            // A streamed input collected past the run's byte budget is refused.
            .fails(FailureClass::OverBudget)
    }
}

/// A transrepted answer: `bytes` as `media`, UTF-8, as cacheable as its input.
///
/// Transreption is a pure function of its input bytes, so its output is *as cacheable as its
/// input*: marked cacheable here, the kernel folds in the expiry of whatever was piped in or
/// read by `src=`.
fn transrepted(media: &str, bytes: Vec<u8>) -> Representation {
    Representation::new(ReprType::new(media).with_param("charset", "utf-8"), bytes).cacheable()
}

/// Whether the `content`/`src` member `name` is GIVEN: present and, inline, non-empty
/// (`InputGroup`: a Sink-style engine sends an empty `content` when there is no body).
fn given(inv: &Invocation<'_>, name: &str) -> bool {
    match inv.request.args.get(name) {
        Some(ArgRef::Inline(bytes)) => !bytes.is_empty(),
        Some(_) => true,
        None => false,
    }
}

/// The refusal for a call that gives both `content` and `src`.
fn both_given() -> Error {
    Error::InvalidArgument {
        name: "src".to_string(),
        detail: "give content= or src=, not both".to_string(),
    }
}

/// The targets written as the input is read (ledger #2278): N-Triples and N-Quads, one
/// statement a line, so nothing waits for the end of the document.
fn streams_to(as_type: &str) -> Option<RdfFormat> {
    match format_for(as_type)? {
        to @ (RdfFormat::NTriples | RdfFormat::NQuads) => Some(to),
        _ => None,
    }
}

/// How much of a streamed input is held to [`sniff`] it before giving up and collecting it
/// (a document opening with more whitespace than this): the opening `<…>` token needs
/// [`ANGLE_SCAN`] bytes after the whitespace.
const SNIFF_HOLD: usize = 64 * 1024;

/// A streamed input, its opening read: the chunks held for the sniff, the stream after them,
/// and the syntax, when the held bytes decide it as they would decide the whole document.
struct Sniffed {
    format: Option<RdfFormat>,
    held: Vec<Bytes>,
    rest: ByteStream,
}

/// Read the opening of `stream` until [`sniff`] would answer for it what it answers for the
/// whole document (the first byte after the whitespace, and [`ANGLE_SCAN`] after it when that
/// is a `<`), or it ends, or [`SNIFF_HOLD`] bytes are held undecided.
async fn sniff_stream(mut rest: ByteStream) -> Result<Sniffed> {
    let (mut prefix, mut held) = (Vec::new(), Vec::new());
    let mut ended = false;
    let decided = |prefix: &[u8]| {
        let rest = &prefix[prefix
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count()..];
        match rest.first() {
            None => false,
            Some(b'<') => rest.len() >= ANGLE_SCAN,
            Some(_) => true,
        }
    };
    while !decided(&prefix) && prefix.len() <= SNIFF_HOLD {
        match rest.next().await {
            Some(chunk) => {
                let chunk = chunk?;
                prefix.extend_from_slice(&chunk[..chunk.len().min(SNIFF_HOLD + 1)]);
                held.push(chunk);
            }
            None => {
                ended = true;
                break;
            }
        }
    }
    let format = (ended || decided(&prefix)).then(|| sniff(&prefix));
    Ok(Sniffed { format, held, rest })
}

impl Sniffed {
    /// The whole input again, as a body.
    fn body(self) -> Body {
        let held = futures_util::stream::iter(self.held.into_iter().map(Ok));
        Body::once(held.chain(self.rest))
    }

    /// The input transrepted to `to` as a stream of lines when it is Turtle, else the input
    /// back, whole, for the bytes path.
    fn lines(self, arg: &'static str, to: RdfFormat) -> std::result::Result<ByteStream, Body> {
        match self.format {
            Some(RdfFormat::Turtle) => Ok(turtle_lines(self.body().into_stream(), arg, to)),
            _ => Err(self.body()),
        }
    }
}

/// Turtle read from `input` and written as `to` (N-Triples or N-Quads) AS IT IS READ: every
/// chunk is scanned for nesting ([`TurtleNesting`], refused past [`MAX_TURTLE_NESTING`]
/// before the parser sees a byte of it), handed to oxttl's push parser, and answered as the
/// lines of the statements it completed. The same parser, the same blank-node labels and the
/// same serializer as [`convert`], so the stream, collected, is the bytes path's answer. A
/// parse error or a refusal ends the stream as an error item, never a clean end.
///
/// [`MAX_TURTLE_NESTING`]: crate::MAX_TURTLE_NESTING
fn turtle_lines(input: ByteStream, arg: &'static str, to: RdfFormat) -> ByteStream {
    struct Lines {
        input: ByteStream,
        scan: TurtleNesting<'static>,
        parser: LowLevelTurtleParser,
        labels: BlankNodeLabels,
        to: RdfFormat,
        ended: bool,
    }
    impl Lines {
        /// The lines of every statement the parser has completed.
        fn drain(&mut self) -> Result<Vec<u8>> {
            let mut out = Vec::new();
            let mut serializer = RdfSerializer::from_format(self.to).for_writer(&mut out);
            while let Some(triple) = self.parser.parse_next() {
                let triple =
                    triple.map_err(|e| Error::Endpoint(format!("RDF parse error: {e}")))?;
                let quad = self.labels.quad(triple.in_graph(GraphName::DefaultGraph));
                serializer
                    .serialize_quad(&quad)
                    .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
            }
            serializer
                .finish()
                .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
            Ok(out)
        }
    }
    let lines = Lines {
        input,
        scan: TurtleNesting::new(arg),
        parser: TurtleParser::new().low_level(),
        labels: BlankNodeLabels::new(),
        to,
        ended: false,
    };
    Box::pin(futures_util::stream::try_unfold(
        lines,
        |mut lines| async move {
            while !lines.ended {
                match lines.input.next().await {
                    Some(chunk) => {
                        let chunk = chunk?;
                        lines.scan.feed(&chunk)?;
                        lines.parser.extend_from_slice(&chunk);
                    }
                    None => {
                        lines.parser.end();
                        lines.ended = true;
                    }
                }
                let out = lines.drain()?;
                if !out.is_empty() {
                    return Ok(Some((Bytes::from(out), lines)));
                }
            }
            Ok(None)
        },
    ))
}

/// The HTTP module's GET, by the name ikigai-http 0.2.0 gave it (ledger #11).
const HTTP_GET: &str = "urn:http:get";

/// Read the resource `src=` names through the kernel, under this invocation's (the caller's)
/// capability — never a wider one — and as a recorded dependency, so its expiry and golden
/// threads flow into the transreption. An `http(s)://` URL is not itself a bound resource, so
/// it is fetched through [`HTTP_GET`], as ikigai-sparql's `graph=` is; anything else resolves
/// directly.
async fn resolve_src(inv: &Invocation<'_>, src: &str) -> Result<Representation> {
    let src = src.trim();
    let invalid = |detail: String| Error::InvalidArgument {
        name: "src".to_string(),
        detail,
    };
    if src.starts_with("http://") || src.starts_with("https://") {
        let get = Iri::parse(HTTP_GET).expect("the HTTP GET name is a valid IRI");
        let request = ikigai_core::Request::new(Verb::Source, get)
            .with_arg("url", ikigai_core::ArgRef::Inline(src.as_bytes().to_vec()));
        return inv.issue(request).await;
    }
    let iri = Iri::parse(src).map_err(|e| invalid(format!("`{src}` is not an IRI: {e}")))?;
    inv.source(&iri).await
}

/// [`resolve_src`], answered as the body the source produced (a stream stays one).
async fn resolve_src_body(inv: &Invocation<'_>, src: &str) -> Result<Body> {
    let src = src.trim();
    if src.starts_with("http://") || src.starts_with("https://") {
        let get = Iri::parse(HTTP_GET).expect("the HTTP GET name is a valid IRI");
        let request = ikigai_core::Request::new(Verb::Source, get)
            .with_arg("url", ArgRef::Inline(src.as_bytes().to_vec()));
        return Ok(inv.issue_body(request).await?.bytes);
    }
    let iri = Iri::parse(src).map_err(|e| Error::InvalidArgument {
        name: "src".to_string(),
        detail: format!("`{src}` is not an IRI: {e}"),
    })?;
    Ok(inv.source_body(&iri).await?.bytes)
}

/// The pure transformation, factored out so it's testable without an [`Invocation`]:
/// parse `input` (input syntax sniffed) and serialize it as `as_type`. Returns the
/// canonical media type and the serialized bytes.
///
/// `arg` names the argument the document arrived in (`content`, or `src` for a resolved
/// one), which a depth refusal names.
fn transrept_bytes(input: &[u8], arg: &str, as_type: &str) -> Result<(String, Vec<u8>)> {
    let from = sniff(input);
    // Bound nesting before any parser, serializer or `Display` recurses over it, with the scan
    // that matches the parser `from` selects: triple-term depth for RDF/XML and for everything
    // oxttl reads (ledger #992); JSON depth for JSON-LD, which carries no triple terms but whose
    // expander recurses once per nested node object, so it also parses on a sized thread
    // (ledger #1038, see `depth`).
    match from {
        RdfFormat::RdfXml => check_rdfxml_nesting(input, arg)?,
        RdfFormat::JsonLd { .. } => {
            check_json_nesting(input, arg)?;
            return depth::on_json_ld_stack(|| convert(input, from, as_type));
        }
        _ => check_turtle_nesting(&String::from_utf8_lossy(input), arg)?,
    }
    convert(input, from, as_type)
}

/// Parse `input` as `from` and serialize it as `as_type`: the half of [`transrept_bytes`] after
/// the bounds, run on the JSON-LD thread when `from` is JSON-LD.
fn convert(input: &[u8], from: RdfFormat, as_type: &str) -> Result<(String, Vec<u8>)> {
    // Transreption streams, so its order is the document's and needs no sorting; but the
    // parser labels anonymous nodes at random, so every face relabels them in document order
    // (ledger #1566).
    let mut labels = BlankNodeLabels::new();
    let quads = RdfParser::from_format(from)
        .for_slice(input)
        .map(move |quad| quad.map(|quad| labels.quad(quad)));

    // The human view: a subject/predicate/object table over the parsed triples.
    if media_base(as_type) == "text/html" {
        let html = to_html(quads)?;
        return Ok(("text/html".to_string(), html.into_bytes()));
    }

    let to = format_for(as_type).ok_or_else(|| {
        Error::Endpoint(format!(
            "urn:rdf:transrept: unknown target `{as_type}` — try text/turtle, \
             application/n-triples, application/n-quads, application/trig, \
             application/rdf+xml, application/ld+json, or text/html"
        ))
    })?;

    let mut out = Vec::new();
    let mut serializer = RdfSerializer::from_format(to).for_writer(&mut out);
    for quad in quads {
        let quad = quad.map_err(|e| Error::Endpoint(format!("RDF parse error: {e}")))?;
        serializer
            .serialize_quad(&quad)
            .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
    }
    serializer
        .finish()
        .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
    Ok((media_base(as_type).to_string(), out))
}

/// Sniff the input syntax from its opening token: `{`/`[` ⇒ JSON-LD; a leading `<…>`
/// that is an IRI (`<scheme:…>`) ⇒ Turtle/N-Triples (a triple's subject), whereas a
/// leading `<` opening an XML element (`<?xml`, `<rdf:RDF`, `<Foo`) ⇒ RDF/XML; anything
/// else (`@prefix`, a prefixed name, a comment) ⇒ Turtle, which subsumes N-Triples.
/// The IRI-vs-element test is [`angle_token_is_iri`] — both syntaxes open with `<`.
fn sniff(bytes: &[u8]) -> RdfFormat {
    let rest = &bytes[bytes.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
    match rest.first() {
        Some(b'{') | Some(b'[') => json_ld(),
        Some(b'<') if angle_token_is_iri(rest) => RdfFormat::Turtle,
        Some(b'<') => RdfFormat::RdfXml,
        _ => RdfFormat::Turtle,
    }
}

/// How many opening bytes the `<…>` test scans for the closing bracket.
const ANGLE_SCAN: usize = 2048;

/// For a leading `<…>` token, whether it is a Turtle/N-Triples IRI subject rather than an
/// XML start tag — the whole of the distinction, since both syntaxes open with `<`.
///
/// The test used to be "does the token contain `://`?", which asks about an *authority*,
/// not a scheme — so every `urn:` IRI (the scheme ikigai names all of its resources with)
/// read as an element tag, and a Turtle document went to the RDF/XML parser to die as
/// `Unknown prefix urn:`. Three cheap signals replace it, all on the bracketed token:
///
/// 1. it closes within the scan window with no character Turtle's `IRIREF` production
///    forbids before the `>` — which rules out every start tag carrying attributes, plus
///    `<?xml …?>` and `<!DOCTYPE …>`;
/// 2. the text before its first `:` is a URI scheme ([`is_uri_scheme`]) with something
///    after the colon — `<doc>` and `<html>` carry no scheme at all;
/// 3. it is not *also* a well-formed XML QName ([`is_xml_qname`]). This is the tie-break
///    `://` was standing in for: an XML name admits only letters, digits, `.`, `-`, `_`
///    and the one prefix colon, so an attribute-less `<rdf:RDF>` satisfies (1) and (2) and
///    is resolved as markup, while `urn:demo:a`'s second colon and `http://ex/a`'s slashes
///    cannot occur in one.
///
/// The residue is a single-colon, all-name-character IRI such as `<urn:x>`, which reads as
/// a tag: nothing in the token separates the two shapes, and ikigai's own URNs are
/// `urn:nid:nss`. Kept in step with the copies in `ikigai-sniff` and `ikigai-sparql`.
fn angle_token_is_iri(rest: &[u8]) -> bool {
    let window = &rest[1..rest.len().min(ANGLE_SCAN)];
    let Some(end) = window.iter().position(|&b| b == b'>') else {
        return false; // unterminated within the window: not an IRI reference
    };
    let token = &window[..end];
    if !token.iter().all(|&b| is_iri_char(b)) {
        return false;
    }
    let Some(colon) = token.iter().position(|&b| b == b':') else {
        return false;
    };
    let (scheme, after_scheme) = token.split_at(colon);
    is_uri_scheme(scheme) && after_scheme.len() > 1 && !is_xml_qname(token)
}

/// Whether a byte may appear inside a Turtle `IRIREF`. The production excludes the ASCII
/// control range, the space, and ``"<>\^`{|}`` — the same set RFC 3987 keeps out of an IRI.
/// (A single quote is *not* excluded; an attribute written with them, `xmlns='…'`, is
/// caught by the space in front of it.)
fn is_iri_char(b: u8) -> bool {
    !(b <= 0x20
        || b == 0x7F
        || matches!(
            b,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        ))
}

/// Whether the bytes are a URI scheme: `[A-Za-z][A-Za-z0-9+.-]*` (RFC 3986 §3.1).
fn is_uri_scheme(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b) if b.is_ascii_alphabetic())
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Whether the token is *also* a well-formed XML QName — `prefix:local`, where both parts
/// are XML names — which is the shape an RDF/XML root tag takes when it carries no
/// attributes (`<rdf:RDF>`). ASCII-only: a non-ASCII name character puts the token outside
/// the overlap this has to arbitrate, and [`is_iri_char`] has already admitted it.
fn is_xml_qname(token: &[u8]) -> bool {
    token.iter().filter(|&&b| b == b':').count() == 1
        && token
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// Map an `as` value — a media type (with optional params) or a short alias — to an
/// [`RdfFormat`]. `None` for anything not a known RDF serialization.
fn format_for(as_type: &str) -> Option<RdfFormat> {
    let media = media_base(as_type);
    if let Some(format) = RdfFormat::from_media_type(media) {
        return Some(format);
    }
    Some(match media {
        "turtle" | "ttl" => RdfFormat::Turtle,
        "ntriples" | "nt" | "n-triples" => RdfFormat::NTriples,
        "nquads" | "nq" | "n-quads" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdfxml" | "rdf/xml" | "xml" => RdfFormat::RdfXml,
        "jsonld" | "json-ld" | "json" => json_ld(),
        _ => return None,
    })
}

/// The bare media type (strip parameters and surrounding whitespace).
fn media_base(media: &str) -> &str {
    media.split(';').next().unwrap_or(media).trim()
}

/// JSON-LD with its default profile (the variant carries a profile set).
fn json_ld() -> RdfFormat {
    RdfFormat::from_media_type("application/ld+json").expect("ld+json is a known media type")
}

/// Render the parsed triples as an HTML table — the "RDF is just data" view.
fn to_html<E: std::fmt::Display>(
    quads: impl Iterator<Item = std::result::Result<Quad, E>>,
) -> Result<String> {
    let mut rows = String::new();
    let mut count = 0usize;
    for quad in quads {
        let quad = quad.map_err(|e| Error::Endpoint(format!("RDF parse error: {e}")))?;
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&quad.subject.to_string()),
            esc(&quad.predicate.to_string()),
            esc(&quad.object.to_string()),
        ));
        count += 1;
    }
    Ok(format!(
        "<table class=\"rdf\"><caption>{count} triples</caption>\
         <thead><tr><th>subject</th><th>predicate</th><th>object</th></tr></thead>\
         <tbody>{rows}</tbody></table>"
    ))
}

/// Minimal HTML escaping for term strings dropped into the table cells.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, FnEndpoint, Kernel, Request};
    use std::sync::Arc;

    const GRAPH_A: &str = r#"<urn:event:1> <urn:p:summary> "Standup" .
<urn:event:2> <urn:p:summary> "Dinner" ."#;
    const GRAPH_B: &str = r#"<urn:event:2> <urn:p:summary> "Dinner" .
<urn:event:3> <urn:p:summary> "Dentist" ."#;

    fn graph_kernel() -> Kernel {
        // urn:test:b serves GRAPH_B so with= can resolve by reference.
        let space = space().bind(
            Exact::new("urn:test:b"),
            FnEndpoint::new("b", |_inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    GRAPH_B.as_bytes().to_vec(),
                ))
            }),
        );
        Kernel::new(Arc::new(space))
    }

    fn run(iri: &str, args: &[(&str, &str)]) -> Graph {
        let kernel = graph_kernel();
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ikigai_core::ArgRef::Inline(v.as_bytes().to_vec()));
        }
        let out = block_on(kernel.issue(request, &Capability::root())).unwrap();
        parse_graph(
            &String::from_utf8(out.bytes).unwrap(),
            "test",
            "content",
            &mut BlankNodeLabels::new(),
        )
        .unwrap()
    }

    #[test]
    fn union_merges_with_set_semantics() {
        let merged = run(
            "urn:rdf:union",
            &[("content", GRAPH_A), ("with", "urn:test:b")],
        );
        // 2 + 2 triples with 1 shared -> 3, not 4: identical triples merge.
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn diff_added_and_removed_split_the_delta() {
        let added = run(
            "urn:rdf:diff",
            &[("content", GRAPH_A), ("with", "urn:test:b")],
        );
        assert_eq!(added.len(), 1, "only urn:event:1 is new");
        assert!(added
            .iter()
            .next()
            .unwrap()
            .subject
            .to_string()
            .contains("urn:event:1"));

        let removed = run(
            "urn:rdf:diff",
            &[
                ("content", GRAPH_A),
                ("with", "urn:test:b"),
                ("mode", "removed"),
            ],
        );
        assert_eq!(removed.len(), 1, "only urn:event:3 is gone");
        assert!(removed
            .iter()
            .next()
            .unwrap()
            .subject
            .to_string()
            .contains("urn:event:3"));
    }

    #[test]
    fn with_accepts_inline_turtle_too() {
        let merged = run("urn:rdf:union", &[("content", GRAPH_A), ("with", GRAPH_B)]);
        assert_eq!(merged.len(), 3);
    }

    const TTL: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<http://example.org/me> foaf:name "Ada" ; foaf:knows <http://example.org/you> ."#;

    fn body(input: &str, as_type: &str) -> String {
        let (_, bytes) = transrept_bytes(input.as_bytes(), "content", as_type).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn turtle_to_ntriples_lists_each_triple() {
        let nt = body(TTL, "application/n-triples");
        // N-Triples is one fully-qualified triple per line.
        assert!(nt.contains("<http://example.org/me> <http://xmlns.com/foaf/0.1/name> \"Ada\""));
        assert!(nt.contains("<http://xmlns.com/foaf/0.1/knows> <http://example.org/you>"));
        assert_eq!(nt.lines().filter(|l| !l.trim().is_empty()).count(), 2);
    }

    #[test]
    fn turtle_round_trips_through_rdfxml_and_jsonld() {
        // Re-serialize to RDF/XML then JSON-LD, and confirm the data survives by
        // transrepting each back to N-Triples and comparing the triple set.
        let canonical = body(TTL, "application/n-triples");
        for via in ["application/rdf+xml", "application/ld+json", "text/turtle"] {
            let intermediate = body(TTL, via);
            let back = body(&intermediate, "application/n-triples");
            let set = |s: &str| {
                let mut v: Vec<String> = s
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect();
                v.sort();
                v
            };
            assert_eq!(
                set(&canonical),
                set(&back),
                "round-trip via {via} lost data"
            );
        }
    }

    #[test]
    fn html_view_tabulates_the_triples() {
        let html = body(TTL, "text/html");
        assert!(html.contains("<table"));
        assert!(html.contains("2 triples"));
        assert!(html.contains("http://example.org/me"));
        assert!(html.contains("&lt;") || html.contains("Ada")); // escaped / literal present
    }

    /// A graph named entirely in `urn:` — the shape every ikigai resource takes.
    const URN_TTL: &str = "<urn:demo:a> <urn:demo:p> <urn:demo:b> .\n\
                           <urn:demo:a> <urn:demo:q> \"lit\" .\n";

    #[test]
    fn a_urn_subject_is_sniffed_as_turtle_not_rdfxml() {
        // The regression: `urn:` has no authority, so the old `://` discriminator read
        // `<urn:demo:a>` as an XML tag and handed Turtle to the RDF/XML parser, which
        // failed with `Unknown prefix urn:` — an XML error raised from a Turtle document.
        assert_eq!(sniff(URN_TTL.as_bytes()), RdfFormat::Turtle);
        assert_eq!(sniff(b"<http://ex/a> <http://ex/p> 1 ."), RdfFormat::Turtle);
        assert_eq!(
            sniff(b"<mailto:ada@ex.org> <urn:demo:p> 1 ."),
            RdfFormat::Turtle
        );
        // Markup still sniffs as markup, attributes or not.
        assert_eq!(
            sniff(br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#),
            RdfFormat::RdfXml
        );
        assert_eq!(sniff(b"<rdf:RDF></rdf:RDF>"), RdfFormat::RdfXml);
        assert_eq!(
            sniff(br#"<?xml version="1.0"?><rdf:RDF/>"#),
            RdfFormat::RdfXml
        );
    }

    #[test]
    fn a_urn_only_graph_round_trips_through_every_syntax() {
        // End to end, not just the sniff: `source urn:<a graph> | urn:rdf:transrept` is
        // the call the bug broke, and re-sniffing each intermediate is what makes it a
        // round trip rather than a one-way serialization.
        let canonical = body(URN_TTL, "application/n-triples");
        assert!(canonical.contains("<urn:demo:a> <urn:demo:p> <urn:demo:b>"));
        for via in ["application/rdf+xml", "application/ld+json", "text/turtle"] {
            let back = body(&body(URN_TTL, via), "application/n-triples");
            let set = |s: &str| {
                let mut v: Vec<String> = s
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect();
                v.sort();
                v
            };
            assert_eq!(
                set(&canonical),
                set(&back),
                "round-trip via {via} lost data"
            );
        }
    }

    #[test]
    fn rdfxml_input_is_sniffed_and_parsed() {
        let xml = body(TTL, "application/rdf+xml"); // produce RDF/XML
        let nt = body(&xml, "application/n-triples"); // feed it back; sniff → RDF/XML
        assert!(nt.contains("\"Ada\""));
    }

    #[test]
    fn unknown_target_is_a_clean_error() {
        let err = transrept_bytes(TTL.as_bytes(), "content", "application/x-nonsense").unwrap_err();
        assert!(format!("{err}").contains("unknown target"));
    }

    #[test]
    fn describes_itself_as_a_transreptor() {
        use ikigai_core::{Iri, Request, Resolution, Scope, Space};
        let request = Request::new(Verb::Meta, Iri::parse("urn:rdf:transrept").unwrap());
        let Resolution::Hit(resolved) = space().resolve(&request, &Scope::empty()) else {
            panic!("urn:rdf:transrept resolves");
        };
        let description = resolved.endpoint.describe();
        let t = description
            .transreption()
            .expect("rdf-transrept is an ik:Transreptor");
        // Sniffs its input, so opaque octet-stream is a valid `from`; html is output-only.
        assert!(t.from.contains(&"application/octet-stream".to_string()));
        assert!(t.from.contains(&"text/turtle".to_string()));
        assert!(t.to.contains(&"application/rdf+xml".to_string()));
        assert!(t.to.contains(&"text/html".to_string()));
        assert!(
            !t.from.contains(&"text/html".to_string()),
            "html is output-only"
        );
    }

    /// Transrept `src=` as `as_type` through `kernel` under `cap`.
    fn transrept_src(
        kernel: &Kernel,
        src: &str,
        as_type: &str,
        cap: &Capability,
    ) -> Result<Representation> {
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg("src", ikigai_core::ArgRef::Inline(src.as_bytes().to_vec()))
            .with_arg(
                "as",
                ikigai_core::ArgRef::Inline(as_type.as_bytes().to_vec()),
            );
        block_on(kernel.issue(request, cap))
    }

    /// `src=<iri>` resolves the named resource through the kernel and transrepts its bytes
    /// (ledger #2144): the composition a caller used to do by reading it and passing `content=`.
    #[test]
    fn src_transrepts_a_referenced_resource() {
        let out = transrept_src(
            &graph_kernel(),
            "urn:test:b",
            "application/n-triples",
            &Capability::root(),
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "application/n-triples");
        let nt = String::from_utf8(out.bytes).unwrap();
        assert!(
            nt.contains("<urn:event:3> <urn:p:summary> \"Dentist\" ."),
            "{nt}"
        );
        assert_eq!(nt.lines().count(), 2, "{nt}");
        // The same answer as reading it and passing the bytes inline.
        let (_, inline) = transrept_bytes(GRAPH_B.as_bytes(), "content", "nt").unwrap();
        assert_eq!(nt.as_bytes(), inline);
    }

    /// The source is read under the CALLER's capability, never this module's: a caller that
    /// cannot read it is denied through `src=` too, and one that can is answered.
    #[test]
    fn src_is_read_under_the_callers_capability() {
        let space = space().bind(
            Exact::new("urn:test:secret"),
            FnEndpoint::new("secret", |_inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    GRAPH_A.as_bytes().to_vec(),
                ))
            })
            .with_description(
                Description::new("secret")
                    .verb(Verb::Source)
                    .requires("urn:cap:test:secret"),
            ),
        );
        let kernel = Kernel::new(Arc::new(space));
        let err = transrept_src(
            &kernel,
            "urn:test:secret",
            "text/turtle",
            &Capability::scoped(["urn:cap:test:other"]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        let out = transrept_src(
            &kernel,
            "urn:test:secret",
            "application/n-triples",
            &Capability::scoped(["urn:cap:test:secret"]),
        )
        .unwrap();
        assert!(String::from_utf8(out.bytes).unwrap().contains("Standup"));
    }

    /// The answer carries the source's golden thread: cached while the source stands, and
    /// recomputed from the source's new bytes once its thread is cut.
    #[test]
    fn src_is_golden_threaded_from_the_source() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let space = space().bind(
            Exact::new("urn:test:cell"),
            FnEndpoint::new("cell", move |_inv: &Invocation<'_>| {
                let graph = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    GRAPH_A
                } else {
                    GRAPH_B
                };
                Ok(
                    Representation::new(ReprType::new("text/turtle"), graph.as_bytes().to_vec())
                        .cacheable()
                        .depends_on("urn:test:cell:state"),
                )
            }),
        );
        let kernel = Kernel::new(Arc::new(space));
        let read = || {
            let out = transrept_src(&kernel, "urn:test:cell", "nt", &Capability::root()).unwrap();
            String::from_utf8(out.bytes).unwrap()
        };
        let first = read();
        assert!(first.contains("Standup"), "{first}");
        assert_eq!(read(), first, "served from cache while the source stands");
        assert_eq!(reads.load(Ordering::SeqCst), 1, "the source was read once");
        kernel.cut("urn:test:cell:state");
        let after = read();
        assert!(
            after.contains("Dentist") && !after.contains("Standup"),
            "{after}"
        );
    }

    /// A resolved `src=` document meets the same depth bound as `content=` before any parser
    /// sees it, and the refusal names `src`, the argument that carried it.
    #[test]
    fn a_too_deep_src_document_is_refused_naming_src() {
        let space = space().bind(
            Exact::new("urn:test:deep"),
            FnEndpoint::new("deep", |_inv: &Invocation<'_>| {
                let deep = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
                Ok(Representation::new(
                    ReprType::new("application/ld+json"),
                    deep.into_bytes(),
                ))
            }),
        );
        let kernel = Kernel::new(Arc::new(space));
        match transrept_src(&kernel, "urn:test:deep", "nt", &Capability::root()) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "src", "{detail}");
                assert!(detail.contains("MAX_JSON_NESTING"), "{detail}");
            }
            other => panic!("a too-deep src= document: {other:?}"),
        }
    }

    /// `content=` and `src=` are the two ways in, exactly one at a time: both is a typed
    /// `InvalidArgument`, and a `src=` that is not an IRI is refused naming `src`.
    #[test]
    fn content_and_src_are_exactly_one() {
        let kernel = graph_kernel();
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg(
                "content",
                ikigai_core::ArgRef::Inline(GRAPH_A.as_bytes().to_vec()),
            )
            .with_arg("src", ikigai_core::ArgRef::Inline(b"urn:test:b".to_vec()));
        match block_on(kernel.issue(request, &Capability::root())) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "src"),
            other => panic!("content= and src= together: {other:?}"),
        }
        match transrept_src(&kernel, "not an iri", "nt", &Capability::root()) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "src"),
            other => panic!("a src= that is no IRI: {other:?}"),
        }
    }

    /// A required input left out is refused as core's typed `MissingArgument` naming it,
    /// never an untyped endpoint error: the caller's omission, not this module's failure
    /// (ikigai-conformance 0.8.0's REQUIRED check).
    #[test]
    fn a_missing_required_input_is_a_typed_missing_argument() {
        let kernel = graph_kernel();
        // (endpoint, the arguments given, the required one left out)
        type Case = (
            &'static str,
            &'static [(&'static str, &'static str)],
            &'static str,
        );
        let cases: [Case; 5] = [
            ("urn:rdf:transrept", &[], "content"),
            ("urn:rdf:union", &[("with", GRAPH_B)], "content"),
            ("urn:rdf:union", &[("content", GRAPH_A)], "with"),
            ("urn:rdf:diff", &[("with", GRAPH_B)], "content"),
            ("urn:rdf:diff", &[("content", GRAPH_A)], "with"),
        ];
        for (iri, args, missing) in cases {
            let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
            for &(k, v) in args {
                request = request.with_arg(k, ikigai_core::ArgRef::Inline(v.as_bytes().to_vec()));
            }
            match block_on(kernel.issue(request, &Capability::root())) {
                Err(Error::MissingArgument(name)) => assert_eq!(name, missing, "{iri}"),
                other => {
                    panic!("{iri} without `{missing}`: expected MissingArgument, got {other:?}")
                }
            }
        }
    }
}
