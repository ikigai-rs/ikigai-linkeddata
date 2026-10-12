//! A shared store never holds a triple term nested deeper than the bound (ledger #1326).
//!
//! Every door that READS caller RDF refuses a triple term nested deeper than
//! [`MAX_TURTLE_NESTING`] (64) before parsing it, and caller SPARQL is refused past
//! [`crate::limits::MAX_SPARQL_NESTING`]. Neither bounds a term an UPDATE builds: `INSERT
//! { <urn:a> <urn:p> ?n } WHERE { <urn:a> <urn:p> ?t BIND(TRIPLE(<urn:s>, <urn:p>, ?t) AS ?n) }`
//! wraps whatever the store already holds in one more level, so a caller holding
//! [`crate::CAP_UPDATE`] grows a term without bound, a request at a time, each request well
//! inside every bound. oxigraph encodes, decodes and serializes a triple term recursively, so
//! past a few thousand levels any read of it, and the next update that matches it, overflows
//! its thread and aborts the whole host. Measured (`tests/sparql_structural_depth.rs`, a debug
//! build): 56 levels an update, the update ABORTED the process at 8,400 levels, though it runs
//! on a 16 MiB thread, and every later query form at 16,800.
//!
//! So the store keeps an invariant: **no stored triple term nests deeper than
//! [`MAX_TURTLE_NESTING`]**, the depth the read doors admit, so whatever a store holds can be
//! read back the way it was written. An update that could build a triple term
//! ([`may_construct_triple_term`]) runs in a transaction, and before the transaction commits,
//! [`check_transaction`] asks the store, as the update left it, whether any term is deeper;
//! one that is is refused and NOTHING is applied. One update cannot itself reach a depth that
//! aborts: its growth is bounded by its text, and it runs on a thread sized for its text.
//!
//! ⚠ **The check reads the whole store**, every graph, so an update that mentions a triple
//! term costs a scan of the store besides its own work; an update that cannot build one
//! (most: `INSERT DATA` of plain triples, `DELETE WHERE`, `CLEAR`) is not checked and costs
//! nothing extra. The detection is lexical and over-approximate on purpose: a false positive
//! costs one scan, and a false negative would be the abort.
//!
//! ⚠ **This guards writes through `urn:sparql:update` only.** A host that writes the same
//! store another way (its own `Store::update`, a bulk load, ikigai-store's endpoints) keeps
//! the invariant itself, or does not have it; and a store that already holds a deeper term,
//! from before this check or from such a write, still aborts whatever reads it.

use ikigai_core::{Error, Result};
use ikigai_rdf::MAX_TURTLE_NESTING;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Transaction;

/// Whether SPARQL update `text` could build an RDF 1.2 triple term, judged from the text
/// alone, erring toward yes.
///
/// The constructors are the `<<( … )>>` term, the `<< … >>` reified triple, an annotation
/// block `{| … |}`, a reifier `~`, and the `TRIPLE(…)` function, any of which can wrap a term
/// bound from the store. So: any `<<`, `{|` or `~`, the word `triple` in any case (and so in an
/// IRI or a string too), and any `\u`/`\U` escape, since spargebra's
/// `standard-unicode-escaping` feature rewrites those before tokenizing (see
/// [`crate::limits`]) and a sibling crate could turn it on.
pub(crate) fn may_construct_triple_term(text: &str) -> bool {
    text.contains("<<")
        || text.contains("{|")
        || text.contains('~')
        || text.contains("\\u")
        || text.contains("\\U")
        || text.to_ascii_lowercase().contains("triple")
}

/// The query that finds a term deeper than [`MAX_TURTLE_NESTING`]: in RDF 1.2 a triple term
/// can only be an OBJECT (never a subject, predicate or graph name, and a triple term's own
/// subject cannot be one either), so a term nests `d` deep exactly when `OBJECT` applied
/// `d - 1` times still yields a triple, and it is deeper than the bound when `OBJECT` applied
/// [`MAX_TURTLE_NESTING`] times does. Over the default graph and every named graph.
fn deeper_than_the_bound() -> String {
    format!(
        "ASK {{ {{ ?s ?p ?o }} UNION {{ GRAPH ?g {{ ?s ?p ?o }} }} FILTER(isTRIPLE({}?o{})) }}",
        "OBJECT(".repeat(MAX_TURTLE_NESTING),
        ")".repeat(MAX_TURTLE_NESTING)
    )
}

