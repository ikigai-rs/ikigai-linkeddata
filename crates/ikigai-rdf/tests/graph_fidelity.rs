#![cfg(feature = "transrept")]

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Representation, Request, Verb};
use oxrdf::Dataset;
use oxrdfio::{RdfFormat, RdfParser};
use std::sync::Arc;

fn transrept(input: &[u8], target: &str) -> ikigai_core::Result<Representation> {
    let kernel = Kernel::new(Arc::new(ikigai_rdf::space()));
    let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
        .with_arg("content", ArgRef::Inline(input.to_vec()))
        .with_arg("as", ArgRef::Inline(target.as_bytes().to_vec()));
    block_on(kernel.issue(request, &Capability::root()))
}

fn dataset(input: &[u8], media: &str) -> Dataset {
    let mut result = Dataset::default();
    for quad in RdfParser::from_format(RdfFormat::from_media_type(media).unwrap()).for_slice(input)
    {
        result.insert(&quad.unwrap());
    }
    result.canonicalize(oxrdf::dataset::CanonicalizationAlgorithm::Unstable);
    result
}

const FORMATS: [&str; 6] = [
    "text/turtle",
    "application/n-triples",
    "application/n-quads",
    "application/trig",
    "application/rdf+xml",
    "application/ld+json",
];

#[test]
fn every_format_pair_preserves_literal_terms_and_blank_node_identity() {
    let input = format!(
        r#"@prefix ex: <http://example.org/> .
ex:s ex:p _:a, _:b ; ex:integer "01"^^<http://www.w3.org/2001/XMLSchema#integer> ;
 ex:decimal "01.00"^^<http://www.w3.org/2001/XMLSchema#decimal> ;
 ex:boolean "1"^^<http://www.w3.org/2001/XMLSchema#boolean> ;
 ex:double "-0.0E0"^^<http://www.w3.org/2001/XMLSchema#double> ;
 ex:invalid "not-an-integer"^^<http://www.w3.org/2001/XMLSchema#integer> ;
 ex:date "2026-01-02T03:04:05+00:00"^^<http://www.w3.org/2001/XMLSchema#dateTime> ;
 ex:xml "<p>é &amp; 🦀</p>"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral> ;
 ex:custom "opaque"^^ex:datatype ; ex:lang "café 🦀"@en-US ;
 ex:escape "\"\\\n\t\u00E9\U0001F980" ; ex:long "{}" ;
 ex:iri <http://example.org/a?x=1&y=é> .
_:a ex:link _:b . _:b ex:link _:a .
"#,
        "long🦀".repeat(10_000)
    );
    let expected = dataset(input.as_bytes(), "text/turtle");
    for from in FORMATS {
        let source = transrept(input.as_bytes(), from).unwrap();
        for to in FORMATS {
            let result =
                transrept(&source.bytes, to).unwrap_or_else(|e| panic!("{from} -> {to}: {e}"));
            assert_eq!(result.repr_type.media_type, to);
            assert_eq!(dataset(&result.bytes, to), expected, "{from} -> {to}");
            assert_eq!(
                transrept(&source.bytes, to).unwrap().bytes,
                result.bytes,
                "determinism: {from} -> {to}"
            );
        }
    }
}

#[test]
fn named_graphs_round_trip_between_dataset_formats() {
    let input = br#"{"@id":"urn:graph:g","@graph":[{"@id":"urn:subject:s","urn:predicate:p":{"@id":"_:a"}},{"@id":"_:a","urn:predicate:p":{"@value":"01","@type":"http://www.w3.org/2001/XMLSchema#integer"}}]}"#;
    let expected = dataset(input, "application/ld+json");
    assert_eq!(expected.len(), 2);
    for from in [
        "application/n-quads",
        "application/trig",
        "application/ld+json",
    ] {
        let source = transrept(input, from).unwrap();
        for to in [
            "application/n-quads",
            "application/trig",
            "application/ld+json",
        ] {
            let result =
                transrept(&source.bytes, to).unwrap_or_else(|e| panic!("{from} -> {to}: {e}"));
            assert_eq!(dataset(&result.bytes, to), expected, "{from} -> {to}");
        }
    }
}

