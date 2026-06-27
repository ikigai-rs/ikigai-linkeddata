//! `ikigai-sniff` — content-type sniffing **and dispatch** as ikigai resources.
//!
//! `urn:sniff` answers the question *"what is this blob?"* — given opaque bytes (the
//! `application/octet-stream` an HTTP fetch with a missing/wrong `Content-Type`, a file
//! read, a pasted payload, or a content-addressed blob delivers), it returns a **concrete
//! media type**. That's the first half of *octet-stream sniff-and-dispatch*.
//!
//! `urn:transrept:auto` is the second half: it sniffs the piped bytes, then asks the kernel
//! for a transreptor chain from the *detected* type to the requested `as=` ([`Invocation::
//! select_transreptor`](ikigai_core::Invocation::select_transreptor)) and runs it — so a
//! caller can transrept opaque bytes **without knowing their input type**. (When the bytes
//! already are the requested type it's a pass-through; when nothing converts them it's a
//! clean error.)
//!
//! Detection is **heuristic only** — it inspects the opening bytes, it does not parse — so
//! it is cheap and total. v1 covers the linked-data / text family:
//!
//! | bytes start with… | → media type |
//! |---|---|
//! | a binary magic signature (`%PDF-`, PNG, JPEG, GIF, gzip) | `application/pdf` / `image/*` / `application/gzip` |
//! | `{` / `[` with a JSON-LD keyword (`@context`/`@id`/`@graph`/`@type`) | `application/ld+json` |
//! | `{` / `[` otherwise | `application/json` |
//! | `<!doctype html` / `<html` | `text/html` |
//! | `<rdf:RDF` or the RDF-syntax namespace | `application/rdf+xml` |
//! | another XML element (`<?xml`, `<Foo`) | `application/xml` |
//! | `@prefix` / `@base` / `PREFIX` / `BASE` / a comment / an `<scheme://…>` subject | `text/turtle` |
//! | other valid UTF-8 text | `text/plain` |
//! | anything else | `application/octet-stream` |
//!
//! Detectors are pluggable ([`Detector`]) and run in priority order ([`detectors`]), so
//! binary detectors (PDF/PNG/gzip magic bytes, …) slot in later without disturbing this set.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FnEndpoint, Invocation,
    Iri, ReprType, Representation, Request, Result, Verb,
};

// The media types v1 detects. `text/turtle` stands in for the whole RDF text family
// (it is a superset of N-Triples), and `urn:rdf:transrept` re-sniffs the exact syntax
// internally anyway, so this is precise enough to drive selection.
const TURTLE: &str = "text/turtle";
const RDFXML: &str = "application/rdf+xml";
const XML: &str = "application/xml";
const JSONLD: &str = "application/ld+json";
const JSON: &str = "application/json";
const HTML: &str = "text/html";
const PLAIN: &str = "text/plain";
const OCTET: &str = "application/octet-stream";
// Binary families, recognized by leading magic bytes.
const PDF: &str = "application/pdf";
const PNG: &str = "image/png";
const JPEG: &str = "image/jpeg";
const GIF: &str = "image/gif";
const GZIP: &str = "application/gzip";

/// A single content-type heuristic. Returns the detected media type if the bytes look like
/// its family, else `None` so the next detector gets a turn. Implementations must not parse
/// or allocate large buffers — only inspect a bounded prefix.
pub trait Detector: Send + Sync {
    /// The media type these bytes look like, or `None` if this detector doesn't recognize them.
    fn detect(&self, bytes: &[u8]) -> Option<&'static str>;
    /// A short label for the detector (diagnostics / ordering docs).
    fn label(&self) -> &'static str;
}

