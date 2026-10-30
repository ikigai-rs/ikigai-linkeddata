//! The module recipe as one test: `ikigai-conformance` walks `urn:iki:feed:graph`, every RDF face
//! included (through ikigai-rdf, mounted beside it as a host would), with no opt-outs.
//!
//! - `feed-graph` is `pure` and `cacheable`: a function of the bytes it is handed (no file,
//!   network, clock or platform read), so its answer carries no golden thread of its own and
//!   hangs on whatever the piped feed's fetch carries.
//! - It needs a [`Fixture`]: the walk's minimal `x` is not a feed, and is refused as
//!   `InvalidArgument`, as it should be.
//! - `content` reads a body, so BODY-INPUTS hands it a stored body (`body_input_store`).
//! - VOCABULARY holds the RSS 1.0 terms to [`ikigai_feed::VOCABULARY`], the published
//!   definitions restated; Dublin Core, RDF and RDFS are well known.
//! - `space()` reads nothing while it is built, so it claims `urn:iki:space:feed`.

use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{Fallback, Kernel, MemoryContentStore, Verb};
use std::sync::Arc;

/// One item using every term of the form, so VOCABULARY sees each namespace it speaks.
const FEED: &str = r#"<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
<title>conformance</title><link>http://example.org/</link><description>d</description><language>en</language>
<item><title>one</title><link>http://example.org/1</link><guid>tag:example.org,2026:1</guid>
<description>s</description><content:encoded>c</content:encoded><author>a@example.org (A)</author>
<pubDate>Wed, 07 Oct 2026 10:00:00 GMT</pubDate><category domain="http://example.org/tags">t</category>
<category domain="text">u</category><enclosure url="http://example.org/1.mp3" length="1" type="audio/mpeg"/>
</item></channel></rss>"#;

#[test]
fn conforms() {
    let space = Arc::new(Fallback::new(vec![
        Arc::new(ikigai_feed::space()),
        Arc::new(ikigai_rdf::space()),
    ]));
    let store = Arc::new(MemoryContentStore::new(1024 * 1024));
    let kernel = Kernel::new(space.clone()).with_content_store(store.clone());
    let report = Suite::new()
        .own("feed-*")
        .vocabulary(ikigai_feed::VOCABULARY)
        .pure("feed-graph")
        .cacheable("feed-graph")
        .fixture(Fixture::new("feed-graph", Verb::Source).arg("content", FEED))
        .self_named_space("feed", ikigai_feed::space)
        .body_space(space)
        .body_input_store(store)
        .run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert!(report.unprobed.is_empty(), "every check probes: {report}");
    assert_eq!(report.endpoints, 1, "{report}");
    assert_eq!(report.actions, 1, "{report}");
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
    assert_eq!(
        ikigai_core::space_iri("feed").as_str(),
        ikigai_feed::SPACE_ID
    );
}
