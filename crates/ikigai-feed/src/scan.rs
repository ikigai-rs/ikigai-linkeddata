//! The bounding pre-scan: one ITERATIVE pass of quick-xml over the caller's bytes, BEFORE
//! feed-rs sees them.
//!
//! It does two jobs, both because the parse cannot:
//!
//! 1. **Bound depth.** feed-rs walks elements with a depth counter, but its handlers recurse
//!    into the elements they know (Atom `source`, XHTML content, MediaRSS groups) and keep an
//!    `xml:base` per open element. Request input is bounded before any recursive parse (the
//!    field guide's rule, `ikigai-jsonld/tests/json_depth.rs` the pattern), so an element nested
//!    deeper than [`MAX_FEED_DEPTH`] is refused here, counted on a `Vec`, never on the stack.
//! 2. **Recover what feed-rs's model drops** and the form needs: the RSS 2.0 `guid`'s
//!    `isPermaLink` (the item IRI rule turns on it), RSS 1.0's `rdf:about` on the channel and
//!    each item (they are already IRIs), the `xml:lang` in effect on each Atom entry, an
//!    entry's source feed (Atom's `atom:source`, RSS 2.0's `source/@url`), and RSS 1.0's
//!    `dc:subject` (feed-rs reads no category in RSS 1.0).
//!
//! It reads attributes, and the text of exactly two elements at fixed places: RSS 1.0's
//! `dc:subject` on the channel or an item, and `atom:source/atom:id`. It decides which elements
//! are entries the way feed-rs does (the root's local name; entries as its or its channel's
//! children), so the `n`th entry here is feed-rs's `n`th. The caller checks the counts agree
//! before using one.

use ikigai_core::{Error, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;

/// The deepest element nesting admitted (the root is depth 1). A feed is shallow: RSS 2.0 puts
/// an item's fields at depth 4 and Atom at depth 3, so this leaves 60 levels of XHTML content
/// or extension markup. Deeper is refused before the parse.
pub const MAX_FEED_DEPTH: usize = 64;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const RSS1: &str = "http://purl.org/rss/1.0/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC: &str = "http://purl.org/dc/elements/1.1/";

/// Which feed the root element names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    /// `<rss>`: RSS 0.9x or 2.0.
    Rss,
    /// `<rdf:RDF>`: RSS 1.0.
    Rdf,
    /// `<feed>`: Atom 1.0.
    Atom,
}

/// What the scan saw on one entry (an RSS `item` or an Atom `entry`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// RSS 1.0: the item's `rdf:about`.
    pub about: Option<String>,
    /// RSS 2.0: the `guid`'s `isPermaLink`, when it says `true` or `false`.
    pub permalink: Option<bool>,
    /// The `xml:lang` in effect on the entry (its own or inherited).
    pub lang: Option<String>,
    /// RSS 1.0: the item's `dc:subject` texts, trimmed, in document order.
    pub subjects: Vec<String>,
    /// The feed the entry was copied from: Atom's `atom:source`, RSS 2.0's `source/@url`.
    pub source: Source,
}

/// What names an entry's source feed, as written (the lift keeps only absolute IRIs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// Atom: `atom:source/atom:link rel="self"`. RSS 2.0: `source/@url`.
    pub this: Option<String>,
    /// Atom: `atom:source/atom:id`.
    pub id: Option<String>,
    /// Atom: the first `atom:source/atom:link` with `rel="alternate"` or no `rel`.
    pub alternate: Option<String>,
}

/// An element whose text the scan keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Text {
    ChannelSubject,
    ItemSubject,
    SourceId,
}

/// The scan's findings.
#[derive(Debug)]
pub struct Scan {
    pub root: Root,
    /// RSS 1.0: the channel's `rdf:about`.
    pub channel_about: Option<String>,
    /// The `xml:lang` in effect on the channel (RSS) or feed (Atom).
    pub channel_lang: Option<String>,
    /// RSS 1.0: the channel's `dc:subject` texts, trimmed, in document order.
    pub channel_subjects: Vec<String>,
    pub entries: Vec<Seen>,
    /// Whether an RSS 1.0 `rss:channel` sat under `rdf:RDF`: RDF/XML without one is not a feed.
    channel: bool,
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: "content".into(),
        detail: detail.into(),
    }
}

