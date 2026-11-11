//! Synthetic result-format and binding round trips through the public kernel doors.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_sparql::Store;
use oxigraph::model::{BaseDirection, Literal, NamedNode, Term, Triple};
use oxigraph::sparql::results::{
    QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput,
};
use std::sync::Arc;

fn answer(query: &str, format: QueryResultsFormat, bindings: Option<&str>) -> Vec<u8> {
    answer_from(true, query, format, bindings)
}

fn answer_from(
    shared: bool,
    query: &str,
    format: QueryResultsFormat,
    bindings: Option<&str>,
) -> Vec<u8> {
    let space = if shared {
        ikigai_sparql::space_with_store(Arc::new(Store::new().unwrap()))
    } else {
        ikigai_sparql::space()
    };
    let kernel = Kernel::new(Arc::new(space));
    let mut request = Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap())
        .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
        .with_arg(
            "as",
            ArgRef::Inline(format.media_type().as_bytes().to_vec()),
        );
    if let Some(bindings) = bindings {
        request = request.with_arg("bindings", ArgRef::Inline(bindings.as_bytes().to_vec()));
    }
    block_on(kernel.issue(request, &Capability::root()))
        .unwrap()
        .bytes
}

#[test]
fn directional_result_literal_can_be_bound_back_without_changing_the_answer() {
    for (shared, direction) in [(true, "ltr"), (true, "rtl"), (false, "ltr"), (false, "rtl")] {
        let query =
            format!("SELECT ?label WHERE {{ VALUES ?label {{ \"chat\"@fr--{direction} }} }}");
        let original = answer_from(shared, &query, QueryResultsFormat::Json, None);
        let document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let binding = document["results"]["bindings"][0].to_string();
        let rebound = answer_from(shared, &query, QueryResultsFormat::Json, Some(&binding));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rebound).unwrap(),
            document,
            "a JSON results row must bind back unchanged: {binding}"
        );
    }
}

#[test]
fn lossless_formats_preserve_terms_unbound_columns_and_empty_solutions() {
    let query = r#"SELECT ?é ?missing (BNODE('same') AS ?blank) WHERE {
        VALUES ?é { <urn:ex:a> "" "a\t\r\n\"\\<&>" "chat"@fr
                    "chat"@fr--rtl "007"^^<urn:ex:datatype>
                    <<( <urn:ex:s> <urn:ex:p> "nested"@en )>> UNDEF }
    }"#;
    let iri = |value| NamedNode::new(value).unwrap();
    let expected: [Option<Term>; 8] = [
        Some(iri("urn:ex:a").into()),
        Some(Literal::new_simple_literal("").into()),
        Some(Literal::new_simple_literal("a\t\r\n\"\\<&>").into()),
        Some(
            Literal::new_language_tagged_literal("chat", "fr")
                .unwrap()
                .into(),
        ),
        Some(
            Literal::new_directional_language_tagged_literal("chat", "fr", BaseDirection::Rtl)
                .unwrap()
                .into(),
        ),
        Some(Literal::new_typed_literal("007", iri("urn:ex:datatype")).into()),
        Some(
            Triple::new(
                iri("urn:ex:s"),
                iri("urn:ex:p"),
                Literal::new_language_tagged_literal("nested", "en").unwrap(),
            )
            .into(),
        ),
        None,
    ];
    let expected = expected.map(|term| term.map(|term| term.to_string()));
    for format in [
        QueryResultsFormat::Json,
        QueryResultsFormat::Xml,
        QueryResultsFormat::Tsv,
    ] {
        for query in [
            query,
            "SELECT ?é ?missing WHERE { FILTER(false) }",
            "SELECT ?é ?missing WHERE {}",
        ] {
            let bytes = answer(query, format, None);
            let SliceQueryResultsParserOutput::Solutions(solutions) =
                QueryResultsParser::from_format(format)
                    .for_slice(&bytes)
                    .unwrap()
            else {
                panic!("expected solutions")
            };
            let vars = solutions.variables().to_vec();
            let rows = solutions
                .map(|row| {
                    let row = row.unwrap();
                    vars.iter()
                        .map(|var| {
                            row.get(var).map(|term| {
                                // BNODE labels have result-document scope, not cross-request identity.
                                if matches!(term, oxigraph::model::Term::BlankNode(_)) {
                                    "_:blank".to_string()
                                } else {
                                    term.to_string()
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            if query.starts_with("SELECT ?é ?missing (BNODE") {
                assert_eq!(rows.len(), 8);
                assert!(rows.iter().all(|row| row[1].is_none()));
                for (row, expected) in rows.iter().zip(&expected) {
                    assert_eq!(&row[0], expected, "{format:?}");
                    assert_eq!(row[2].as_deref(), Some("_:blank"));
                }
            } else if query.contains("false") {
                assert!(rows.is_empty());
            } else {
                assert_eq!(rows, vec![vec![None, None]]);
            }
        }
    }
}

#[test]
fn ask_true_and_false_parse_in_every_lossless_format() {
    for format in [
        QueryResultsFormat::Json,
        QueryResultsFormat::Xml,
        QueryResultsFormat::Tsv,
    ] {
        for value in [true, false] {
            let bytes = answer(&format!("ASK {{ FILTER({value}) }}"), format, None);
            let SliceQueryResultsParserOutput::Boolean(actual) =
                QueryResultsParser::from_format(format)
                    .for_slice(&bytes)
                    .unwrap()
            else {
                panic!("expected boolean")
            };
            assert_eq!(actual, value);
        }
    }
}

#[test]
fn csv_is_lexical_while_tsv_preserves_term_syntax() {
    let query = r#"SELECT ?iri ?plain ?lang ?typed ?empty ?unbound WHERE {
        VALUES (?iri ?plain ?lang ?typed ?empty) {
            (<urn:ex:a> "a,\"b\"\nline" "chat"@fr "007"^^<urn:ex:datatype> "")
        }
    }"#;
    assert_eq!(
        String::from_utf8(answer(query, QueryResultsFormat::Csv, None)).unwrap(),
        "iri,plain,lang,typed,empty,unbound\r\nurn:ex:a,\"a,\"\"b\"\"\nline\",chat,007,,\r\n"
    );
    let tsv = String::from_utf8(answer(query, QueryResultsFormat::Tsv, None)).unwrap();
    assert!(tsv.contains("<urn:ex:a>"));
    assert!(tsv.contains("\"chat\"@fr"));
    assert!(tsv.contains("\"007\"^^<urn:ex:datatype>"));
    assert!(tsv.ends_with("\t\"\"\t\n"));
    for value in [true, false] {
        assert_eq!(
            String::from_utf8(answer(
                &format!("ASK {{ FILTER({value}) }}"),
                QueryResultsFormat::Csv,
                None
            ))
            .unwrap()
            .trim(),
            value.to_string()
        );
    }
}
