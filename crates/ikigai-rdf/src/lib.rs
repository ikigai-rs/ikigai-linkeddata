//! `ikigai-rdf` — RDF **transreption** as an ikigai resource.
//!
//! `urn:rdf:transrept` takes an RDF document and re-serializes it into another syntax,
//! chosen by the `as` argument (content negotiation): Turtle, N-Triples, N-Quads, TriG,
//! RDF/XML, JSON-LD, or a human-readable HTML table. The input syntax is sniffed (an
//! explicit format isn't needed for the common cases).
//!
//! It's the first step toward NetKernel-style *transreption* — lossless transformation
//! between representations of the same resource. Built on Oxigraph's **pure** parser/
//! serializer crates (`oxrdfio`/`oxrdf`) — no store, no rocksdb — so it compiles to
//! `wasm32` and the browser can transrept a fetched graph **client-side**:
//!
//! ```text
//! source urn:httpGet url=https://example.org/thing | urn:rdf:transrept as=text/turtle
//! ```
//!
//! The kernel pipes the fetched bytes into `content`; `as` picks the representation.

#![forbid(unsafe_code)]

mod depth;
pub use depth::{check_rdfxml_nesting, check_turtle_nesting, MAX_TURTLE_NESTING};

use ikigai_core::{
    space_iri, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FnEndpoint, Invocation,
    Iri, ReprType, Representation, Result, Verb,
};
use oxrdf::{Graph, NamedOrBlankNode, Quad, Term};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};

/// The XSD `string` datatype IRI — the `class` of every by-value input here. An RDF
/// document, a graph reference-or-document, a media type and a mode token are all
/// scalars an agent supplies as text; `ArgSpec` has no narrower class for a document.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The `rdfs:subClassOf` IRI.
const SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// Parse a Turtle alignment graph and return every `rdfs:subClassOf` axiom as a
/// `(subclass, superclass)` IRI pair — the closure pairs a host feeds to the kernel's
/// type-aware action selection (`Kernel::with_subclass_axioms`), so an entity typed
/// `foaf:Person` satisfies a `schema:Person` action. Best-effort: triples whose subject or
/// object isn't an IRI (blank nodes, literals) and any unparseable triples are skipped —
/// `subClassOf` relates named classes.
pub fn subclass_axioms(turtle: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let Ok(quad) = quad else { continue };
        if quad.predicate.as_str() != SUBCLASS_OF {
            continue;
        }
        if let (NamedOrBlankNode::NamedNode(sub), Term::NamedNode(sup)) =
            (&quad.subject, &quad.object)
        {
            pairs.push((sub.as_str().to_string(), sup.as_str().to_string()));
        }
    }
    pairs
}

/// The space binding `urn:rdf:transrept`. Mount it in any kernel (the CLI's embedded
/// space, the in-browser kernel) to give it RDF content negotiation.
/// Parse Turtle into an [`oxrdf::Graph`] — a triple SET, so union is `extend`
/// and difference is a filter. Works because our event graphs are SKOLEMIZED
/// (no blank nodes): graph equality is set equality, never isomorphism.
///
/// `arg` names the argument the text arrived in, which a depth refusal names (ledger #992):
/// the nesting is bounded BEFORE the parser sees the text, since the parser itself recurses.
fn parse_graph(turtle: &str, who: &str, arg: &str) -> Result<Graph> {
    check_turtle_nesting(turtle, arg)?;
    let mut graph = Graph::default();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let quad = quad.map_err(|e| Error::Endpoint(format!("{who}: RDF parse error: {e}")))?;
        graph.insert(&quad.into());
    }
    Ok(graph)
}