/// What a refusal says is accepted.
pub const ACCEPTED: &str =
    "expected an RSS 0.9x/2.0 (<rss>), RSS 1.0 (<rdf:RDF>) or Atom 1.0 (<feed>) document";

/// Scan `bytes`, refusing a body that is not well-formed XML, does not open with a feed root,
/// or nests deeper than [`MAX_FEED_DEPTH`].
pub fn scan(bytes: &[u8]) -> Result<Scan> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    // One entry per open element: the `xml:lang` in effect inside it.
    let mut langs: Vec<Option<String>> = Vec::new();
    let mut found: Option<Scan> = None;
    // The depth of the entry being read, while one is open.
    let mut entry_depth: Option<usize> = None;
    // The depth of the RSS 1.0 channel, or of an entry's `atom:source`, while one is open.
    let mut channel_depth: Option<usize> = None;
    let mut source_depth: Option<usize> = None;
    // The element whose text is being kept, its depth, and the text so far.
    let mut text: Option<(Text, usize, String)> = None;
    loop {
        let (ns, event) = match reader.read_resolved_event() {
            Ok((ns, event)) => (namespace(ns), event),
            Err(e) => return Err(invalid(format!("not well-formed XML: {e}; {ACCEPTED}"))),
        };
        match event {
            Event::Start(element) => {
                let depth = langs.len() + 1;
                if depth > MAX_FEED_DEPTH {
                    return Err(invalid(format!(
                        "elements nest deeper than {MAX_FEED_DEPTH} levels"
                    )));
                }
                let lang = attribute(&reader, &element, None, "xml:lang")
                    .or_else(|| langs.last().cloned().flatten());
                let name = element.local_name();
                let local: &str = name.into_inner();
                match (&mut found, depth) {
                    (None, 1) => {
                        let root = match local {
                            "rss" => Root::Rss,
                            "RDF" => Root::Rdf,
                            "feed" => Root::Atom,
                            _ => return Err(invalid(ACCEPTED)),
                        };
                        found = Some(Scan {
                            root,
                            channel_about: None,
                            channel_lang: (root == Root::Atom).then(|| lang.clone()).flatten(),
                            channel_subjects: Vec::new(),
                            entries: Vec::new(),
                            channel: false,
                        });
                    }
                    (Some(scan), _) => match (scan.root, depth, local) {
                        (Root::Atom, 2, "entry") if matches!(ns.as_deref(), None | Some(ATOM)) => {
                            entry_depth = Some(depth);
                            scan.entries.push(Seen {
                                lang: lang.clone(),
                                ..Seen::default()
                            });
                        }
                        (Root::Rss, 2, "channel") if ns.is_none() => {
                            scan.channel_lang = lang.clone();
                        }
                        (Root::Rss, 3, "item") if ns.is_none() => {
                            entry_depth = Some(depth);
                            scan.entries.push(Seen {
                                lang: lang.clone(),
                                ..Seen::default()
                            });
                        }
                        (Root::Rss, 4, "guid") if ns.is_none() && entry_depth == Some(3) => {
                            let flag = attribute(&reader, &element, None, "isPermaLink");
                            if let Some(seen) = scan.entries.last_mut() {
                                seen.permalink = match flag.as_deref().map(str::trim) {
                                    Some(v) if v.eq_ignore_ascii_case("true") => Some(true),
                                    Some(v) if v.eq_ignore_ascii_case("false") => Some(false),
                                    _ => None,
                                };
                            }
                        }
                        (Root::Atom, 3, "source")
                            if ns.as_deref() == Some(ATOM) && entry_depth == Some(2) =>
                        {
                            source_depth = Some(depth);
                        }
                        (Root::Atom, 4, "id")
                            if ns.as_deref() == Some(ATOM) && source_depth == Some(3) =>
                        {
                            text = Some((Text::SourceId, depth, String::new()));
                        }
                        (Root::Atom, 4, "link")
                            if ns.as_deref() == Some(ATOM) && source_depth == Some(3) =>
                        {
                            let rel = attribute(&reader, &element, None, "rel");
                            let href = attribute(&reader, &element, None, "href");
                            if let Some(seen) = scan.entries.last_mut() {
                                let source = &mut seen.source;
                                match rel.as_deref().map(str::trim) {
                                    Some("self") if source.this.is_none() => source.this = href,
                                    None | Some("alternate") if source.alternate.is_none() => {
                                        source.alternate = href
                                    }
                                    _ => {}
                                }
                            }
                        }
                        (Root::Rss, 4, "source") if ns.is_none() && entry_depth == Some(3) => {
                            if let Some(seen) = scan.entries.last_mut() {
                                seen.source.this = attribute(&reader, &element, None, "url");
                            }
                        }
                        (Root::Rdf, 3, "subject") if ns.as_deref() == Some(DC) => {
                            if entry_depth == Some(2) {
                                text = Some((Text::ItemSubject, depth, String::new()));
                            } else if channel_depth == Some(2) {
                                text = Some((Text::ChannelSubject, depth, String::new()));
                            }
                        }
                        (Root::Rdf, 2, "channel") if ns.as_deref() == Some(RSS1) => {
                            channel_depth = Some(depth);
                            scan.channel_about = attribute(&reader, &element, Some(RDF), "about");
                            scan.channel = true;
                            scan.channel_lang = lang.clone();
                        }
                        (Root::Rdf, 2, "item") if ns.as_deref() == Some(RSS1) => {
                            entry_depth = Some(depth);
                            scan.entries.push(Seen {
                                about: attribute(&reader, &element, Some(RDF), "about"),
                                lang: lang.clone(),
                                ..Seen::default()
                            });
                        }
                        _ => {}
                    },
                    (None, _) => unreachable!("the root is the first element"),
                }
                langs.push(lang);
            }
            Event::End(_) => {
                let depth = Some(langs.len());
                if text.as_ref().map(|(_, d, _)| *d) == depth {
                    let (kind, _, value) = text.take().expect("the kept element is open");
                    keep(found.as_mut(), kind, value);
                }
                for open in [&mut entry_depth, &mut channel_depth, &mut source_depth] {
                    if *open == depth {
                        *open = None;
                    }
                }
                langs.pop();
            }
            // The text of a kept element, entity references resolved as an attribute's are.
            Event::Text(t) => {
                if let Some((_, _, value)) = text.as_mut() {
                    value.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some((_, _, value)) = text.as_mut() {
                    value.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, _, value)) = text.as_mut() {
                    let name = r.xml10_content();
                    match r.resolve_char_ref() {
                        Ok(Some(c)) => value.push(c),
                        _ => match quick_xml::escape::resolve_predefined_entity(&name) {
                            Some(resolved) => value.push_str(resolved),
                            None => value.push_str(&format!("&{name};")),
                        },
                    }
                }
            }
            Event::Eof if !langs.is_empty() => {
                return Err(invalid(format!(
                    "not well-formed XML: unclosed elements; {ACCEPTED}"
                )))
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match found {
        Some(scan) if scan.root != Root::Rdf || scan.channel => Ok(scan),
        Some(_) => Err(invalid(format!(
            "RDF/XML with no RSS 1.0 channel; {ACCEPTED}"
        ))),
        None => Err(invalid(ACCEPTED)),
    }
}

/// File the text of a kept element where it belongs. Empty text is no subject and no id.
fn keep(scan: Option<&mut Scan>, kind: Text, value: String) {
    let Some(scan) = scan else { return };
    let value = value.trim().to_string();
    if value.is_empty() {
        return;
    }
    match kind {
        Text::ChannelSubject => scan.channel_subjects.push(value),
        Text::ItemSubject => {
            if let Some(seen) = scan.entries.last_mut() {
                seen.subjects.push(value);
            }
        }
        Text::SourceId => {
            if let Some(seen) = scan.entries.last_mut() {
                seen.source.id.get_or_insert(value);
            }
        }
    }
}

/// The resolved namespace of an element, owned so the reader can be borrowed again.
fn namespace(ns: ResolveResult<'_>) -> Option<String> {
    match ns {
        ResolveResult::Bound(ns) => Some(ns.into_inner().to_string()),
        ResolveResult::Unbound | ResolveResult::Unknown(_) => None,
    }
}

/// The value of attribute `local` on `element`: in namespace `ns`, or, with `ns` `None`, the
/// attribute written exactly as `local` (an unprefixed name, or `xml:lang` spelled out).
fn attribute(
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
    ns: Option<&str>,
    local: &str,
) -> Option<String> {
    element.attributes().flatten().find_map(|attr| {
        let matches = match ns {
            None => attr.key.into_inner() == local,
            Some(ns) => {
                let (resolved, name) = reader.resolver().resolve_attribute(attr.key);
                matches!(resolved, ResolveResult::Bound(n) if n.into_inner() == ns)
                    && name.into_inner() == local
            }
        };
        // An entity the reader cannot expand (`&nbsp;` with no DTD) keeps the text as written.
        matches.then(|| match quick_xml::escape::unescape(&attr.value) {
            Ok(value) => value.into_owned(),
            Err(_) => attr.value.clone().into_owned(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_is_counted_iteratively_and_refused_past_the_bound() {
        let deep = |n: usize| format!("<feed>{}{}</feed>", "<x>".repeat(n), "</x>".repeat(n));
        assert!(scan(deep(MAX_FEED_DEPTH - 1).as_bytes()).is_ok());
        let err = scan(deep(MAX_FEED_DEPTH).as_bytes())
            .unwrap_err()
            .to_string();
        assert!(err.contains("deeper than 64"), "{err}");
        // A million levels cost a Vec, not the stack: refused at the 65th.
        assert!(scan(deep(1_000_000).as_bytes()).is_err());
    }

    #[test]
    fn recovers_what_the_model_drops() {
        let rss = br#"<rss version="2.0"><channel xml:lang="en"><item><guid isPermaLink="false">a</guid></item><item><guid>http://x/b</guid></item></channel></rss>"#;
        let s = scan(rss).unwrap();
        assert_eq!(s.root, Root::Rss);
        assert_eq!(s.channel_lang.as_deref(), Some("en"));
        assert_eq!(s.entries[0].permalink, Some(false));
        assert_eq!(s.entries[1].permalink, None);
        let rdf = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/"><channel rdf:about="http://x/feed"/><item rdf:about="http://x/1"/></rdf:RDF>"#;
        let s = scan(rdf).unwrap();
        assert_eq!(s.channel_about.as_deref(), Some("http://x/feed"));
        assert_eq!(s.entries[0].about.as_deref(), Some("http://x/1"));
    }

    #[test]
    fn keeps_text_only_where_it_is_read() {
        // dc:subject on the channel and an item; one anywhere else (here, an image) is not read.
        let rdf = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel rdf:about="http://x/feed"><dc:subject> a </dc:subject></channel><image><dc:subject>no</dc:subject></image><item rdf:about="http://x/1"><dc:subject>b&amp;c</dc:subject><dc:subject/></item></rdf:RDF>"#;
        let s = scan(rdf).unwrap();
        assert_eq!(s.channel_subjects, vec!["a"]);
        assert_eq!(s.entries[0].subjects, vec!["b&c"]);
        // atom:source's id and links; the entry's own id and links are not the source's.
        let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>http://x/e</id><link href="http://x/e"/><source><id>tag:x,2026:s</id><link rel="self" href="http://x/s.atom"/><link href="http://x/s"/></source></entry></feed>"#;
        let s = scan(atom).unwrap();
        assert_eq!(
            s.entries[0].source,
            Source {
                this: Some("http://x/s.atom".into()),
                id: Some("tag:x,2026:s".into()),
                alternate: Some("http://x/s".into()),
            }
        );
        let rss = br#"<rss><channel><item><source url="http://x/r.xml">R</source></item></channel></rss>"#;
        assert_eq!(
            scan(rss).unwrap().entries[0].source.this.as_deref(),
            Some("http://x/r.xml")
        );
    }

    #[test]
    fn refuses_what_is_not_a_feed() {
        for body in [
            &b"<html/>"[..],
            b"{\"version\":\"https://jsonfeed.org/version/1.1\"}",
            b"<rss><channel>",
        ] {
            assert!(
                matches!(scan(body), Err(Error::InvalidArgument { .. })),
                "{body:?}"
            );
        }
    }
}
