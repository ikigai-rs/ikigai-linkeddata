//! Synthetic correctness probes for the feed form, without network input.
use ikigai_feed::{lift, vocab};
use oxrdf::{Literal, Term, Triple};

const BASE: &str = "https://example.org/feed";

fn atom(body: &str) -> String {
    format!(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><id>urn:feed</id><title>Feed</title><updated>2026-10-07T00:00:00Z</updated>{body}</feed>"#
    )
}

fn values(graph: &[Triple], ns: &str, local: &str) -> Vec<String> {
    let mut found: Vec<_> = graph
        .iter()
        .filter(|t| t.predicate.as_str() == format!("{ns}{local}"))
        .map(|t| match &t.object {
            Term::Literal(l) => l.value().to_owned(),
            other => panic!("expected literal: {other}"),
        })
        .collect();
    found.sort();
    found
}

#[test]
fn atom_author_parentheses_are_name_text() {
    let feed = atom(
        r#"<author><name>(Editorial)</name><email>editor@example.org</email></author><entry><id>urn:entry</id><title>Entry</title><author><name>Ada (Editor)</name></author></entry>"#,
    );
    let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
    assert_eq!(
        values(&graph, vocab::DC, "creator"),
        ["(Editorial)", "Ada (Editor)"]
    );
}

#[test]
fn invalid_rss_hour_is_omitted_instead_of_reset_to_midnight() {
    let feed = br#"<rss version="2.0"><channel><title>Feed</title><item><title>Entry</title><pubDate>07 Oct 2026 24:30:00 GMT</pubDate></item></channel></rss>"#;
    let graph = lift(feed, Some(BASE)).unwrap();
    for (ns, local) in [
        (vocab::DC, "date"),
        (vocab::DCTERMS, "issued"),
        (vocab::DCTERMS, "modified"),
    ] {
        assert!(
            values(&graph, ns, local).is_empty(),
            "invalid date emitted as {local}"
        );
    }
}

#[test]
fn atom_date_requires_a_time_and_zone() {
    for bad in [
        "2026-10-07",
        "07 Oct 2026 12:00:00 GMT",
        "2026-02-30T12:00:00Z",
        "2026-10-07T24:30:00Z",
        "2026-10-07t12:00:00Z",
        "2026-10-07T12:00:00z",
        "2026-10-07 12:00:00Z",
    ] {
        let feed = atom(&format!(
            "<entry><id>urn:entry</id><title>Entry</title><published>{bad}</published></entry>"
        ));
        let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
        assert!(
            values(&graph, vocab::DCTERMS, "issued").is_empty(),
            "accepted {bad}"
        );
    }
}

#[test]
fn rss_date_zones_and_two_digit_years_keep_the_instant() {
    for (zone, hour) in [
        ("GMT", "12"),
        ("UT", "12"),
        ("EST", "17"),
        ("EDT", "16"),
        ("CST", "18"),
        ("CDT", "17"),
        ("MST", "19"),
        ("MDT", "18"),
        ("PST", "20"),
        ("PDT", "19"),
        ("+0000", "12"),
        ("-0500", "17"),
        ("Z", "12"),
    ] {
        for year in ["2026", "26"] {
            let feed = format!("<rss version=\"2.0\"><channel><title>Feed</title><item><title>Entry</title><pubDate>07 Oct {year} 12:00:00 {zone}</pubDate></item></channel></rss>");
            let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
            assert_eq!(
                values(&graph, vocab::DCTERMS, "issued"),
                [format!("2026-10-07T{hour}:00:00Z")],
                "{year} {zone}"
            );
        }
    }
}

#[test]
fn atom_content_is_decoded_once_and_kept_as_a_literal() {
    for (kind, body, expected) in [
        (
            "text",
            "A &amp; B &lt;b&gt;text&lt;/b&gt;",
            "A & B <b>text</b>",
        ),
        (
            "html",
            "&lt;p&gt;A &amp;amp; B&lt;/p&gt;",
            "<p>A &amp; B</p>",
        ),
        ("html", "<![CDATA[<p>A &amp; B</p>]]>", "<p>A &amp; B</p>"),
    ] {
        let feed = atom(&format!("<entry><id>urn:entry</id><title>Entry</title><content type=\"{kind}\">{body}</content></entry>"));
        let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
        assert_eq!(values(&graph, vocab::CONTENT, "encoded"), [expected]);
        assert!(graph
            .iter()
            .any(|t| t.object == Term::Literal(Literal::new_simple_literal(expected))));
    }
}