#[test]
fn aliases_answer_canonical_media_types() {
    for (alias, expected) in [
        ("ttl", "text/turtle"),
        ("nt", "application/n-triples"),
        ("nq", "application/n-quads"),
        ("trig", "application/trig"),
        ("xml", "application/rdf+xml"),
        ("jsonld", "application/ld+json"),
    ] {
        let result = transrept(b"<urn:test:s> <urn:test:p> <urn:test:o> .", alias).unwrap();
        assert_eq!(result.repr_type.media_type, expected, "{alias}");
    }
}

#[test]
fn named_graphs_are_refused_by_triple_formats() {
    let input = br#"{"@id":"urn:graph:g","@graph":[{"@id":"urn:subject:s","urn:predicate:p":{"@id":"urn:object:o"}}]}"#;
    for to in [
        "text/turtle",
        "application/n-triples",
        "application/rdf+xml",
    ] {
        assert!(
            transrept(input, to).is_err(),
            "{to} must not drop the graph name"
        );
    }
}

#[cfg(feature = "rdf-12")]
#[test]
fn rdf12_triple_terms_reifiers_and_annotations_survive_supported_formats() {
    let input = br#"@prefix ex: <http://example.org/> .
_:a ex:p ex:o .
ex:s ex:term <<( _:a ex:p <<( _:a ex:q "01"^^ex:type )>> )>> .
ex:s ex:p ex:o ~ ex:reifier {| ex:certainty "high" |} .
"#;
    let expected = dataset(input, "text/turtle");
    for from in [
        "text/turtle",
        "application/n-triples",
        "application/n-quads",
        "application/trig",
        "application/rdf+xml",
    ] {
        let source = transrept(input, from).unwrap();
        for to in [
            "text/turtle",
            "application/n-triples",
            "application/n-quads",
            "application/trig",
            "application/rdf+xml",
        ] {
            let result = transrept(&source.bytes, to).unwrap();
            assert_eq!(dataset(&result.bytes, to), expected, "{from} -> {to}");
        }
        assert!(
            transrept(&source.bytes, "application/ld+json").is_err(),
            "JSON-LD must refuse triple terms"
        );
    }
}

#[cfg(feature = "rdf-12")]
#[test]
fn directional_literals_survive_every_format_pair() {
    let input =
        "@prefix ex: <http://example.org/> . ex:s ex:p \"مرحبا\"@ar--rtl, \"hello\"@en--ltr .";
    let expected = dataset(input.as_bytes(), "text/turtle");
    for from in FORMATS {
        let source = transrept(input.as_bytes(), from).unwrap();
        for to in FORMATS {
            let result = transrept(&source.bytes, to).unwrap();
            assert_eq!(dataset(&result.bytes, to), expected, "{from} -> {to}");
        }
    }
}

#[test]
fn turtle_and_trig_openings_are_not_mistaken_for_jsonld() {
    for input in [
        "[] <urn:test:p> <urn:test:o> .",
        "[ <urn:test:p> <urn:test:o> ] <urn:test:q> <urn:test:r> .",
        "{ <urn:test:s> <urn:test:p> <urn:test:o> . }",
    ] {
        let expected = dataset(input.as_bytes(), "application/trig");
        let result = transrept(input.as_bytes(), "application/n-quads").unwrap();
        assert_eq!(dataset(&result.bytes, "application/n-quads"), expected);
    }
}

#[test]
fn escaped_iri_subject_is_not_mistaken_for_xml() {
    let input = br#"<http://example.org/\u00E9> <urn:test:p> <urn:test:o> ."#;
    let result = transrept(input, "application/n-triples").unwrap();
    assert_eq!(
        dataset(&result.bytes, "application/n-triples"),
        dataset(input, "text/turtle")
    );
}

#[test]
fn in_document_bases_resolve_relative_iris_and_prefixes_are_expanded() {
    let input = b"@base <http://example.org/a/> . @prefix ex: <../vocab#> . <s> ex:p <../o> .";
    let expected = dataset(
        b"<http://example.org/a/s> <http://example.org/vocab#p> <http://example.org/o> .",
        "text/turtle",
    );
    for to in FORMATS {
        let result = transrept(input, to).unwrap();
        assert_eq!(dataset(&result.bytes, to), expected, "{to}");
    }
    assert!(transrept(b"<relative> <urn:test:p> <urn:test:o> .", "text/turtle").is_err());
}

#[test]
fn unknown_output_media_type_is_refused() {
    assert!(transrept(
        b"<urn:test:s> <urn:test:p> <urn:test:o> .",
        "application/x-unknown"
    )
    .is_err());
}