/// The default detector registry, in priority order. The first detector to return `Some`
/// wins; if none match, [`sniff`] falls back to `text/plain` (valid UTF-8) or
/// `application/octet-stream`.
pub fn detectors() -> Vec<Box<dyn Detector>> {
    vec![
        // Magic bytes first: an unambiguous binary signature beats any text heuristic, and a
        // classified binary type makes the dispatch path (urn:transrept:auto) refuse it
        // cleanly ("no transreptor from image/png to …") rather than mis-reading it as text.
        Box::new(MagicDetector),
        Box::new(JsonDetector),
        // Turtle before Markup: both inspect a leading `<`, but they are mutually exclusive —
        // Turtle claims an `<scheme://…>` IRI subject, Markup claims an `<element` tag.
        Box::new(TurtleDetector),
        Box::new(MarkupDetector),
    ]
}

/// Binary families, recognized by leading magic bytes (checked at the very start — no
/// whitespace skipping, since a binary signature may itself begin with a whitespace byte).
struct MagicDetector;
impl Detector for MagicDetector {
    fn detect(&self, bytes: &[u8]) -> Option<&'static str> {
        let sigs: &[(&[u8], &str)] = &[
            (b"%PDF-", PDF),
            (&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], PNG),
            (&[0xFF, 0xD8, 0xFF], JPEG),
            (b"GIF87a", GIF),
            (b"GIF89a", GIF),
            (&[0x1F, 0x8B], GZIP),
        ];
        sigs.iter()
            .find(|(sig, _)| bytes.starts_with(sig))
            .map(|&(_, media)| media)
    }
    fn label(&self) -> &'static str {
        "magic"
    }
}

/// Detect the media type of `bytes`. Runs [`detectors`] in order; falls back to `text/plain`
/// for other valid UTF-8 and `application/octet-stream` for binary. Never fails.
pub fn sniff(bytes: &[u8]) -> &'static str {
    for detector in detectors() {
        if let Some(media) = detector.detect(bytes) {
            return media;
        }
    }
    if is_text(bytes) {
        PLAIN
    } else {
        OCTET
    }
}

/// JSON family: a leading `{` or `[`. JSON-LD is distinguished from plain JSON by a
/// JSON-LD keyword (`"@context"`, `"@id"`, `"@graph"`, `"@type"`) in the opening window.
struct JsonDetector;
impl Detector for JsonDetector {
    fn detect(&self, bytes: &[u8]) -> Option<&'static str> {
        let rest = lead(bytes);
        match rest.first()? {
            b'{' | b'[' => {
                let window = &rest[..rest.len().min(SCAN)];
                let has_keyword = [
                    &b"\"@context\""[..],
                    b"\"@id\"",
                    b"\"@graph\"",
                    b"\"@type\"",
                ]
                .iter()
                .any(|kw| contains(window, kw));
                Some(if has_keyword { JSONLD } else { JSON })
            }
            _ => None,
        }
    }
    fn label(&self) -> &'static str {
        "json"
    }
}

/// Turtle / N-Triples family: a leading `@prefix`/`@base`, a SPARQL-style `PREFIX`/`BASE`
/// header, a `#` comment, or an `<scheme://…>` IRI subject (an N-Triples/Turtle triple).
/// All map to `text/turtle` (a superset of N-Triples).
struct TurtleDetector;
impl Detector for TurtleDetector {
    fn detect(&self, bytes: &[u8]) -> Option<&'static str> {
        let rest = lead(bytes);
        match rest.first()? {
            b'@' | b'#' => Some(TURTLE), // @prefix / @base / a comment
            b'<' if angle_token_is_iri(rest) => Some(TURTLE), // <scheme://…> subject
            _ if starts_with_ci(rest, b"prefix ") || starts_with_ci(rest, b"base ") => Some(TURTLE),
            _ => None,
        }
    }
    fn label(&self) -> &'static str {
        "turtle"
    }
}

