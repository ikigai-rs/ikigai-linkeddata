//! The module recipe as one test, twice: `ikigai-conformance` walks the four
//! query forms [`ikigai_sparql::space`] binds, then the five endpoints
//! [`ikigai_sparql::space_with_store`] binds over a caller-owned store.
//!
//! The suite FIRES the actions it checks — `urn:sparql:update` included — so the
//! shared-store kernel is a fixture: a fresh in-memory [`Store`] this file owns,
//! seeded with the bundled vocabulary so DESCRIBE and CONSTRUCT have triples to
//! emit. Nothing here reaches outside the process.
//!
//! ## What the RDF checks see here
//!
//! CONSTRUCT and DESCRIBE declare six RDF faces (`text/turtle` … `application/
//! ld+json`), so the suite resolves each with `as=<face>`, parses it, and reads
//! it for blank nodes and undefined terms. The fixture queries run over the
//! always-loaded ikigai vocabulary — so what is really being checked is that
//! the vocabulary this module folds into EVERY query's default graph carries no
//! blank node and no term it does not define, and that six serializers preserve
//! that. A blank node arriving in the vocabulary would surface here first, and
//! should.
//!
//! ## Declarations, and why each
//!
//! - `space()`'s four forms are `pure` AND `cacheable`. Each marks its result
//!   `.cacheable()` and, with no `graph=`, is a function of the query and the
//!   bundled (static) vocabulary — an empty golden-thread set is right. With
//!   `graph=` the sources are resolved through the kernel and their threads join
//!   the result's, which is the whole federation-with-invalidation design.
//! - `space_with_store()`'s four forms are declared NEITHER: they answer from a
//!   live store other modules write through a raw handle, so their results are
//!   uncacheable by design (see [`ikigai_sparql::UPDATE_THREAD`] for why a
//!   partially-covered thread would be worse). The suite's probe has nothing to
//!   hold them to, and that is the intended outcome, not a gap.
//! - `sparql-update` is a `Sink` with `requires urn:cap:sparql:update`. ENFORCED
//!   fires it under a capability holding no grants and expects `Denied`;
//!   PIPELINE fires it under root with the fixture's `content` and expects the
//!   text to be read — the second test checks the write landed, so "declares
//!   `content`, reads `content`" is proven by the store, not inferred from the
//!   absence of an error.
//!
//! ## Fixtures
//!
//! The suite's minimal scalar is `x`, which is not a SPARQL query, so every form
//! takes a [`Fixture`] carrying one. `as=` is never fixed: the suite sets it per
//! declared face, and the endpoint's per-form default covers the rest.
//!
//! No opt-outs, no module namespace, and NAMES runs: every id is kebab-case.

use ikigai_conformance::{Fixture, Report, Suite};
use ikigai_core::{Kernel, Verb};
use ikigai_sparql::{load_vocabulary, Store};
use std::sync::Arc;

/// The four query forms both spaces bind, by description id, each with a query
/// that is valid over the vocabulary alone (and over an empty store).
const FORMS: [(&str, &str); 4] = [
    ("sparql-select", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"),
    ("sparql-ask", "ASK { ?s ?p ?o }"),
    // A class every vocabulary version defines: the transreptor concept.
    (
        "sparql-describe",
        "DESCRIBE <https://ikigai-rs.dev/ns#Transreptor>",
    ),
    // Every typed subject: the largest graph the vocabulary yields with only
    // `rdf:type` as a predicate, so the terms check reads the vocabulary's classes.
    (
        "sparql-construct",
        "CONSTRUCT { ?s a ?c } WHERE { ?s a ?c }",
    ),
];

/// The fifth endpoint, bound only over a shared store.
const UPDATE: &str = "sparql-update";

/// What the pipeline probe sinks: one skolemized triple under a defined term.
const INSERT: &str =
    "INSERT DATA { <urn:example:conformance> <http://purl.org/dc/terms/title> \"conformance\" }";

/// The four forms' fixtures, shared by both walks.
fn form_fixtures(suite: Suite) -> Suite {
    FORMS.iter().fold(suite, |suite, (id, query)| {
        suite.fixture(Fixture::new(*id, Verb::Source).arg("query", *query))
    })
}

/// Every check ran, and the walk saw exactly `endpoints` endpoints with one
/// action each. A sixth endpoint bound without a line here would be held to a
/// weaker standard; a declared id that binds nothing is a stale list.
fn assert_shape(report: &Report, endpoints: usize) {
    assert_eq!(report.endpoints, endpoints, "{report}");
    assert_eq!(
        report.actions, endpoints,
        "one action per endpoint (Source, or Sink for update): {report}"
    );
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
}

#[test]
fn conforms() {
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space()));
    let suite = FORMS
        .iter()
        .fold(form_fixtures(Suite::new()), |suite, (id, _)| {
            suite.pure(*id).cacheable(*id)
        });
    let report = suite.run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report, FORMS.len());
}

#[test]
fn shared_store_conforms() {
    let store = Arc::new(Store::new().unwrap());
    load_vocabulary(&store).unwrap();
    let seeded = store.len().unwrap();
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space_with_store(Arc::clone(
        &store,
    ))));
    let suite = form_fixtures(Suite::new())
        .fixture(Fixture::new(UPDATE, Verb::Sink).arg("content", INSERT));
    let report = suite.run_blocking(&kernel);
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report, FORMS.len() + 1);
    // The suite fired the Sink exactly once under root (the pipeline probe; the
    // ENFORCED probe under no grants was refused before invoke), and the update
    // read the piped `content`: the fixture's triple is in the store. This is the
    // half of "declares content ⇒ reads it" the suite can only infer.
    assert_eq!(
        store.len().unwrap(),
        seeded + 1,
        "the pipeline probe's INSERT landed exactly once"
    );
}
