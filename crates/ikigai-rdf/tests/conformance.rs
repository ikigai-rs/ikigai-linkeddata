#![cfg(feature = "transrept")]

//! The module recipe as one test: `ikigai-conformance` walks the three endpoints
//! [`ikigai_rdf::space`] binds and reports every violation at once.
//!
//! This is the RDF hub, so the two graph checks are the ones that matter here:
//! every RDF face `rdf-transrept` declares (six syntaxes, selected by `as=`) is
//! resolved, parsed, and read for blank nodes and vocabulary terms, and so are
//! the Turtle faces of `rdf-union` and `rdf-diff`. A transreptor emits what it is
//! given, so the fixture graph is skolemized (stable `urn:` IRIs, no `[ … ]`) and
//! uses only defined terms; the walk then proves that the SERIALIZERS mint no
//! blank node on the way through any syntax — the property union and diff rely
//! on (set semantics over triples that compare by name, never by isomorphism).
//!
//! ## Declarations, and why each
//!
//! All three endpoints are `pure` AND `cacheable`: each is a function of the
//! documents it is handed (no file, network, clock or platform read), marks its
//! result `.cacheable()`, and rightly carries an empty golden-thread set when
//! every input arrives inline. When `with=` names a resource instead, the kernel
//! folds that resolution's expiry and threads in, so union and diff are exactly
//! as cacheable as the graph they were joined against and never more. Declared
//! cacheable, the suite holds them to a cache hit on the second resolution.
//!
//! ## Fixtures
//!
//! The suite's minimal scalar is `x`, which no RDF parser accepts, so every
//! action takes a [`Fixture`]: a two-triple graph as `content`, and for union
//! and diff a second one inline as `with=`. `as=` is deliberately NOT fixed: the
//! suite sets it per declared output, which is how all six RDF faces of the
//! transreptor get exercised from one fixture.
//!
//! The transreptor's `content` and `src` are a group, exactly one of them, so REQUIRED also
//! calls it with only `src`: with the IRI it samples for an `xsd:anyURI`,
//! `urn:example:conformance`. [`SOURCE`] binds that IRI beside the module, serving [`BASE`],
//! so the call is answered and the group is observed rather than left unprobed. It is the
//! test's, not the module's, so the walk is scoped to [`ENDPOINTS`] and it is inherited.
//!
//! `space()` is declared self-named (SPACE-NAME): it reads nothing while it is built, so it
//! claims `urn:iki:space:rdf`, exported as `SPACE_ID`.
//!
//! No opt-outs, no module namespace (a face carries the caller's terms, and the
//! fixture's are defined ones), and NAMES runs: every id here is kebab-case.

use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{
    EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Kernel, ReprType, Representation,
    Space, Verb,
};
use std::sync::Arc;

/// The three endpoints `space()` binds, by description id.
const TRANSREPT: &str = "rdf-transrept";
const UNION: &str = "rdf-union";
const DIFF: &str = "rdf-diff";
const ENDPOINTS: [&str; 3] = [TRANSREPT, UNION, DIFF];

/// A skolemized graph over defined terms — what a module's RDF face looks like:
/// `ik:Endpoint` is in ikigai-vocab, `dcterms:title` under a well-known namespace.
const BASE: &str = "@prefix dcterms: <http://purl.org/dc/terms/> .\n\
<urn:example:conformance> a <https://ikigai-rs.dev/ns#Endpoint> ; dcterms:title \"base\" .\n";

/// The second graph for `with=`, inline: one triple shared with [`BASE`]'s
/// subject's neighborhood, one not, so union and diff each have work to do.
const WITH: &str = "@prefix dcterms: <http://purl.org/dc/terms/> .\n\
<urn:example:conformance> a <https://ikigai-rs.dev/ns#Endpoint> .\n\
<urn:example:conformance:other> dcterms:title \"with\" .\n";

/// The fixture source `src=` names: the description id of the endpoint at the IRI the suite
/// samples for an `xsd:anyURI`.
const SOURCE: &str = "conformance-source";

/// The module's space beside [`SOURCE`], which serves [`BASE`] at `urn:example:conformance`.
fn kernel() -> Kernel {
    let source = EndpointSpace::new().bind(
        Exact::new("urn:example:conformance"),
        FnEndpoint::new(SOURCE, |_: &Invocation<'_>| {
            Ok(
                Representation::new(ReprType::new("text/turtle"), BASE.as_bytes().to_vec())
                    .cacheable(),
            )
        }),
    );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(ikigai_rdf::space()) as Arc<dyn Space>,
        Arc::new(source) as Arc<dyn Space>,
    ])))
}

/// The suite, configured for this module (see the file docs for why each line).
fn suite() -> Suite {
    ENDPOINTS
        .iter()
        .fold(Suite::new(), |suite, id| {
            let fixture = Fixture::new(*id, Verb::Source).arg("content", BASE);
            let fixture = if *id == TRANSREPT {
                fixture
            } else {
                fixture.arg("with", WITH)
            };
            suite.fixture(fixture).pure(*id).cacheable(*id)
        })
        .own_all(ENDPOINTS)
        .inherit(SOURCE, "this test's source for src=, not the module's")
}

#[test]
fn conforms() {
    let kernel = kernel();
    // SPACE-NAME: `space()` is configuration-free, so it names itself `urn:iki:space:rdf`,
    // and two calls hold the same doors under that name.
    let report = suite()
        .self_named_space("rdf", ikigai_rdf::space)
        .run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert_eq!(ikigai_core::space_iri("rdf").as_str(), ikigai_rdf::SPACE_ID);
    // The walk saw exactly the endpoints declared above. A fourth bound without
    // a `pure`/`cacheable` line would be held to a weaker standard (the suite
    // cannot know which endpoints it was not told about); a declared id that
    // binds nothing is a stale list. Both change this count or fail above.
    assert_eq!(
        report.endpoints,
        ENDPOINTS.len(),
        "transrept, union, diff: {report}"
    );
    assert_eq!(
        report.actions,
        ENDPOINTS.len(),
        "one Source action per endpoint: {report}"
    );
    // Nothing is skipped: every id is already kebab-case, so NAMES runs too.
    // Exactly one skip: FAILURES, off unless a suite opts in (`check_failures()`, the next wave).
    assert_eq!(
        report.checks.skipped().count(),
        1,
        "every check runs but FAILURES: {report}"
    );
}

/// The transreptor's declared outputs ARE its `to` list. The suite probes faces
/// from `outputs`; selection routes on `transreptsTo`. If the two drift, one
/// consumer sees a face the other cannot reach.
#[test]
fn the_transreptor_declares_every_face_it_can_produce() {
    let kernel = Kernel::new(Arc::new(ikigai_rdf::space()));
    let description = kernel
        .describe_pattern("urn:rdf:transrept")
        .expect("urn:rdf:transrept describes itself");
    let transreption = description
        .transreption()
        .expect("rdf-transrept is an ik:Transreptor");
    assert_eq!(
        description.outputs, transreption.to,
        "declared outputs and transreptsTo must be the same list, in the same order"
    );
}
