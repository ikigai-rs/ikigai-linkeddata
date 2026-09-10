//! The module recipe as one test: `ikigai-conformance` walks the two endpoints
//! [`ikigai_sniff::space`] binds and reports every violation at once.
//!
//! ## Declarations, and why each
//!
//! - `sniff` is `pure` AND `cacheable`: a bounded-prefix heuristic over the
//!   bytes it is handed (no file, network, clock or platform read), marked
//!   `.cacheable()`, so its result rightly carries an empty golden-thread set.
//!   No fixture: the suite's minimal `x` is valid input (it sniffs as
//!   `text/plain`), which is the point of a detector that is total.
//! - `transrept-auto` is `pure` AND `cacheable` for the same reason, with one
//!   more clause: it delegates to whatever transreptor chain the kernel selects,
//!   and the kernel folds those resolutions' expiry and threads in — so it is as
//!   cacheable as the chain it ran, and no more. It needs a [`Fixture`]: with
//!   `x` it sniffs `text/plain`, and no transreptor reaches `text/turtle` from
//!   there. The fixture hands it Turtle, so under conformance the dispatcher is
//!   exercised on its pass-through arm (sniffed type == requested type). The
//!   dispatch arm needs a transreptor bound beside it, and `space()` binds none
//!   by design — it is covered by this crate's unit tests with a stub, and by
//!   `ikigai-rdf`'s own walk for the real one.
//!
//! No opt-outs, no module namespace (there is no RDF face), and NAMES runs:
//! both ids are kebab-case.

use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{Kernel, Verb};
use std::sync::Arc;

/// The two endpoints `space()` binds, by description id.
const SNIFF: &str = "sniff";
const AUTO: &str = "transrept-auto";
const ENDPOINTS: [&str; 2] = [SNIFF, AUTO];

/// Bytes that sniff as `text/turtle` — the default `as=`, so the call is a
/// pass-through rather than a dispatch to a transreptor this space does not bind.
const TURTLE: &str = "@prefix dcterms: <http://purl.org/dc/terms/> .\n\
<urn:example:conformance> dcterms:title \"conformance\" .\n";

/// The suite, configured for this module (see the file docs for why each line).
fn suite() -> Suite {
    ENDPOINTS
        .iter()
        .fold(Suite::new(), |suite, id| suite.pure(*id).cacheable(*id))
        .fixture(Fixture::new(AUTO, Verb::Source).arg("content", TURTLE))
}

#[test]
fn conforms() {
    let kernel = Kernel::new(Arc::new(ikigai_sniff::space()));
    let report = suite().run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    // The walk saw exactly the endpoints declared above. A third bound without
    // a `pure`/`cacheable` line would be held to a weaker standard; a declared
    // id that binds nothing is a stale list.
    assert_eq!(
        report.endpoints,
        ENDPOINTS.len(),
        "sniff, transrept-auto: {report}"
    );
    assert_eq!(
        report.actions,
        ENDPOINTS.len(),
        "one Source action per endpoint: {report}"
    );
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
}
