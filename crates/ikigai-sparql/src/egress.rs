//! **No SPARQL text reaches the network** (ledger #1083).
//!
//! Two parts of SPARQL fetch over HTTP inside oxigraph: `SERVICE <iri> { … }` (federated
//! query, in a query or in an update's `WHERE`) and `LOAD <iri>` (an update operation). That
//! fetch is oxigraph's own. It never passes through `urn:httpGet`, so the kernel never sees it
//! and no `urn:cap:net:*` is consulted: a caller holding only a SPARQL door would get arbitrary
//! outbound HTTP from inside a query string.
//!
//! This crate takes oxigraph with `default-features = false`, so in its OWN build both fail
//! with "not supported". That is a build property, not a code one. Cargo unifies features
//! across a host's whole graph, and rudof_rdf (behind ikigai-shacl) turns on
//! `oxigraph/http-client` natively, so in ikigai-cli the same `SparqlEvaluator::new()` installs
//! oxigraph's default HTTP service handler and `LOAD` gets a client. Measured by
//! `tests/sparql_service_egress.rs`: before this module, every door sent `SERVICE` and `LOAD` to
//! a local stub and returned its data. The off switch, `without_default_http_service_handler`,
//! is itself behind that feature, so this crate cannot call it, and a crate cannot test
//! whether someone else turned a dependency's feature on.
//!
//! So two layers, both independent of the feature:
//!
//! 1. **Refused before evaluation, typed** ([`check_query`], [`check_update`]). Any `SERVICE`
//!    anywhere in the algebra (an expression's `EXISTS` included, and a service named by a
//!    variable) and any `LOAD` are an [`Error::InvalidArgument`] naming the argument the text
//!    came in. `LOAD` can ONLY be stopped here: oxigraph fetches it with its own client and
//!    offers no hook.
//! 2. **No service handler that fetches** ([`evaluator`]). Every `SparqlEvaluator` this crate
//!    builds installs [`RefuseService`] as its default service handler, which replaces
//!    oxigraph's HTTP one (`with_default_service_handler` is always available, and it is what
//!    switches the HTTP handler off). Should a `SERVICE` ever get past layer 1, it fails
//!    without a request. This is the guarantee; layer 1 is what makes the refusal legible.
//!
//! ★ **Why `InvalidArgument` and not `Denied`.** `Denied` means a different grant would
//! succeed against the same state. None would here: holding `urn:cap:net:*` does not make a
//! `SERVICE` run, because this crate has no gated way to make the call. Answering `Denied`
//! would send a caller looking for a grant that changes nothing. The text uses something this
//! endpoint refuses, which is the argument's fault, so the refusal names the argument, and the
//! detail names `urn:cap:net:*` as the reason and says what works instead.
//!
//! ⚠ `FROM <iri>` / `FROM NAMED <iri>` fetch nothing: oxigraph reads them as names of graphs
//! already in the dataset (ledger #840), so they need no refusal here.

use crate::budget::Cost;
use ikigai_core::{Error, Result};
use oxigraph::model::NamedNode;
use oxigraph::sparql::{DefaultServiceHandler, QuerySolutionIter, SparqlEvaluator};
use oxiri::Iri;
use spargebra::algebra::GraphPattern;
use spargebra::{GraphUpdateOperation, Query, Update};
use std::fmt;

/// The `SparqlEvaluator` every evaluation in this crate starts from: oxigraph's, with
/// [`RefuseService`] as the default service handler in place of oxigraph's HTTP one.
pub(crate) fn evaluator() -> SparqlEvaluator {
    SparqlEvaluator::new().with_default_service_handler(RefuseService)
}

/// Refuse a parsed query that calls `SERVICE` anywhere, naming the argument `arg`.
pub(crate) fn check_query(query: &Query, arg: &str) -> Result<()> {
    match Cost::of_query(query).first_service {
        Some(service) => Err(refuse_service(&service, arg)),
        None => Ok(()),
    }
}