#[test]
fn rss_military_zones_follow_the_rfc5322_unknown_offset_policy() {
    // RFC 5322 section 4.3 treats these obsolete zones as -0000 because
    // RFC 822 reversed the military offsets. The form drops zone information.
    for zone in (b'A'..=b'Y').filter(|z| *z != b'J') {
        for suffix in ["", " (zone)"] {
            let feed = format!("<rss version=\"2.0\"><channel><title>Feed</title><item><title>Entry</title><pubDate>07 Oct 2026 12:00:00 {}{suffix}</pubDate></item></channel></rss>", char::from(zone));
            let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
            assert_eq!(
                values(&graph, vocab::DCTERMS, "issued"),
                ["2026-10-07T12:00:00Z"],
                "{}",
                char::from(zone)
            );
        }
    }
}

#[test]
fn invalid_atom_updated_cannot_replace_a_valid_version() {
    let feed = atom(
        r#"<entry><id>urn:entry</id><title>Valid</title><updated>2026-10-07T00:00:00Z</updated></entry><entry><id>urn:entry</id><title>Invalid</title><updated>2099-01-01</updated></entry>"#,
    );
    let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
    assert_eq!(values(&graph, vocab::RSS, "title"), ["Feed", "Valid"]);
}

#[test]
fn atom_fractional_dates_and_offsets_map_published_and_updated_separately() {
    let feed = atom(
        r#"<entry><id>urn:entry</id><title>Entry</title><published>2026-10-07T10:00:00.125-07:00</published><updated>2026-10-08T18:00:00+01:00</updated></entry>"#,
    );
    let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
    assert_eq!(
        values(&graph, vocab::DCTERMS, "issued"),
        ["2026-10-07T17:00:00.125Z"]
    );
    assert_eq!(
        values(&graph, vocab::DCTERMS, "modified"),
        ["2026-10-07T00:00:00Z", "2026-10-08T17:00:00Z"]
    );
    assert!(graph.iter().filter(|t| t.predicate.as_str() == format!("{}issued", vocab::DCTERMS)).all(|t| matches!(&t.object, Term::Literal(l) if l.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#dateTime")));
}

#[test]
fn rss_versions_preserve_description_and_encoded_content_separately() {
    for version in ["0.91", "0.92", "2.0"] {
        let feed = format!(
            r#"<rss version="{version}" xmlns:c="http://purl.org/rss/1.0/modules/content/"><channel><title>Feed</title><link>https://example.org/</link><description>Channel</description><item><title>Entry</title><description>A &amp; B</description><c:encoded><![CDATA[<p>A &amp; B</p>]]></c:encoded></item></channel></rss>"#
        );
        let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
        assert_eq!(
            values(&graph, vocab::RSS, "description"),
            ["A & B", "Channel"],
            "{version}"
        );
        assert_eq!(
            values(&graph, vocab::CONTENT, "encoded"),
            ["<p>A &amp; B</p>"],
            "{version}"
        );
    }
}

#[test]
fn prefixed_atom_resolves_nested_bases_and_keeps_xhtml_markup() {
    let feed = r#"<a:feed xmlns:a="http://www.w3.org/2005/Atom" xml:base="../blog/"><a:id>urn:feed</a:id><a:title>Feed</a:title><a:entry xml:base="posts/"><a:id>urn:entry</a:id><a:title>Entry</a:title><a:link xml:base="../media/" href="./one?x=1&amp;y=2#part"/><a:content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>A &amp; B</p></div></a:content></a:entry></a:feed>"#;
    let graph = lift(feed.as_bytes(), Some("https://example.org/feeds/feed.xml")).unwrap();
    assert_eq!(
        values(&graph, vocab::RSS, "link"),
        ["https://example.org/blog/media/one?x=1&y=2#part"]
    );
    let content = values(&graph, vocab::CONTENT, "encoded");
    assert_eq!(content.len(), 1);
    assert!(content[0].contains("<p>A &amp; B</p>"), "{content:?}");
    assert!(graph.iter().all(
        |t| matches!(t.subject, oxrdf::NamedOrBlankNode::NamedNode(_))
            && !matches!(t.object, Term::BlankNode(_))
    ));
}

#[test]
fn identical_input_produces_byte_identical_turtle() {
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
    use std::sync::Arc;
    let feed = atom(
        r#"<entry><title>Missing id</title><content type="html">&lt;b&gt;Body&lt;/b&gt;</content><category term="b"/><category term="a"/></entry>"#,
    );
    let kernel = Kernel::new(Arc::new(ikigai_feed::space()));
    let request = Request::new(Verb::Source, Iri::parse("urn:iki:feed:graph").unwrap())
        .with_arg("content", ArgRef::Inline(feed.into_bytes()))
        .with_arg("base", ArgRef::Inline(BASE.as_bytes().to_vec()));
    let first = block_on(kernel.issue(request.clone(), &Capability::root())).unwrap();
    for _ in 0..8 {
        let next = block_on(kernel.issue(request.clone(), &Capability::root())).unwrap();
        assert_eq!(first.bytes, next.bytes);
    }
}

#[test]
fn rss_contact_delimiters_leave_parentheses_inside_the_name() {
    let feed = br#"<rss version="2.0"><channel><title>Feed</title><item><title>Entry</title><author>ada@example.org (Ada (Editor))</author></item><item><title>Other</title><author>(Editorial)</author></item></channel></rss>"#;
    let graph = lift(feed, Some(BASE)).unwrap();
    assert_eq!(
        values(&graph, vocab::DC, "creator"),
        ["(Editorial)", "Ada (Editor)"]
    );
}

#[test]
fn rss_comments_and_two_digit_year_boundary_keep_the_instant() {
    for (year, expected) in [
        ("49", "2049"),
        ("50", "1950"),
        ("99", "1999"),
        ("00", "2000"),
    ] {
        for zone in ["GMT", "Z", "UTC", "+0000"] {
            let feed = format!("<rss version=\"2.0\"><channel><title>Feed</title><item><title>Entry</title><pubDate>07 (day) Oct {year} 12:00:00 {zone} (zone (nested))</pubDate></item></channel></rss>");
            let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
            assert_eq!(
                values(&graph, vocab::DCTERMS, "issued"),
                [format!("{expected}-10-07T12:00:00Z")],
                "{year} {zone}"
            );
        }
    }
}

#[test]
fn atom_registered_relation_iris_mean_the_same_as_short_names() {
    let feed = atom(
        r#"<link rel="self" href="https://example.org/feed"/><entry><id>urn:entry</id><title>Entry</title><link rel="alternate" href="https://example.org/post"/><link rel="via" href="https://example.org/original"/><link rel="enclosure" href="https://example.org/audio" type="audio/ogg" length="123"/><source><link rel="self" href="https://example.org/source"/></source></entry>"#,
    );
    let expanded =
        ["self", "alternate", "via", "enclosure"]
            .into_iter()
            .fold(feed.clone(), |text, rel| {
                text.replace(
                    &format!("rel=\"{rel}\""),
                    &format!("rel=\"{}{rel}\"", vocab::IANA),
                )
            });
    let short: std::collections::HashSet<_> =
        lift(feed.as_bytes(), None).unwrap().into_iter().collect();
    let long: std::collections::HashSet<_> = lift(expanded.as_bytes(), None)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(short, long);
}

#[test]
fn person_projection_uses_author_name_else_email() {
    let feed = atom(
        r#"<author><name>Ada</name><email>ada@example.org</email><uri>https://example.org/ada</uri></author><contributor><name>Contributor</name></contributor><entry><id>urn:entry</id><title>Entry</title><author><email>editor@example.org</email></author></entry>"#,
    );
    let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
    assert_eq!(
        values(&graph, vocab::DC, "creator"),
        ["Ada", "editor@example.org"]
    );
    // The form projects authors to literals, not person nodes or contributors.
    assert!(graph
        .iter()
        .all(|t| t.object.to_string() != "<https://example.org/ada>"));
}

#[test]
fn relative_external_content_is_a_documented_loss() {
    let feed = atom(
        r#"<entry xml:base="https://example.org/posts/"><id>urn:entry</id><title>Entry</title><content src="body.html" type="text/html"/></entry>"#,
    );
    let graph = lift(feed.as_bytes(), Some(BASE)).unwrap();
    assert!(graph
        .iter()
        .all(|t| t.predicate.as_str() != format!("{}hasFormat", vocab::DCTERMS)));
}
