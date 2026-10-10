//! Caller RDF is bounded in triple-term DEPTH before it is parsed (ledger #992).
//!
//! With RDF 1.2 on (`oxrdfio/rdf-12`, which ikigai-cli turns on for every crate in its build:
//! rudof enables it, and Cargo unifies features), oxrdf copies a nested triple term
//! recursively, once per level. oxttl's Turtle parser does that copy while it parses, and
//! oxrdfxml's does it for a `rdf:parseType="Triple"` element that also carries `rdf:ID` or
//! `rdf:annotation`; the serializers and `Display` recurse the same way afterwards. So 3000
//! levels of `<<( … )>>` (about 90 KB) overflowed a 2 MiB thread and aborted the whole host
//! through every door of this crate, and 3000 nested `parseType="Triple"` elements did the
//! same through `urn:rdf:transrept` (reproduced in a child process, `tests/triple_term_depth.rs`).
//!
//! Each scan here reads the text the way the parser that will read it does, refuses past
//! [`MAX_TURTLE_NESTING`] as a typed `InvalidArgument` naming the argument that carried the
//! text, and costs one pass with no recursion. JSON-LD needs none: oxjsonld 0.2's `rdf-12`
//! adds only directional language strings, never a triple term (re-read `to_rdf.rs` when
//! raising the oxrdfio floor).

use ikigai_core::{Error, Result};
use quick_xml::events::Event;
use quick_xml::NsReader;

// COPY: `MAX_TURTLE_NESTING` and `check_turtle_nesting` are ikigai-shacl's (`src/depth.rs`,
// ikigai-rs/ikigai-shacl PR 17), which exports only the constant. The fold is ledger #976's
// shared limits crate; until it lands, change both copies together. (Only the refusal's wording
// differs: here it is shared with the RDF/XML scan below.)

/// How deep caller RDF may nest RDF 1.2 triple terms and reified triples (`<<( … )>>`,
/// `<< … >>`), counted per `<<`; for RDF/XML, per nested element carrying `rdf:parseType`. The
/// same number as `ikigai_store::limits::MAX_SPARQL_NESTING` and ikigai-shacl's: one bound on
/// caller nesting across the doors. Real data nests these a handful deep; a debug build
/// survived 300 levels on a 2 MiB thread and aborted at 3000.
pub const MAX_TURTLE_NESTING: usize = 64;

fn too_deep(arg: &str, how: &str) -> Error {
    Error::InvalidArgument {
        name: arg.to_string(),
        detail: format!(
            "this RDF nests RDF 1.2 triple terms ({how}) deeper than {MAX_TURTLE_NESTING} \
             (MAX_TURTLE_NESTING): the RDF library copies a nested triple term recursively, \
             once per level, where a stack overflow aborts the whole host"
        ),
    }
}