fn operation(iri: &str, content: &str, with: &str, mode: &str) -> Representation {
    let kernel = Kernel::new(Arc::new(ikigai_rdf::space()));
    let request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
        .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()))
        .with_arg("with", ArgRef::Inline(with.as_bytes().to_vec()))
        .with_arg("mode", ArgRef::Inline(mode.as_bytes().to_vec()));
    block_on(kernel.issue(request, &Capability::root())).unwrap()
}

#[test]
fn union_and_diff_are_sets_and_reconstruct_a_skolemized_graph() {
    let a = "<urn:test:a> <urn:test:p> <urn:test:o> .\n<urn:test:b> <urn:test:p> <urn:test:o> .";
    let b = "<urn:test:b> <urn:test:p> <urn:test:o> .\n<urn:test:c> <urn:test:p> <urn:test:o> .";
    let union = operation("urn:rdf:union", a, b, "added");
    assert_eq!(dataset(&union.bytes, "text/turtle").len(), 3);
    let added = operation("urn:rdf:diff", a, b, "added");
    let removed = operation("urn:rdf:diff", a, b, "removed");
    assert_eq!(
        dataset(&added.bytes, "text/turtle"),
        dataset(a.lines().next().unwrap().as_bytes(), "text/turtle")
    );
    assert_eq!(
        dataset(&removed.bytes, "text/turtle"),
        dataset(b.lines().nth(1).unwrap().as_bytes(), "text/turtle")
    );
    let intersection = operation(
        "urn:rdf:diff",
        b,
        std::str::from_utf8(&removed.bytes).unwrap(),
        "added",
    );
    let restored = operation(
        "urn:rdf:union",
        std::str::from_utf8(&intersection.bytes).unwrap(),
        std::str::from_utf8(&added.bytes).unwrap(),
        "added",
    );
    assert_eq!(
        dataset(&restored.bytes, "text/turtle"),
        dataset(a.as_bytes(), "text/turtle")
    );
}

#[test]
fn set_operations_share_explicit_labels_but_do_not_compare_isomorphism() {
    let a = "@prefix ex: <urn:test:> . _:a ex:p ex:o .";
    let same = "@prefix ex: <urn:test:> . _:a ex:p ex:o .";
    let isomorphic = "@prefix ex: <urn:test:> . _:b ex:p ex:o .";
    assert_eq!(
        dataset(
            &operation("urn:rdf:union", a, same, "added").bytes,
            "text/turtle"
        )
        .len(),
        1
    );
    assert_eq!(
        dataset(
            &operation("urn:rdf:diff", a, same, "added").bytes,
            "text/turtle"
        )
        .len(),
        0
    );
    assert_eq!(
        dataset(
            &operation("urn:rdf:union", a, isomorphic, "added").bytes,
            "text/turtle"
        )
        .len(),
        2
    );
    assert_eq!(
        dataset(
            &operation("urn:rdf:diff", a, isomorphic, "added").bytes,
            "text/turtle"
        )
        .len(),
        1
    );
    let anonymous = "@prefix ex: <urn:test:> . [] ex:p ex:o .";
    assert_eq!(
        dataset(
            &operation("urn:rdf:union", anonymous, anonymous, "added").bytes,
            "text/turtle"
        )
        .len(),
        2
    );
}

#[test]
fn with_accepts_turtle_blank_node_and_sparql_prefix_openings() {
    let a = "<urn:test:s> <urn:test:p> <urn:test:o> .";
    for with in [
        "_:a <urn:test:p> <urn:test:o> .",
        "[] <urn:test:p> <urn:test:o> .",
        "PREFIX ex: <urn:test:> ex:a ex:p ex:o .",
    ] {
        assert_eq!(
            dataset(
                &operation("urn:rdf:union", a, with, "added").bytes,
                "text/turtle"
            )
            .len(),
            2,
            "{with}"
        );
    }
}

#[test]
fn long_subject_iris_are_accepted() {
    let long = format!(
        "<http://example.org/{}> <urn:test:p> <urn:test:o> .",
        "a".repeat(80_000)
    );
    let result = transrept(long.as_bytes(), "application/n-quads").unwrap();
    assert_eq!(
        dataset(&result.bytes, "application/n-quads"),
        dataset(long.as_bytes(), "application/trig")
    );
}

