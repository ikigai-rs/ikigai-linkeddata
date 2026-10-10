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
//! | `@prefix` / `@base` / `PREFIX` / `BASE` / a comment / an `<scheme:…>` IRI subject | `text/turtle` |
//! | other valid UTF-8 text | `text/plain` |
//! | anything else | `application/octet-stream` |
//!
//! The one subtle row is the last `<`: Turtle's `<scheme:…>` subject and XML's `<element`
//! tag open with the same byte, and they are told apart by `angle_token_is_iri` — an IRI
//! reference closes with no whitespace, quote or `<` in it and leads with a URI scheme,
//! where an element tag either carries attributes or is a bare XML QName like `<rdf:RDF>`.
//! *Not* by looking for `://`, which asks about an authority: `urn:` has none.
//!
//! Detectors are pluggable ([`Detector`]) and run in priority order ([`detectors`]), so
//! binary detectors (PDF/PNG/gzip magic bytes, …) slot in later without disturbing this set.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FnEndpoint,
    Invocation, Iri, ReprType, Representation, Request, Result, Verb,
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

/// The XSD `string` datatype IRI — the `class` of the by-value inputs here. `content` is
/// opaque bytes and may well be binary; `xsd:string` is what the wire carries (a piped
/// value, an MCP argument) and `ArgSpec` has no class for "bytes of unknown type", which
/// is the very question `urn:sniff` answers.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

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
        // Turtle claims an `<scheme:…>` IRI subject, Markup claims an `<element` tag.
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
/// header, a `#` comment, or an `<scheme:…>` IRI subject (an N-Triples/Turtle triple).
/// All map to `text/turtle` (a superset of N-Triples).
struct TurtleDetector;
impl Detector for TurtleDetector {
    fn detect(&self, bytes: &[u8]) -> Option<&'static str> {
        let rest = lead(bytes);
        match rest.first()? {
            b'@' | b'#' => Some(TURTLE), // @prefix / @base / a comment
            b'<' if angle_token_is_iri(rest) => Some(TURTLE), // <scheme:…> subject
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

/// For a leading `<…>` token, whether it is a Turtle/N-Triples IRI subject rather than an
/// XML start tag. Both syntaxes open with `<`, so this is the whole of the distinction.
///
/// The test used to be "does the token contain `://`?", which asks about an *authority*,
/// not a scheme — so every `urn:` IRI (the scheme ikigai names all of its resources with)
/// read as an element tag and its document as RDF/XML. Three cheap signals replace it,
/// all on the bracketed token alone:
///
/// 1. it closes within the scan window, with no character Turtle's `IRIREF` production
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
/// What stays ambiguous is the residue: a single-colon, all-name-character IRI such as
/// `<urn:x>` reads as a tag. Nothing in the token separates the two shapes, and ikigai's
/// own URNs are `urn:nid:nss`, so this is documented rather than guessed at.
fn angle_token_is_iri(rest: &[u8]) -> bool {
    let window = &rest[1..rest.len().min(SCAN)];
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
/// the overlap this has to arbitrate, and `is_iri_char` has already admitted it as an IRI.
fn is_xml_qname(token: &[u8]) -> bool {
    token.iter().filter(|&&b| b == b':').count() == 1
        && token
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
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

/// The name [`space`] claims: `urn:iki:space:sniff`.
pub const SPACE_ID: &str = "urn:iki:space:sniff";

/// The space binding `urn:sniff` (classify opaque bytes) and `urn:transrept:auto` (sniff,
/// then dispatch to the matching transreptor chain). Mount it in any kernel. It is
/// configuration-free, so it names itself [`SPACE_ID`]; a host that binds more doors onto it
/// drops that name (core 0.1.89) and names its own composition.
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
                    .input(
                        ArgSpec::new("content")
                            .summary(
                                "the bytes to classify — usually piped in (e.g. from urn:httpGet \
                                 or a file)",
                            )
                            .class(XSD_STRING),
                    )
                    .output("text/plain;charset=utf-8"),
            ),
        )
        .bind(Exact::new("urn:transrept:auto"), AutoTransrept)
        .named(space_iri("sniff"))
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
            .input(
                ArgSpec::new("content")
                    .summary(
                        "the bytes to transrept — usually piped in (e.g. from urn:httpGet or a \
                         file)",
                    )
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("as")
                    .summary("target media type (default text/turtle)")
                    .class(XSD_STRING)
                    .default_value(TURTLE),
            )
            // The ONE face this endpoint chooses by itself: with no `as=`, `invoke`
            // targets `TURTLE`, so `text/turtle` is a declaration that is true rather
            // than one today's fixture happened to produce.
            //
            // What it deliberately does NOT announce is the rest of the set. Called
            // with `as=X` this serves X, for any X some *bound* transreptor chain
            // reaches — a set that belongs to the host's bindings, not to this
            // endpoint, and that therefore changes under it. `outputs` is a closed
            // list with no spelling for "follows `as=`" (core PENDING §20,
            // ikigai-conformance #48), and enumerating a guess would make the
            // manifold over-offer, which is the worse of the two errors. So the
            // default is declared, the open remainder is pinned by hand in
            // `as_selects_the_served_type` below, and this comment is the record
            // until core can say "any".
            .output(TURTLE)
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
        // Both open with `<`. An IRI reference closes with no whitespace or quote in it
        // and leads with a URI scheme; an element tag carries attributes or is a QName.
        assert_eq!(sniff(b"<http://ex/a> <http://ex/b> 1 ."), TURTLE);
        assert_eq!(sniff(b"<doc><item>x</item></doc>"), XML);
    }

