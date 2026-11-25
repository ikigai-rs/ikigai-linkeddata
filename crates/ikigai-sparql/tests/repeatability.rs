use ikigai_rdf::algebra::{repeatability_reasons, RepeatabilityReason};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use spargebra::SparqlParser;

#[test]
fn uuid_construct_mints_different_rdf_terms() {
    let query = SparqlParser::new()
        .parse_query("CONSTRUCT { ?id <urn:p> <urn:o> } WHERE { BIND(UUID() AS ?id) }")
        .unwrap();
    assert_eq!(
        repeatability_reasons(&query).unwrap(),
        [RepeatabilityReason::Uuid].into()
    );
    let store = Store::new().unwrap();
    let evaluate = || {
        let QueryResults::Graph(mut triples) = SparqlEvaluator::new()
            .for_query(query.clone())
            .on_store(&store)
            .execute()
            .unwrap()
        else {
            panic!("expected a CONSTRUCT graph");
        };
        let triple = triples.next().unwrap().unwrap();
        assert!(triples.next().is_none());
        triple.subject
    };
    assert_ne!(evaluate(), evaluate(), "UUID() creates fresh RDF terms");
}

#[test]
fn lateral_and_rdf_12_use_the_same_parsed_query_in_the_evaluator() {
    let query = SparqlParser::new()
        .parse_query(
            "CONSTRUCT { ?s <urn:quote> <<( _:fresh <urn:p> ?x )>> }
             WHERE {
                 VALUES ?s { <urn:s> }
                 LATERAL { BIND(UUID() AS ?x) }
             }",
        )
        .unwrap();
    assert_eq!(
        repeatability_reasons(&query).unwrap(),
        [
            RepeatabilityReason::Uuid,
            RepeatabilityReason::ConstructTemplateBlankNode,
        ]
        .into()
    );
    let store = Store::new().unwrap();
    let QueryResults::Graph(mut triples) = SparqlEvaluator::new()
        .for_query(query)
        .on_store(&store)
        .execute()
        .unwrap()
    else {
        panic!("expected a CONSTRUCT graph");
    };
    let triple = triples.next().unwrap().unwrap();
    let oxigraph::model::Term::Triple(quoted) = triple.object else {
        panic!("expected an RDF 1.2 triple term");
    };
    assert!(quoted.subject.is_blank_node());
    assert!(triples.next().is_none());
}