fn serialize_graph(graph: &Graph) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle).for_writer(&mut out);
    for triple in graph.iter() {
        serializer
            .serialize_triple(triple)
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
async fn with_graph(inv: &Invocation<'_>, who: &str) -> Result<Graph> {
    let with = inv.inline_str("with").map_err(|_| {
        Error::Endpoint(format!("{who}: needs with= (a graph IRI or inline Turtle)"))
    })?;
    if with.trim_start().starts_with('@') || with.trim_start().starts_with('<') {
        return parse_graph(with, who, "with");
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
    parse_graph(&text, who, "with")
}

/// `urn:rdf:union` — the piped graph ∪ the `with=` graph, as Turtle. Set
/// semantics: identical triples merge; skolemized IRIs make cross-source
/// identity literal (the org∪calendar merged view).
struct UnionEndpoint;

#[async_trait::async_trait]
impl Endpoint for UnionEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let content = inv
            .inline_str("content")
            .map_err(|_| Error::Endpoint("urn:rdf:union: pipe a Turtle graph in".to_string()))?;
        let mut graph = parse_graph(content, "urn:rdf:union", "content")?;
        for triple in with_graph(inv, "urn:rdf:union").await?.iter() {
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
        let content = inv
            .inline_str("content")
            .map_err(|_| Error::Endpoint("urn:rdf:diff: pipe a Turtle graph in".to_string()))?;
        let mode = inv.inline_str("mode").unwrap_or("added");
        let ours = parse_graph(content, "urn:rdf:diff", "content")?;
        let theirs = with_graph(inv, "urn:rdf:diff").await?;
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
        .bind(
            Exact::new("urn:rdf:transrept"),
            FnEndpoint::new("rdf-transrept", |inv: &Invocation<'_>| transrept(inv))
                .with_description(
                    Description::new("rdf-transrept")
                        .title("RDF transreption")
                        .summary(
                            "Re-serialize an RDF graph into another syntax — client-side content \
                     negotiation. Pipe a resource in and choose `as`.",
                        )
                        .verb(Verb::Source)
                        .verb(Verb::Meta)
                        .input(
                            ArgSpec::new("content")
                                .summary(
                                    "the RDF document to transrept — usually piped in (e.g. from \
                                 urn:httpGet)",
                                )
                                .class(XSD_STRING),
                        )
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
                        ),
                ),
        )
        .named(space_iri("rdf"))
}

/// Resolve a transreption request: read the RDF from `content` (piped or named), the
/// target syntax from `as` (default Turtle), and return the re-serialized graph.
fn transrept(inv: &Invocation<'_>) -> Result<Representation> {
    let input = inv.inline_str("content").map_err(|_| {
        Error::Endpoint(
            "urn:rdf:transrept needs RDF input — pipe a resource into it (e.g. \
             `source urn:httpGet url=… | urn:rdf:transrept as=text/turtle`) or pass `content=…`"
                .to_string(),
        )
    })?;
    let as_type = inv.inline_str("as").unwrap_or("text/turtle");
    let (media, bytes) = transrept_bytes(input.as_bytes(), as_type)?;
    // Transreption is a pure function of its input bytes, so its output is *as
    // cacheable as its input*. Mark it cacheable here; the kernel folds in the
    // expiry of whatever was piped in (`source <X> | urn:rdf:transrept …`), so a
    // stable source (e.g. urn:kernel:catalog) yields a cacheable result while a live
    // fetch (no Cache-Control) yields an uncacheable one — cacheability flows down
    // the pipe rather than being asserted unconditionally here.
    Ok(
        Representation::new(ReprType::new(&media).with_param("charset", "utf-8"), bytes)
            .cacheable(),
    )
}

/// The pure transformation, factored out so it's testable without an [`Invocation`]:
/// parse `input` (input syntax sniffed) and serialize it as `as_type`. Returns the
/// canonical media type and the serialized bytes.
fn transrept_bytes(input: &[u8], as_type: &str) -> Result<(String, Vec<u8>)> {
    let from = sniff(input);
    // Bound triple-term depth before any parser, serializer or `Display` recurses over it
    // (ledger #992), with the scan that matches the parser `from` selects. JSON-LD carries no
    // triple terms (see `depth`); every other syntax here is read by oxttl.
    match from {
        RdfFormat::RdfXml => check_rdfxml_nesting(input, "content")?,
        RdfFormat::JsonLd { .. } => {}
        _ => check_turtle_nesting(&String::from_utf8_lossy(input), "content")?,
    }

    // The human view: a subject/predicate/object table over the parsed triples.
    if media_base(as_type) == "text/html" {
        let html = to_html(RdfParser::from_format(from).for_slice(input))?;
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
    for quad in RdfParser::from_format(from).for_slice(input) {
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
    use ikigai_core::{Capability, Kernel, Request};
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
        parse_graph(&String::from_utf8(out.bytes).unwrap(), "test", "content").unwrap()
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
        let (_, bytes) = transrept_bytes(input.as_bytes(), as_type).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn subclass_axioms_extracts_rdfs_subclassof_pairs() {
        let ttl = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix schema: <https://schema.org/> .
foaf:Person rdfs:subClassOf schema:Person .
schema:Person rdfs:subClassOf schema:Thing .
foaf:Person foaf:name "ignored" ."#;
        let mut pairs = subclass_axioms(ttl);
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "http://xmlns.com/foaf/0.1/Person".to_string(),
                    "https://schema.org/Person".to_string()
                ),
                (
                    "https://schema.org/Person".to_string(),
                    "https://schema.org/Thing".to_string()
                ),
            ],
            "only the two rdfs:subClassOf triples, as IRI pairs"
        );
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
        let err = transrept_bytes(TTL.as_bytes(), "application/x-nonsense").unwrap_err();
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
}