/// XML markup family: a leading `<` opening an element (not an IRI). Resolves to `text/html`
/// for an HTML document, `application/rdf+xml` when the RDF-syntax namespace or `<rdf:RDF>`
/// is present, else `application/xml`.
struct MarkupDetector;
impl Detector for MarkupDetector {
    fn detect(&self, bytes: &[u8]) -> Option<&'static str> {
        let rest = lead(bytes);
        if rest.first()? != &b'<' || angle_token_is_iri(rest) {
            return None;
        }
        let window = lower(&rest[..rest.len().min(SCAN)]);
        if window.starts_with(b"<!doctype html") || contains(&window, b"<html") {
            Some(HTML)
        } else if contains(&window, b"<rdf:rdf") || contains(&window, b"22-rdf-syntax-ns#") {
            Some(RDFXML)
        } else {
            Some(XML)
        }
    }
    fn label(&self) -> &'static str {
        "markup"
    }
}

/// How many opening bytes a detector scans for namespace / keyword markers.
const SCAN: usize = 2048;

/// Strip a leading UTF-8 BOM and ASCII whitespace.
fn lead(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let n = bytes.iter().take_while(|b| b.is_ascii_whitespace()).count();
    &bytes[n..]
}

/// For a leading `<…>` token, whether it is an IRI (carries a `://` scheme separator) rather
/// than an XML element tag. Both syntaxes open with `<`; the `://` is the discriminator
/// (same test `urn:rdf:transrept` uses internally).
fn angle_token_is_iri(rest: &[u8]) -> bool {
    let token: Vec<u8> = rest
        .iter()
        .skip(1)
        .take_while(|&&b| b != b'>' && !b.is_ascii_whitespace())
        .copied()
        .collect();
    token.windows(3).any(|w| w == b"://")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn starts_with_ci(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn lower(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_lowercase).collect()
}

/// Whether the bytes are plausibly text (valid UTF-8 with no NUL) rather than binary.
fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// The space binding `urn:sniff` (classify opaque bytes) and `urn:transrept:auto` (sniff,
/// then dispatch to the matching transreptor chain). Mount it in any kernel.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new("urn:sniff"),
            FnEndpoint::new("sniff", |inv: &Invocation<'_>| sniff_endpoint(inv)).with_description(
                Description::new("sniff")
                    .title("Content-type sniff")
                    .summary(
                        "Detect the concrete media type of opaque bytes (the first step of \
                         octet-stream sniff-and-dispatch). Pipe a resource in; returns the \
                         detected media type as text/plain.",
                    )
                    .verb(Verb::Source)
                    .verb(Verb::Meta)
                    .input(ArgSpec::new("content").summary(
                        "the bytes to classify — usually piped in (e.g. from urn:httpGet or a file)",
                    ))
                    .output("text/plain;charset=utf-8"),
            ),
        )
        .bind(Exact::new("urn:transrept:auto"), AutoTransrept)
}

/// `urn:transrept:auto` — sniff the piped bytes, then transrept them to `as=` by selecting
/// and running the matching transreptor chain. The convenience that needs no input-type
/// knowledge: dereference an opaque resource straight into the representation you want.
struct AutoTransrept;

#[async_trait]
impl Endpoint for AutoTransrept {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let bytes = inv.inline_arg("content").map_err(|_| {
            Error::Endpoint(
                "urn:transrept:auto needs bytes — pipe a resource into it (e.g. \
                 `source urn:httpGet url=… | urn:transrept:auto as=text/html`) or pass `content=…`"
                    .to_string(),
            )
        })?;
        let from = sniff(bytes);
        let to = inv.inline_str("as").unwrap_or(TURTLE);

        // Already the requested type → pass through (re-typed from octet-stream to what it is).
        if from == to {
            return Ok(Representation::new(ReprType::new(from), bytes.to_vec()).cacheable());
        }

        let plan = inv.select_transreptor(from, to).ok_or_else(|| {
            Error::Endpoint(format!(
                "urn:transrept:auto: no transreptor from sniffed `{from}` to `{to}`"
            ))
        })?;

