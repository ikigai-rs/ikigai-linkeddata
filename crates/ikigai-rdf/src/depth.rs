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
//! text, and costs one pass with no recursion. oxjsonld 0.2's `rdf-12` adds only directional
//! language strings, never a triple term (re-read `to_rdf.rs` when raising the oxrdfio floor),
//! but JSON-LD has a recursion of its own, below.
//!
//! # JSON-LD: a depth bound AND a sized stack (ledger #1038)
//!
//! oxjsonld's expander buffers each node object's events until it has seen the whole object
//! (an `@context` may come last), then replays them through itself, so it recurses once per
//! nested node object. `{"@id":…,"p":{"@id":…,"p":{…}}}` 1,021 levels deep (about 36 KB)
//! aborted the host through `urn:rdf:transrept` on a 2 MiB thread in a release build, and 29
//! levels (about 1 KB) in a debug one. Measured by `tests/jsonld_depth_measure.rs`, oxjsonld
//! 0.2.6, aarch64-apple-darwin, the largest JSON depth (`{` and `[` alike) that survived the
//! parser alone:
//!
//! | shape | debug, 2 MiB | debug, 8 MiB | debug, 64 MiB | release, 2 MiB | release, 8 MiB |
//! | --- | --- | --- | --- | --- | --- |
//! | node objects as values (one level a level) | 29 | 126 | 1,030 | 1,025 | 4,093 |
//! | blank nodes, `@reverse`, `@graph`, `@list` objects, unmapped keys | 28–59 | | | 1,024–2,049 | |
//! | scoped `@context`s in term definitions (two levels a level) | 158 | 658 | 5,338 | 902 | 3,602 |
//! | arrays only: of values, in `@list`, at the top, in `@context`; a `@json` literal | ≥ 4,097 | | | ≥ 16,385 | |
//!
//! So a node level costs ~64 KiB of stack in a debug build and ~2 KiB in release, linearly in
//! the thread's size; arrays alone never recurse. A bigger stack alone would NOT be the fix:
//! the replay also re-buffers a node's whole content once per level, so memory grows with the
//! square of the depth (8,000 levels, ~290 KB of JSON, took 5.2 GB; 16,000 would take ~20), and
//! on a 64 MiB thread a ~1 MB document runs the host out of memory instead of stack.
//!
//! Hence two layers, as `ikigai-sparql`'s `limits` does for SPARQL:
//!
//! 1. [`check_json_nesting`] refuses JSON nested deeper than [`MAX_JSON_NESTING`] (64), as a
//!    typed `InvalidArgument`, before the parser sees a byte. That bounds the recursion and the
//!    memory both: the re-buffering costs at most 64 times the document.
//! 2. The JSON-LD parse runs on a thread of [`JSON_LD_STACK`] (16 MiB), so a debug build holds
//!    the bound too (it needs ~4.5 MiB at 64), and a debug and a release host admit and answer
//!    the same documents. ⚠ On wasm there is no thread: the parse runs inline, and the bound
//!    alone applies (~130 KiB at 64 in an optimized build).

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

/// How deep caller JSON-LD may nest JSON objects and arrays, counted together, outside strings:
/// **64**, the same number as [`MAX_TURTLE_NESTING`], so one bound on caller nesting holds
/// across the doors.
///
/// Real JSON-LD is shallow: the deepest of the 2,146 documents in the W3C JSON-LD 1.1 API test
/// suite (expand, compact, flatten, toRdf, fromRdf) nests 10, and every JSON-LD file in the
/// ikigai ecosystem (the vocabulary's context among them) 3 (measured 2026-10-10). Arrays count
/// although they never recurse alone, because compacted JSON-LD wraps values in arrays freely,
/// so a node level is one or two JSON levels and the count stays a plain bracket count.
pub const MAX_JSON_NESTING: usize = 64;