/// Refuse, on argument `arg`, a transaction an update has written that leaves a triple term
/// nested deeper than [`MAX_TURTLE_NESTING`] anywhere in the store. `evaluator` carries the
/// update's cancellation token, so the scan answers to the same time budget.
///
/// A build without SPARQL 1.2 cannot parse the check (`isTRIPLE` and `OBJECT` are 1.2), and
/// cannot build a triple term either, so there the check passes. With 1.2 on, as in every
/// test of this crate, a unit test pins that the check parses.
pub(crate) fn check_transaction(
    transaction: &Transaction<'_>,
    evaluator: SparqlEvaluator,
    arg: &str,
) -> Result<()> {
    let Ok(prepared) = evaluator.parse_query(&deeper_than_the_bound()) else {
        return Ok(());
    };
    match prepared.on_transaction(transaction).execute() {
        Ok(QueryResults::Boolean(false)) => Ok(()),
        Ok(QueryResults::Boolean(true)) => Err(Error::InvalidArgument {
            name: arg.to_string(),
            detail: format!(
                "this update would leave the store holding an RDF 1.2 triple term nested deeper \
                 than {MAX_TURTLE_NESTING} (MAX_TURTLE_NESTING), so nothing was applied: the \
                 store encodes, decodes and serializes a triple term recursively, once per \
                 level, so a deep enough one aborts the whole host on whatever request reads \
                 it, and a term that grows by an update at a time has no other limit. Nest \
                 terms less deeply, or reify instead (`<< … >>` names a triple with a fresh \
                 reifier rather than nesting it)"
            ),
        }),
        Ok(_) => Err(Error::Endpoint(
            "the stored triple-term depth check answered with something other than a boolean"
                .to_string(),
        )),
        Err(e) => Err(Error::Endpoint(format!(
            "checking the depth of stored triple terms: {e}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_constructor_spelling_is_detected() {
        for text in [
            "INSERT { <urn:a> <urn:p> <<( <urn:s> <urn:p> ?t )>> } WHERE { <urn:a> <urn:p> ?t }",
            "INSERT { <urn:a> <urn:p> << <urn:s> <urn:p> ?t >> } WHERE { <urn:a> <urn:p> ?t }",
            "INSERT { <urn:s> <urn:p> ?t {| <urn:q> <urn:r> |} } WHERE { <urn:a> <urn:p> ?t }",
            "INSERT { <urn:s> <urn:p> ?t ~ <urn:r> } WHERE { <urn:a> <urn:p> ?t }",
            "INSERT { <urn:a> <urn:p> ?n } WHERE { <urn:a> <urn:p> ?t \
             BIND(tRiPlE(<urn:s>, <urn:p>, ?t) AS ?n) }",
            "INSERT { <urn:a> <urn:p> ?n } WHERE { <urn:a> <urn:p> ?t \
             BIND(\\u0054RIPLE(<urn:s>, <urn:p>, ?t) AS ?n) }",
        ] {
            assert!(may_construct_triple_term(text), "{text}");
        }
    }

    #[test]
    fn plain_updates_are_not_checked() {
        for text in [
            "INSERT DATA { <urn:s> <urn:p> \"o\" }",
            "DELETE WHERE { ?s ?p ?o }",
            "CLEAR ALL",
            "INSERT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o FILTER(?o < 3) }",
        ] {
            assert!(!may_construct_triple_term(text), "{text}");
        }
    }

    #[test]
    fn the_check_parses_where_sparql_1_2_is_on() {
        // RDF 1.2 is on in this crate's tests (the `oxigraph/rdf-12` dev-dependency), as in
        // the CLI's build. If the check stopped parsing there, `check_transaction` would pass
        // everything silently, so pin it. On a thread of its own: 64 nested function calls take
        // ~4 MiB to parse in a debug build (in the endpoint, the update's thread holds them).
        let parsed = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                oxigraph::sparql::SparqlEvaluator::new()
                    .parse_query(&deeper_than_the_bound())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(parsed, Ok(()));
    }
}
