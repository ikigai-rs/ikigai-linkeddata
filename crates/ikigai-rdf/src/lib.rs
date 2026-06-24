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

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, ReprType,
    Representation, Result, Verb,
};
use oxrdf::Quad;
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};

/// The space binding `urn:rdf:transrept`. Mount it in any kernel (the CLI's embedded
/// space, the in-browser kernel) to give it RDF content negotiation.
pub fn space() -> EndpointSpace {
    EndpointSpace::new().bind(
        Exact::new("urn:rdf:transrept"),
        FnEndpoint::new("rdf-transrept", |inv: &Invocation<'_>| transrept(inv)).with_description(
            Description::new("rdf-transrept")
                .title("RDF transreption")
                .summary(
                    "Re-serialize an RDF graph into another syntax — client-side content \
                     negotiation. Pipe a resource in and choose `as`.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(ArgSpec::new("content").summary(
                    "the RDF document to transrept — usually piped in (e.g. from urn:httpGet)",
                ))
                .input(ArgSpec::new("as").summary(
                    "target representation: text/turtle (default), application/n-triples, \
                     application/n-quads, application/trig, application/rdf+xml, \
                     application/ld+json, or text/html",
                ))
                .output("text/turtle;charset=utf-8"),
        ),
    )
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
    Ok(Representation::new(
        ReprType::new(&media).with_param("charset", "utf-8"),
        bytes,
    )
    .cacheable())
}

/// The pure transformation, factored out so it's testable without an [`Invocation`]:
/// parse `input` (input syntax sniffed) and serialize it as `as_type`. Returns the
/// canonical media type and the serialized bytes.
fn transrept_bytes(input: &[u8], as_type: &str) -> Result<(String, Vec<u8>)> {
    let from = sniff(input);

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
/// that is an IRI (`<scheme://…>`) ⇒ Turtle/N-Triples (a triple's subject), whereas a
/// leading `<` opening an XML element (`<?xml`, `<rdf:RDF`, `<Foo`) ⇒ RDF/XML; anything
/// else (`@prefix`, a prefixed name, a comment) ⇒ Turtle, which subsumes N-Triples.
/// The IRI-vs-element test is the `://` scheme separator — both syntaxes open with `<`.
fn sniff(bytes: &[u8]) -> RdfFormat {
    let rest = &bytes[bytes.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
    match rest.first() {
        Some(b'{') | Some(b'[') => json_ld(),
        Some(b'<') => {
            // The first `<…>` token: an IRI carries a `://` scheme separator; an XML
            // element tag does not.
            let token = rest
                .iter()
                .take_while(|&&b| b != b'>' && !b.is_ascii_whitespace())
                .copied()
                .collect::<Vec<u8>>();
            if token.windows(3).any(|w| w == b"://") {
                RdfFormat::Turtle
            } else {
                RdfFormat::RdfXml
            }
        }
        _ => RdfFormat::Turtle,
    }
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

    const TTL: &str = r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
<http://example.org/me> foaf:name "Ada" ; foaf:knows <http://example.org/you> ."#;

    fn body(input: &str, as_type: &str) -> String {
        let (_, bytes) = transrept_bytes(input.as_bytes(), as_type).unwrap();
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
}
