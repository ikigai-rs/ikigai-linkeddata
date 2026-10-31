//! The module recipe as one test: `ikigai-conformance` walks `urn:iki:html:graph`, every RDF face
//! included (through ikigai-rdf, mounted beside it as a host would), with no opt-outs.
//!
//! - `html-graph` is `pure` and `cacheable`: a function of the bytes it is handed, or of the
//!   graph it resolves through the kernel (whose golden thread it then inherits).
//! - The walk's minimal `x` is HTML too (a text node), so the [`Fixture`] is only there to
//!   give VOCABULARY an element and an attribute to see.
//! - `content` reads a body, so BODY-INPUTS hands it a stored body (`body_input_store`).
//! - VOCABULARY holds the `htm:` terms to [`ikigai_html::VOCABULARY`]; `rdf:`, `dcterms:` and
//!   `schema:` are well known.
//! - `space()` reads nothing while it is built, so it claims `urn:iki:space:html`.

use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{Fallback, Kernel, MemoryContentStore, Verb};
use std::sync::Arc;

const PAGE: &str = r#"<blockquote cite="https://example.org/s"><p>q</p></blockquote>"#;

#[test]
fn conforms() {
    let space = Arc::new(Fallback::new(vec![
        Arc::new(ikigai_html::space()),
        Arc::new(ikigai_rdf::space()),
    ]));
    let store = Arc::new(MemoryContentStore::new(1024 * 1024));
    let kernel = Kernel::new(space.clone()).with_content_store(store.clone());
    let report = Suite::new()
        .own("html-*")
        .vocabulary(ikigai_html::VOCABULARY)
        .pure("html-graph")
        .cacheable("html-graph")
        .fixture(Fixture::new("html-graph", Verb::Source).arg("content", PAGE))
        .self_named_space("html", ikigai_html::space)
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
        ikigai_core::space_iri("html").as_str(),
        ikigai_html::SPACE_ID
    );
}