/// Refuse caller Turtle that nests `<<` deeper than [`MAX_TURTLE_NESTING`], without parsing it.
///
/// The scan reads the text as oxttl 0.2's lexer does (`lexer.rs`), in the parts that decide
/// what is code: strings in all four quote forms, closing at the FIRST unescaped delimiter (a
/// long string at the first triple, a short one even across a line end), `<…>` IRIs as
/// everything up to the first `>`, `#` comments to the line end, and `\`-escapes. So a `<<`
/// inside a literal, an IRI or a comment is text, not nesting.
///
/// ⚠ That is oxttl's LENIENT reading, which ikigai-shacl needs because rudof parses leniently.
/// Every door in this crate parses STRICT (`RdfParser::from_format`, no `.lenient()`) and stops
/// at the parser's first error, and the copy still holds there: the two modes lex every text
/// the strict parser accepts identically (they differ only in what they reject: a line end in
/// a short string, a malformed IRI, a bad escape), so wherever this reading and the strict
/// lexer part ways, the strict parser has already reported an error, the door has stopped,
/// and nothing after that point is ever built into a term. Over-reading is the safe direction
/// (a `<<` inside an IRI the strict parser would reject is counted); hiding one the parser
/// reads would not be, and the strict lexer never reads past a place this scan treats as text.
pub fn check_turtle_nesting(text: &str, arg: &str) -> Result<()> {
    let b = text.as_bytes();
    let at = |i: usize| b.get(i).copied();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                if at(i + 1) == Some(q) && at(i + 2) == Some(q) {
                    // A long string: ends at the first unescaped triple quote.
                    i += 3;
                    while i < b.len() {
                        if b[i] == b'\\' {
                            i += 2;
                        } else if b[i] == q && at(i + 1) == Some(q) && at(i + 2) == Some(q) {
                            i += 3;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                } else {
                    // A short string: ends at the quote, and NOT at a line end, which Turtle
                    // forbids but oxttl's `lenient()` lexer (lexer.rs:674) reads straight
                    // through. Stopping there would take the real closing quote for an
                    // opening one and hide what follows it. (The strict lexer errors at the
                    // line end instead, and the doors here stop at that error.)
                    i += 1;
                    while i < b.len() {
                        match b[i] {
                            b'\\' => i += 2,
                            c if c == q => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                }
            }
            b'<' if at(i + 1) == Some(b'<') => {
                depth += 1;
                if depth > MAX_TURTLE_NESTING {
                    return Err(too_deep(arg, "`<<( … )>>` or `<< … >>`"));
                }
                i += 2;
            }
            b'<' => {
                // An IRI, read as oxttl reads it: everything up to the first `>`, a `\` escaping
                // what follows. NOT only IRIREF's characters: in lenient mode oxttl validates
                // nothing inside the brackets, so `<x"> , <<( …` is one IRI and then real
                // nesting, which a stricter reading would mistake for a string.
                let mut j = i + 1;
                while j < b.len() && b[j] != b'>' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                if j >= b.len() {
                    // Never closed: the parser fails here, and builds nothing after it.
                    break;
                }
                i = j + 1;
            }
            b'>' if at(i + 1) == Some(b'>') => {
                depth = depth.saturating_sub(1);
                i += 2;
            }
            // `\` outside a string is a prefixed name's escape (`ex:a\#b`): skip what it escapes.
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    Ok(())
}

/// Refuse caller RDF/XML whose `rdf:parseType` elements nest deeper than
/// [`MAX_TURTLE_NESTING`], without parsing it as RDF.
///
/// RDF/XML writes a triple term as a property element with `rdf:parseType="Triple"` around the
/// one triple it holds, so triple terms nest exactly as those elements do. Rather than mirror
/// an XML lexer by hand, the scan RUNS oxrdfxml's: quick-xml's `NsReader` over the same bytes,
/// configured as oxrdfxml 0.2 configures it (`expand_empty_elements`, defaults otherwise), so
/// comments, CDATA, processing instructions, the DOCTYPE and quoted attribute values are
/// whatever the parser takes them to be. It stops where the reader reports an error, as the
/// parser does (and the door with it).
///
/// It counts MORE than the parser builds, which is the safe direction: any element with an
/// attribute whose local name is `parseType`, whatever its prefix or value (a value can be
/// spelled with character or entity references), and an element whose attributes do not parse.
/// Real RDF/XML nests `parseType` elements a few deep; one 65 deep is refused even when it is
/// `Resource`, which never recursed.
pub fn check_rdfxml_nesting(bytes: &[u8], arg: &str) -> Result<()> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    // One entry per open element: whether it carries a `parseType`.
    let mut open: Vec<bool> = Vec::new();
    let mut depth = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let counts = element.attributes().any(|attribute| match attribute {
                    Ok(attribute) => attribute.key.local_name().as_ref() == b"parseType",
                    Err(_) => true,
                });
                if counts {
                    depth += 1;
                    if depth > MAX_TURTLE_NESTING {
                        return Err(too_deep(arg, "nested `rdf:parseType=\"Triple\"` elements"));
                    }
                }
                open.push(counts);
            }
            Ok(Event::End(_)) => {
                if open.pop() == Some(true) {
                    depth -= 1;
                }
            }
            Ok(Event::Eof) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turtle(levels: usize) -> String {
        format!(
            "<urn:ex:s> <urn:ex:p> {}<urn:ex:o>{} .",
            "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
            " )>>".repeat(levels)
        )
    }

    fn rdfxml(levels: usize, parse_type: &str) -> String {
        format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
             xmlns:ex=\"urn:ex:\"><rdf:Description rdf:about=\"urn:ex:s\">{}\
             <ex:p rdf:resource=\"urn:ex:o\"/>{}</rdf:Description></rdf:RDF>",
            format!(
                "<ex:p rdf:parseType=\"{parse_type}\"><rdf:Description rdf:about=\"urn:ex:s\">"
            )
            .repeat(levels),
            "</rdf:Description></ex:p>".repeat(levels)
        )
    }

    #[test]
    fn the_bound_is_the_shared_number() {
        assert_eq!(MAX_TURTLE_NESTING, 64);
    }

    #[test]
    fn turtle_at_the_bound_passes_and_one_past_is_refused_by_name() {
        check_turtle_nesting(&turtle(64), "content").unwrap();
        let err = check_turtle_nesting(&turtle(65), "with").unwrap_err();
        assert!(
            err.to_string().starts_with("invalid argument `with`"),
            "{err}"
        );
    }

    #[test]
    fn sequential_triple_terms_are_not_nesting() {
        let flat = "<urn:ex:s> <urn:ex:p> <<( <urn:ex:a> <urn:ex:b> <urn:ex:c> )>> .\n".repeat(500);
        check_turtle_nesting(&flat, "content").unwrap();
    }

    #[test]
    fn rdfxml_at_the_bound_passes_and_one_past_is_refused_by_name() {
        check_rdfxml_nesting(rdfxml(64, "Triple").as_bytes(), "content").unwrap();
        let err = check_rdfxml_nesting(rdfxml(65, "Triple").as_bytes(), "content").unwrap_err();
        assert!(
            err.to_string().starts_with("invalid argument `content`"),
            "{err}"
        );
    }

    #[test]
    fn rdfxml_counts_a_parse_type_however_it_is_spelled() {
        // `&#84;riple` is `Triple` once the parser unescapes it, so the value is not read.
        let spelled = rdfxml(65, "&#84;riple");
        assert!(check_rdfxml_nesting(spelled.as_bytes(), "content").is_err());
        let other_prefix = rdfxml(65, "Triple")
            .replace("xmlns:rdf=", "xmlns:r=")
            .replace("rdf:", "r:");
        assert!(check_rdfxml_nesting(other_prefix.as_bytes(), "content").is_err());
    }

    #[test]
    fn rdfxml_markup_in_comments_and_cdata_is_not_nesting() {
        let fake = rdfxml(1, "Triple").replace(
            "<ex:p rdf:resource",
            &format!(
                "<!-- {} --><ex:q><![CDATA[{}]]></ex:q><ex:p rdf:resource",
                "<ex:p rdf:parseType=\"Triple\">".repeat(100),
                "<ex:p rdf:parseType=\"Triple\">".repeat(100)
            ),
        );
        check_rdfxml_nesting(fake.as_bytes(), "content").unwrap();
    }

    #[test]
    fn rdfxml_siblings_are_not_nesting() {
        let flat = format!(
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
             xmlns:ex=\"urn:ex:\">{}</rdf:RDF>",
            "<rdf:Description rdf:about=\"urn:ex:s\"><ex:p rdf:parseType=\"Resource\">\
             <ex:q>x</ex:q></ex:p></rdf:Description>"
                .repeat(500)
        );
        check_rdfxml_nesting(flat.as_bytes(), "content").unwrap();
    }
}