/// The stack the JSON-LD parse is given on a thread of its own (native only): 16 MiB.
///
/// A debug build spends ~64 KiB of stack per nested node object and a release one ~2 KiB, so a
/// document at [`MAX_JSON_NESTING`] needs ~4.5 MiB in debug, more than a tokio worker's 2 MiB;
/// 16 MiB holds about 250 levels in debug, four times the bound. Reserved address space, touched
/// only as deep as a document recurses.
pub const JSON_LD_STACK: usize = 16 << 20;

/// Refuse caller JSON (here, JSON-LD) that nests objects and arrays deeper than
/// [`MAX_JSON_NESTING`], without parsing it.
///
/// One pass, no recursion, and it reads strings as JSON does: a string opens at a `"` outside
/// one and closes at the next `"` not escaped by `\`, so a bracket inside a string is text.
/// It counts the whole input, past the point where the parser would stop at an error, which is
/// the safe direction (it can over-count, never hide a level the parser reads).
///
/// The bound alone does not make a debug build safe on a small thread (see the module notes):
/// a host with its own JSON-LD door over oxjsonld should parse on a thread like
/// [`JSON_LD_STACK`], as `urn:rdf:transrept` does.
///
/// ```
/// use ikigai_rdf::{check_json_nesting, MAX_JSON_NESTING};
///
/// let nodes = |n: usize| {
///     format!("{}{{}}{}", "{\"urn:ex:p\":".repeat(n - 1), "}".repeat(n - 1))
/// };
/// assert!(check_json_nesting(nodes(MAX_JSON_NESTING).as_bytes(), "content").is_ok());
/// let refusal = check_json_nesting(nodes(MAX_JSON_NESTING + 1).as_bytes(), "content")
///     .unwrap_err()
///     .to_string();
/// assert!(refusal.starts_with("invalid argument `content`"), "{refusal}");
/// assert!(refusal.contains("deeper than 64 (MAX_JSON_NESTING)"), "{refusal}");
/// // Brackets in a string are not nesting.
/// let quoted = format!("{{\"@id\":\"urn:ex:s\",\"urn:ex:p\":\"{}\"}}", "[{".repeat(100));
/// assert!(check_json_nesting(quoted.as_bytes(), "content").is_ok());
/// ```
pub fn check_json_nesting(bytes: &[u8], arg: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // A string: ends at the first `"` a `\` does not escape. The loop leaves `i` on
                // that quote, and the step below moves past it.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_NESTING {
                    return Err(Error::InvalidArgument {
                        name: arg.to_string(),
                        detail: format!(
                            "this JSON-LD nests JSON objects and arrays deeper than \
                             {MAX_JSON_NESTING} (MAX_JSON_NESTING): the JSON-LD expander recurses \
                             once per nested node object, where a stack overflow aborts the whole \
                             host, and re-reads a node's content once per level. Real JSON-LD \
                             nests about 10 deep at most; flatten this one"
                        ),
                    });
                }
            }
            // A closer with nothing open is a syntax error the parser stops at.
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// Run `work`, a JSON-LD parse and what consumes it, on a thread of [`JSON_LD_STACK`], and wait
/// for it. A panic there is resumed here, so the caller sees what it would have inline; a thread
/// that cannot be spawned is a transient [`Error::Unavailable`], never a fallback to running
/// inline, which is the overflow this exists to prevent. On wasm there are no threads, and
/// `work` runs inline.
pub(crate) fn on_json_ld_stack<T, F>(work: F) -> Result<T>
where
    T: Send,
    F: FnOnce() -> Result<T> + Send,
{
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("ikigai-rdf-jsonld".to_string())
                .stack_size(JSON_LD_STACK)
                .spawn_scoped(scope, work)
                .map_err(|e| {
                    Error::Unavailable(format!(
                        "could not start a {} MiB thread to parse this JSON-LD on: {e}",
                        JSON_LD_STACK >> 20
                    ))
                })?;
            match handle.join() {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })
    }
    #[cfg(target_family = "wasm")]
    {
        work()
    }
}

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