/// Refuse a parsed update that has a `LOAD` operation or calls `SERVICE` in any `WHERE`,
/// naming the argument `arg`. Nothing has been applied: this runs before the transaction opens.
pub(crate) fn check_update(update: &Update, arg: &str) -> Result<()> {
    for operation in &update.operations {
        match operation {
            GraphUpdateOperation::Load { source, .. } => {
                return Err(Error::InvalidArgument {
                    name: arg.to_string(),
                    detail: format!(
                        "this SPARQL update has LOAD {source}, and this endpoint refuses LOAD \
                         before evaluation: the fetch would be oxigraph's own HTTP request, made \
                         outside the kernel where no urn:cap:net:* is consulted (ledger #1083). \
                         No grant changes this. Pull the document in through the kernel instead \
                         (urn:httpGet, which urn:cap:net:* gates) and sink an INSERT DATA built \
                         from it. Nothing was applied"
                    ),
                });
            }
            GraphUpdateOperation::DeleteInsert { pattern, .. } => {
                if let Some(service) = Cost::of_pattern(pattern).first_service {
                    return Err(refuse_service(&service, arg));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn refuse_service(service: &str, arg: &str) -> Error {
    Error::InvalidArgument {
        name: arg.to_string(),
        detail: format!(
            "this SPARQL text calls SERVICE {service}, and this endpoint refuses SERVICE before \
             evaluation: a federated call would be oxigraph's own HTTP request, made outside the \
             kernel where no urn:cap:net:* is consulted (ledger #1083). No grant changes this. \
             Federate through the kernel instead: list the remote graph as a graph= source, \
             which is resolved through the kernel where urn:cap:net:* applies"
        ),
    }
}

/// The default service handler of every evaluator this crate builds: it answers every
/// `SERVICE`, whatever its IRI, with [`ServiceRefused`] and makes no request. See the module
/// doc for why it exists beside [`check_query`].
pub(crate) struct RefuseService;

impl DefaultServiceHandler for RefuseService {
    type Error = ServiceRefused;

    fn handle(
        &self,
        service_name: &NamedNode,
        _pattern: &GraphPattern,
        _base_iri: Option<&Iri<String>>,
    ) -> std::result::Result<QuerySolutionIter<'static>, ServiceRefused> {
        Err(ServiceRefused(service_name.as_str().to_string()))
    }
}

/// What [`RefuseService`] answers: the service IRI it refused to call.
#[derive(Debug)]
pub(crate) struct ServiceRefused(String);

impl fmt::Display for ServiceRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SERVICE <{}> refused: this endpoint makes no network request from SPARQL \
             (no urn:cap:net:* is consulted there; ledger #1083)",
            self.0
        )
    }
}

impl std::error::Error for ServiceRefused {}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::sparql::QueryResults;
    use oxigraph::store::Store;
    use spargebra::SparqlParser;

    /// Layer 2 on its own: an evaluator from [`evaluator`] given a `SERVICE` that layer 1
    /// never saw fails, naming the refusal, instead of calling anything. In the probe build
    /// (`--features http-client-probe`) this is oxigraph WITH its HTTP client, so without
    /// [`RefuseService`] the call reaches that client and fails with the client's own error
    /// (checked by mutation: "The port 9 is not allowed for HTTP(S)"), not with this one. The
    /// request-level proof, against a live local stub, is `tests/sparql_service_egress.rs`.
    #[test]
    fn the_evaluator_refuses_service_without_layer_one() {
        let store = Store::new().unwrap();
        for query in [
            "SELECT * WHERE { SERVICE <http://127.0.0.1:9/sparql> { ?s ?p ?o } }",
            "SELECT * WHERE { BIND(<http://127.0.0.1:9/sparql> AS ?svc) SERVICE ?svc { ?s ?p ?o } }",
        ] {
            let outcome = evaluator()
                .parse_query(query)
                .unwrap()
                .on_store(&store)
                .execute();
            let err = match outcome {
                Err(e) => e.to_string(),
                Ok(QueryResults::Solutions(rows)) => rows
                    .map(|r| r.map(|_| ()))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map(|_| String::new())
                    .unwrap_or_else(|e| e.to_string()),
                Ok(_) => String::new(),
            };
            // A service named by a variable that is never bound is refused by oxigraph
            // itself (no name, nothing to call); a bound one reaches the handler.
            assert!(
                err.contains("refused") || err.contains("unbound"),
                "`{query}` must fail without a request, got {err:?}"
            );
            assert!(!err.contains("connect"), "`{query}`: {err}");
        }
        let err = evaluator()
            .parse_update(
                "INSERT { ?s ?p ?o } WHERE { SERVICE <http://127.0.0.1:9/sparql> { ?s ?p ?o } }",
            )
            .unwrap()
            .on_store(&store)
            .execute()
            .unwrap_err()
            .to_string();
        assert!(err.contains("refused"), "{err}");
        assert_eq!(store.len().unwrap(), 0);
    }

    #[test]
    fn layer_one_finds_service_wherever_the_algebra_puts_it() {
        let refused = |text: &str| {
            check_query(&SparqlParser::new().parse_query(text).unwrap(), "query").unwrap_err()
        };
        for text in [
            "SELECT * WHERE { SERVICE <http://x/> { ?s ?p ?o } }",
            "SELECT * WHERE { SERVICE SILENT <http://x/> { ?s ?p ?o } }",
            "SELECT * WHERE { ?a ?b ?c OPTIONAL { SERVICE ?svc { ?s ?p ?o } } }",
            "SELECT * WHERE { ?a ?b ?c FILTER NOT EXISTS { SERVICE <http://x/> { ?s ?p ?o } } }",
            "SELECT * WHERE { { SELECT ?s WHERE { SERVICE <http://x/> { ?s ?p ?o } } } }",
            "CONSTRUCT { ?s ?p ?o } WHERE { SERVICE <http://x/> { ?s ?p ?o } }",
            "ASK { ?a ?b ?c BIND(EXISTS { SERVICE <http://x/> { ?s ?p ?o } } AS ?e) }",
        ] {
            let err = refused(text);
            assert!(
                matches!(&err, Error::InvalidArgument { name, detail }
                    if name == "query" && detail.contains("urn:cap:net:*")),
                "`{text}`: {err:?}"
            );
        }
        check_query(
            &SparqlParser::new()
                .parse_query("SELECT * FROM <http://x/g> WHERE { ?s ?p ?o }")
                .unwrap(),
            "query",
        )
        .expect("FROM names a graph in the dataset and fetches nothing");
    }

    #[test]
    fn layer_one_refuses_load_and_service_in_an_update() {
        for text in [
            "LOAD <http://x/g>",
            "LOAD SILENT <http://x/g> INTO GRAPH <urn:g>",
            "INSERT DATA { <urn:a> <urn:b> <urn:c> } ; LOAD <http://x/g>",
            "DELETE { ?s ?p ?o } WHERE { SERVICE <http://x/> { ?s ?p ?o } }",
        ] {
            let err = check_update(&SparqlParser::new().parse_update(text).unwrap(), "content")
                .unwrap_err();
            assert!(
                matches!(&err, Error::InvalidArgument { name, .. } if name == "content"),
                "`{text}`: {err:?}"
            );
        }
        check_update(
            &SparqlParser::new()
                .parse_update(
                    "INSERT DATA { <urn:a> <urn:b> <urn:c> } ; CLEAR SILENT GRAPH <urn:g>",
                )
                .unwrap(),
            "update",
        )
        .expect("an update that fetches nothing passes");
    }
}