#[test]
fn empty_initial_trig_graphs_are_accepted() {
    let input = b"{} <urn:test:g> { <urn:test:s> <urn:test:p> <urn:test:o> . }";
    let result = transrept(input, "application/n-quads").unwrap();
    assert_eq!(
        dataset(&result.bytes, "application/n-quads"),
        dataset(input, "application/trig")
    );
}

#[test]
fn src_uses_the_retrieval_iri_as_the_document_base() {
    use ikigai_core::{EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, ReprType, Space};
    for (media, input) in [
        (
            "text/turtle",
            "@prefix ex: <http://example.org/> . <s> ex:p <o> .",
        ),
        (
            "application/rdf+xml",
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ex="http://example.org/"><rdf:Description rdf:about="s"><ex:p rdf:resource="o"/></rdf:Description></rdf:RDF>"#,
        ),
        (
            "application/ld+json",
            r#"{"@id":"s","http://example.org/p":{"@id":"o"}}"#,
        ),
    ] {
        // The HTTP module is a synthetic source; this test makes no network request.
        let source = EndpointSpace::new().bind(
            Exact::new("urn:http:get"),
            FnEndpoint::new("document", move |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new(media),
                    input.as_bytes().to_vec(),
                ))
            }),
        );
        let kernel = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(ikigai_rdf::space()) as Arc<dyn Space>,
            Arc::new(source),
        ])));
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg(
                "src",
                ArgRef::Inline(b"https://example.org/docs/source".to_vec()),
            )
            .with_arg("as", ArgRef::Inline(b"application/n-quads".to_vec()));
        let result = block_on(kernel.issue(request, &Capability::root())).unwrap();
        let expected = dataset(
            b"<https://example.org/docs/s> <http://example.org/p> <https://example.org/docs/o> .",
            "text/turtle",
        );
        assert_eq!(
            dataset(&result.bytes, "application/n-quads"),
            expected,
            "{media}"
        );
    }
}

#[test]
fn default_iri_named_and_blank_named_graphs_remain_distinct() {
    let input = b"<urn:test:s> <urn:test:p> _:a .\n<urn:test:s> <urn:test:p> _:a <urn:test:g> .\n<urn:test:s> <urn:test:p> _:a _:a .\n_:a <urn:test:q> \"v\" _:a .";
    let expected = dataset(input, "application/n-quads");
    assert_eq!(expected.len(), 4);
    for from in [
        "application/n-quads",
        "application/trig",
        "application/ld+json",
    ] {
        let source = transrept(input, from).unwrap();
        for to in [
            "application/n-quads",
            "application/trig",
            "application/ld+json",
        ] {
            let result = transrept(&source.bytes, to).unwrap();
            assert_eq!(dataset(&result.bytes, to), expected, "{from} -> {to}");
            assert_eq!(transrept(&source.bytes, to).unwrap().bytes, result.bytes);
        }
        for to in [
            "text/turtle",
            "application/n-triples",
            "application/rdf+xml",
        ] {
            assert!(
                transrept(&source.bytes, to).is_err(),
                "{from} -> {to} must refuse graph names"
            );
        }
    }
}

#[test]
fn mixing_incompatible_turtle_and_nquads_syntax_is_refused() {
    for input in [
        "@prefix ex: <urn:test:> . ex:s ex:p ex:o . <urn:test:s> <urn:test:p> <urn:test:o> <urn:test:g> .",
        "<urn:test:s> <urn:test:p> <urn:test:o> <urn:test:g> . @prefix ex: <urn:test:> . ex:s ex:p ex:o .",
    ] {
        assert!(transrept(input.as_bytes(), "application/n-quads").is_err());
    }
}

#[test]
fn unicode_escapes_in_iri_schemes_and_separators_are_recognized() {
    for input in [
        br"<h\u0074tp://example.org/s> <urn:test:p> <urn:test:o> .".as_slice(),
        br"<http\u003A//example.org/s> <urn:test:p> <urn:test:o> .".as_slice(),
        br"<urn:\u0078> <urn:test:p> <urn:test:o> .".as_slice(),
    ] {
        let result = transrept(input, "application/n-triples").unwrap();
        assert_eq!(
            dataset(&result.bytes, "application/n-triples"),
            dataset(input, "text/turtle")
        );
    }
}