    #[test]
    fn a_urn_subject_is_turtle_not_rdfxml() {
        // The regression this test exists for: `urn:` has no authority, so the old `://`
        // discriminator read every ikigai resource IRI as an XML tag and handed a Turtle
        // document to the RDF/XML parser ("Unknown prefix urn:"). One colon after the
        // scheme is enough — `urn:demo:a` cannot be an XML QName, which admits only one.
        assert_eq!(sniff(b"<urn:demo:a> <urn:demo:b> <urn:demo:c> .\n"), TURTLE);
        assert_eq!(
            sniff(b"<urn:demo:a> <urn:demo:b> \"lit\" .\n<urn:demo:c> a <urn:demo:D> .\n"),
            TURTLE
        );
        // Other authority-less schemes ride along with it.
        assert_eq!(sniff(b"<mailto:ada@ex.org> <urn:demo:p> 1 ."), TURTLE);
        assert_eq!(sniff(b"<tag:ex.org,2026:a> <urn:demo:p> 1 ."), TURTLE);
        assert_eq!(sniff(b"<did:web:ex.org> <urn:demo:p> 1 ."), TURTLE);
    }

    #[test]
    fn an_rdfxml_root_is_markup_with_or_without_attributes() {
        // With attributes: whitespace inside the token settles it.
        assert_eq!(
            sniff(
                br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"></rdf:RDF>"#
            ),
            RDFXML
        );
        // Bare: no whitespace, and `rdf` is a valid scheme — so the QName test is what
        // keeps it markup. (An undeclared prefix makes this invalid RDF/XML, but the
        // sniff must not read it as Turtle and mis-name the parse error.)
        assert_eq!(sniff(b"<rdf:RDF></rdf:RDF>"), RDFXML);
        assert_eq!(sniff(b"<soap:Envelope></soap:Envelope>"), XML);
        // A declaration or a doctype is never an IRI: neither opens with a scheme.
        assert_eq!(
            sniff(br#"<?xml version="1.0"?><note><to>x</to></note>"#),
            XML
        );
        assert_eq!(sniff(b"<!DOCTYPE note SYSTEM \"note.dtd\"><note/>"), XML);
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
    fn auto_dispatches_a_graph_whose_subjects_are_all_urns() {
        // The end of the reported failure: `source urn:<a graph> | urn:transrept:auto`.
        // Every subject is a `urn:`, so the old discriminator sniffed RDF/XML and the
        // chain selected for it (or, in the real kernel, the RDF/XML parser) never saw
        // Turtle. Both arms of the dispatcher have to survive it.
        let ttl = b"<urn:demo:a> <urn:demo:b> <urn:demo:c> .\n";
        let rep = run_auto(ttl, "text/html").unwrap();
        assert_eq!(rep.repr_type.media_type, "text/html");
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.starts_with("<table>"), "turtle→html ran: {body}");
        assert!(
            body.contains("urn:demo:a"),
            "over the sniffed turtle: {body}"
        );
        // Pass-through arm: sniffed type == requested type, no chain needed.
        let rep = run_auto(ttl, "text/turtle").unwrap();
        assert_eq!(rep.repr_type.media_type, "text/turtle");
        assert_eq!(rep.bytes, ttl);
    }

    /// Resolve `urn:transrept:auto` with **no** `as` argument at all — the call the
    /// conformance walk makes, and the only one that observes the endpoint's own choice.
    fn run_auto_default(content: &[u8]) -> Result<Representation> {
        let request = Request::new(Verb::Source, Iri::parse("urn:transrept:auto").unwrap())
            .with_arg("content", ArgRef::Inline(content.to_vec()));
        futures::executor::block_on(auto_kernel().issue(request, &Capability::root()))
    }

    #[test]
    fn auto_serves_text_turtle_when_no_as_is_given() {
        // The declared output (`.output(TURTLE)`) is exactly this: absent an `as=`,
        // `invoke` targets text/turtle, so that face is the endpoint's own and the
        // manifold announces it. Changing the default without changing the
        // declaration breaks here and in the conformance walk together.
        assert_eq!(
            run_auto_default(b"<urn:demo:a> <urn:demo:b> <urn:demo:c> .\n")
                .unwrap()
                .repr_type
                .media_type,
            TURTLE
        );
        // Reached by dispatch as well as by pass-through: the chain is selected *to*
        // the default, so the default holds whichever arm ran.
        assert_eq!(
            run_auto_default(b"@prefix ex: <http://e/> . ex:a ex:b ex:c .")
                .unwrap()
                .repr_type
                .media_type,
            TURTLE
        );
    }

    #[test]
    fn as_selects_the_served_type() {
        // What the single declared output gives up, pinned by hand because `outputs`
        // cannot spell it (core PENDING §20): with an `as=`, the served type FOLLOWS
        // the caller over a set the host's bindings decide, not this endpoint.
        let ttl = b"@prefix ex: <http://e/> . ex:a ex:b ex:c .";
        for target in [TURTLE, "text/html"] {
            assert_eq!(
                run_auto(ttl, target).unwrap().repr_type.media_type,
                target,
                "as={target} is the served type"
            );
        }
        // And an `as=` nothing reaches is refused, not quietly served as the declared
        // default — the declaration is a face, never a fallback.
        let err = format!("{}", run_auto(ttl, "application/pdf").unwrap_err());
        assert!(err.contains("no transreptor"), "{err}");
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
