//! The module recipe walks both reasoning resources, including every RDF face.
//! RDF transreption and the graph fixture are inherited host resources, outside
//! this module's ownership; no checks on the reasoning endpoints are waived.
use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{Exact, FnEndpoint, ReprType, Representation, Verb};
use ikigai_reason::{space, space_with_limits, Limits, SPACE_ID};

#[test]
fn conforms() {
    let composed = space().bind(Exact::new("urn:test:graph"), FnEndpoint::new("fixture", |_| Ok(Representation::new(ReprType::new("text/turtle"),
        b"@prefix dcterms: <http://purl.org/dc/terms/> . <urn:example:conformance> dcterms:title \"conformance\" .".to_vec()).cacheable())));
    let mut suite = Suite::new()
        .own("reason-*")
        .self_named_space("reason", space)
        .host_named_space("space_with_limits", space_with_limits(Limits::default()));
    for id in ["reason-closure", "reason-inferred"] {
        suite = suite
            .pure(id)
            .cacheable(id)
            .fixture(Fixture::new(id, Verb::Source).arg("graph", "urn:test:graph"));
    }
    // The same composite space serves byte and body probes.
    let space = std::sync::Arc::new(ikigai_core::Fallback::new(vec![
        std::sync::Arc::new(composed),
        std::sync::Arc::new(ikigai_rdf::space()),
    ]));
    let k = ikigai_core::Kernel::new(space.clone());
    let report = suite.body_space(space).run_blocking(&k);
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert!(report.unprobed.is_empty(), "{report}");
    assert_eq!(report.endpoints, 2);
    assert_eq!(report.actions, 2);
    assert_eq!(report.checks.skipped().count(), 0, "{report}");
    assert_eq!(ikigai_core::space_iri("reason").as_str(), SPACE_ID);
}