        // Run the chain: each step takes the running bytes as `content` and its target as `as`.
        let mut current = Representation::new(ReprType::new(from), bytes.to_vec());
        for step in plan {
            let iri = Iri::parse(&step.endpoint).map_err(|e| {
                Error::Endpoint(format!("bad transreptor IRI `{}`: {e}", step.endpoint))
            })?;
            let request = Request::new(Verb::Source, iri)
                .with_arg("content", ArgRef::Inline(current.bytes))
                .with_arg("as", ArgRef::Inline(step.to.into_bytes()));
            current = inv.issue(request).await?;
        }
        Ok(current.cacheable())
    }

    fn name(&self) -> &str {
        "transrept-auto"
    }

    fn describe(&self) -> Description {
        Description::new("transrept-auto")
            .title("Auto-transrept")
            .summary(
                "Sniff opaque bytes, then transrept them to `as` by selecting the matching \
                 transreptor chain — content negotiation without knowing the input type.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(ArgSpec::new("content").summary(
                "the bytes to transrept — usually piped in (e.g. from urn:httpGet or a file)",
            ))
            .input(ArgSpec::new("as").summary("target media type (default text/turtle)"))
    }
}

/// Resolve a sniff request: read the bytes from `content` and return the detected media type.
fn sniff_endpoint(inv: &Invocation<'_>) -> Result<Representation> {
    let bytes = inv.inline_arg("content").map_err(|_| {
        Error::Endpoint(
            "urn:sniff needs bytes — pipe a resource into it (e.g. \
             `source urn:httpGet url=… | urn:sniff`) or pass `content=…`"
                .to_string(),
        )
    })?;
    let media = sniff(bytes);
    Ok(Representation::new(
        ReprType::new(PLAIN).with_param("charset", "utf-8"),
        media.as_bytes().to_vec(),
    )
    .cacheable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_linked_data_text_family() {
        assert_eq!(sniff(b"@prefix ex: <http://e/> . ex:a ex:b ex:c ."), TURTLE);
        assert_eq!(sniff(b"PREFIX ex: <http://e/>\nSELECT *"), TURTLE); // SPARQL-style turtle header
        assert_eq!(sniff(b"# a comment\n@prefix ex: <http://e/> ."), TURTLE);
        assert_eq!(
            sniff(b"<http://ex/a> <http://ex/b> <http://ex/c> ."),
            TURTLE
        ); // N-Triples → turtle superset
    }

    #[test]
    fn distinguishes_iri_subject_from_xml_element() {
        // Both open with `<`; the `://` scheme separator is the discriminator.
        assert_eq!(sniff(b"<http://ex/a> <http://ex/b> 1 ."), TURTLE);
        assert_eq!(sniff(b"<doc><item>x</item></doc>"), XML);
    }

    #[test]
    fn detects_rdfxml_html_and_plain_xml() {
        assert_eq!(
            sniff(br#"<?xml version="1.0"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"></rdf:RDF>"#),
            RDFXML
        );
        // RDF/XML recognized by the namespace even without the rdf:RDF root prefix.
        assert_eq!(
            sniff(br#"<RDF xmlns="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#),
            RDFXML
        );
        assert_eq!(sniff(b"<!DOCTYPE html><html><body>hi</body></html>"), HTML);
        assert_eq!(sniff(b"<html lang=\"en\"></html>"), HTML);
        assert_eq!(sniff(b"<note><to>x</to></note>"), XML);
    }

    #[test]
    fn distinguishes_jsonld_from_plain_json() {
        assert_eq!(
            sniff(br#"{"@context":"http://schema.org","@id":"x"}"#),
            JSONLD
        );
        assert_eq!(sniff(br#"[{"@id":"x"}]"#), JSONLD);
        assert_eq!(sniff(br#"{"name":"Ada","age":36}"#), JSON);
    }

    #[test]
    fn falls_back_to_text_then_octet_stream() {
        assert_eq!(sniff(b"just some prose, nothing structured"), PLAIN);
        assert_eq!(sniff(&[0x01, 0x02, 0x00, 0x03]), OCTET); // unknown binary (a NUL)
        assert_eq!(sniff(b""), PLAIN); // empty is trivially valid UTF-8
    }

    #[test]
    fn detects_binary_families_by_magic_bytes() {
        assert_eq!(sniff(b"%PDF-1.7\n..."), PDF);
        assert_eq!(
            sniff(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
            PNG
        );
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), JPEG);
        assert_eq!(sniff(b"GIF89a..."), GIF);
        assert_eq!(sniff(&[0x1F, 0x8B, 0x08]), GZIP);
    }

    #[test]
    fn ignores_a_bom_and_leading_whitespace() {
        assert_eq!(sniff(b"\xEF\xBB\xBF   @prefix ex: <http://e/> ."), TURTLE);
        assert_eq!(sniff(b"\n\n  {\"@id\":\"x\"}"), JSONLD);
    }

    #[test]
    fn the_endpoint_reports_the_media_type() {
        use ikigai_core::{Iri, Request, Resolution, Scope, Space};
        let request = Request::new(Verb::Meta, Iri::parse("urn:sniff").unwrap());
        let Resolution::Hit(resolved) = space().resolve(&request, &Scope::empty()) else {
            panic!("urn:sniff resolves");
        };
        let description = resolved.endpoint.describe();
        assert_eq!(description.title, "Content-type sniff");
        assert!(
            description.transreption().is_none(),
            "sniff classifies, it doesn't convert"
        );
    }

    // --- urn:transrept:auto (sniff → dispatch), end-to-end through a kernel ---

    use ikigai_core::{Capability, Kernel};
    use std::sync::Arc;

    /// A stub auto-invocable transreptor turtle → text/html that wraps its `content`, so a
    /// test can see it ran and that the right bytes flowed through.
    fn stub_turtle_to_html() -> FnEndpoint {
        FnEndpoint::new("ttl2html", |inv: &Invocation<'_>| {
            let content = inv.inline_str("content").unwrap_or("");
            Ok(Representation::new(
                ReprType::new("text/html"),
                format!("<table>{content}</table>").into_bytes(),
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

    fn auto_kernel() -> Kernel {
        let space = space().bind(Exact::new("urn:rdf:transrept"), stub_turtle_to_html());
        Kernel::new(Arc::new(space))
    }

    fn run_auto(content: &[u8], as_type: &str) -> Result<Representation> {
        let request = Request::new(Verb::Source, Iri::parse("urn:transrept:auto").unwrap())
            .with_arg("content", ArgRef::Inline(content.to_vec()))
            .with_arg("as", ArgRef::Inline(as_type.as_bytes().to_vec()));
        futures::executor::block_on(auto_kernel().issue(request, &Capability::root()))
    }

    #[test]
    fn auto_sniffs_opaque_bytes_then_dispatches() {
        // Turtle bytes (as a server might hand them back untyped) → text/html, with no input
        // type asserted: auto sniffs text/turtle, selects the turtle→html transreptor, runs it.
        let rep = run_auto(b"@prefix ex: <http://e/> . ex:a ex:b ex:c .", "text/html").unwrap();
        assert_eq!(rep.repr_type.media_type, "text/html");
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.starts_with("<table>"), "transreptor ran: {body}");
        assert!(body.contains("ex:a"), "over the sniffed turtle: {body}");
    }

    #[test]
    fn auto_passes_through_when_already_the_target_type() {
        let rep = run_auto(b"@prefix ex: <http://e/> . ex:a ex:b ex:c .", "text/turtle").unwrap();
        assert_eq!(rep.repr_type.media_type, "text/turtle");
        assert!(String::from_utf8(rep.bytes).unwrap().contains("ex:a"));
    }

    #[test]
    fn auto_errors_cleanly_when_no_transreptor_reaches_the_target() {
        let err = run_auto(
            b"@prefix ex: <http://e/> . ex:a ex:b ex:c .",
            "application/pdf",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("no transreptor"), "{err}");
    }

    #[test]
    fn auto_refuses_a_classified_binary_blob() {
        // A PNG sniffs to image/png; nothing converts it to turtle, so dispatch refuses it
        // cleanly (naming the sniffed type) rather than feeding binary to a graph parser.
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x01];
        let err = run_auto(&png, "text/turtle").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("no transreptor") && msg.contains("image/png"),
            "{msg}"
        );
    }
}
