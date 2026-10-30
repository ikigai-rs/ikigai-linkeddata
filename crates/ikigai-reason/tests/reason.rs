mod common;
use common::*;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Exact};
use ikigai_reason::{space, space_with_limits, Limits};
use std::time::Instant;

fn entails(input: &str, expected: &str) {
    let result = closure(&with_graph(input));
    assert!(
        triples(input).is_subset(&result),
        "closure must preserve all input"
    );
    assert!(
        triples(expected).is_subset(&result),
        "missing {expected}: {result:?}"
    );
}

#[test]
fn subclass_chain() {
    entails(
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C . ex:x a ex:A .",
        "ex:x a ex:C . ex:A rdfs:subClassOf ex:C .",
    );
}
#[test]
fn subproperty() {
    entails(
        "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .",
        "ex:x ex:q ex:y .",
    );
}
#[test]
fn domain_and_range() {
    entails(
        "ex:p rdfs:domain ex:A ; rdfs:range ex:B . ex:x ex:p ex:y .",
        "ex:x a ex:A . ex:y a ex:B .",
    );
}
#[test]
fn inverse() {
    entails(
        "ex:p owl:inverseOf ex:q . ex:x ex:p ex:y .",
        "ex:y ex:q ex:x .",
    );
}
#[test]
fn equivalent_classes_and_same_as() {
    entails(
        "ex:A owl:equivalentClass ex:B . ex:x a ex:A ; owl:sameAs ex:y .",
        "ex:x a ex:B . ex:y a ex:B .",
    );
}
#[test]
fn skos_broader_transitive() {
    entails("skos:broader rdfs:subPropertyOf skos:broaderTransitive . skos:broaderTransitive a owl:TransitiveProperty . ex:a skos:broader ex:b . ex:b skos:broader ex:c .", "ex:a skos:broaderTransitive ex:c .");
}
#[test]
fn inferred_is_exactly_closure_minus_input() {
    let input = "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .";
    let k = with_graph(input);
    let all = closure(&k);
    let inferred = parse(
        &issue(&k, "inferred", &[("graph", "urn:test:graph")])
            .unwrap()
            .bytes,
    );
    assert_eq!(inferred, all.difference(&triples(input)).cloned().collect());
    assert!(triples("ex:x ex:q ex:y .").is_subset(&inferred));
}
#[test]
fn merges_graphs_and_scopes_blank_nodes_to_each_source() {
    let k = kernel(
        space()
            .bind(
                Exact::new("urn:test:data"),
                fixture("_:a ex:p ex:y ; ex:child [ ex:q \"anonymous\" ] ."),
            )
            .bind(
                Exact::new("urn:test:vocab"),
                fixture("ex:p rdfs:subPropertyOf ex:q . _:a ex:z ex:w ."),
            ),
    );
    let args = [("graph", "urn:test:data, urn:test:vocab urn:test:data")];
    let out = issue(&k, "closure", &args).unwrap();
    let all = parse(&out.bytes);
    let p = triples("ex:x ex:p ex:y .")
        .into_iter()
        .next()
        .unwrap()
        .predicate;
    let z = triples("ex:x ex:z ex:w .")
        .into_iter()
        .next()
        .unwrap()
        .predicate;
    let subject = |predicate| {
        all.iter()
            .find(|t| t.predicate == predicate)
            .unwrap()
            .subject
            .clone()
    };
    assert_ne!(
        subject(p),
        subject(z),
        "the same blank label in different documents names different nodes"
    );
    k.cut("urn:test:data");
    assert_eq!(
        issue(&k, "closure", &args).unwrap().bytes,
        out.bytes,
        "recomputing anonymous nodes is byte deterministic"
    );
}
#[test]
fn input_bound_refuses_before_reasoning_and_counts_across_sources() {
    let k = kernel(
        space_with_limits(Limits {
            max_input_triples: 1,
            max_output_triples: 100_000,
        })
        .bind(Exact::new("urn:test:one"), fixture("ex:x ex:p ex:y ."))
        .bind(Exact::new("urn:test:two"), fixture("ex:a ex:p ex:b .")),
    );
    assert!(issue(&k, "closure", &[("graph", "urn:test:one")]).is_ok());
    for form in ["closure", "inferred"] {
        assert!(matches!(
            issue(&k, form, &[("graph", "urn:test:one urn:test:two")]),
            Err(Error::OverBudget { .. })
        ));
    }
}
#[test]
fn output_bound_applies_to_whole_closure_even_on_inferred() {
    let input = "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .";
    let size = closure(&with_graph(input)).len();
    for limit in [size, size - 1] {
        let k = kernel(
            space_with_limits(Limits {
                max_input_triples: 2,
                max_output_triples: limit,
            })
            .bind(Exact::new("urn:test:graph"), fixture(input)),
        );
        for form in ["closure", "inferred"] {
            let result = issue(&k, form, &[("graph", "urn:test:graph")]);
            if limit == size {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert!(
                    matches!(result, Err(Error::OverBudget { .. })),
                    "{result:?}"
                );
            }
        }
    }
}
#[test]
fn typed_errors() {
    let k = kernel(
        space()
            .bind(
                Exact::new("urn:test:text"),
                Graph("not RDF".into(), "text/plain"),
            )
            .bind(
                Exact::new("urn:test:broken"),
                Graph("not RDF".into(), "text/turtle"),
            ),
    );
    assert!(matches!(
        issue(&k, "closure", &[]),
        Err(Error::MissingArgument(_))
    ));
    for graph in ["", "urn:test:text", "urn:test:broken", "not-an-iri"] {
        assert!(
            matches!(issue(&k, "closure", &[("graph", graph)]), Err(Error::InvalidArgument { name, .. }) if name == "graph")
        );
    }
    assert!(matches!(
        issue(&k, "closure", &[("graph", "urn:test:absent")]),
        Err(Error::NotFound(_))
    ));
    for (name, value) in [("profile", "rdfs"), ("as", "text/plain")] {
        assert!(
            matches!(issue(&k, "closure", &[("graph", "urn:test:absent"), (name, value)]), Err(Error::InvalidArgument { name: n, .. }) if n == name)
        );
    }
    let bad = request("closure", &[("graph", "urn:test:absent")])
        .with_arg("profile", ArgRef::Inline(vec![255]));
    assert!(
        matches!(block_on(k.issue(bad, &Capability::root())), Err(Error::InvalidArgument { name, .. }) if name == "profile")
    );
}
#[test]
fn cut_recomputes_only_the_dependent_closure() {
    use async_trait::async_trait;
    use ikigai_core::{Description, Endpoint, Invocation, ReprType, Representation, Result, Verb};
    use std::sync::{Arc, RwLock};
    struct Mutable(Arc<RwLock<String>>, &'static str);
    #[async_trait]
    impl Endpoint for Mutable {
        async fn invoke(&self, _: &Invocation<'_>) -> Result<Representation> {
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                self.0.read().unwrap().as_bytes().to_vec(),
            )
            .cacheable()
            .depends_on(self.1))
        }
        fn name(&self) -> &str {
            "mutable"
        }
        fn describe(&self) -> Description {
            Description::new("mutable").verb(Verb::Source)
        }
    }
    let content = Arc::new(RwLock::new(format!(
        "{PREFIXES}ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y ."
    )));
    let k = kernel(
        space()
            .bind(
                Exact::new("urn:test:changed"),
                Mutable(content.clone(), "urn:test:changed-thread"),
            )
            .bind(
                Exact::new("urn:test:untouched"),
                fixture("ex:a ex:p ex:b ."),
            ),
    );
    for form in ["closure", "inferred"] {
        let changed = request(form, &[("graph", "urn:test:changed")]);
        let untouched = request(form, &[("graph", "urn:test:untouched")]);
        let read = |req| block_on(k.issue(req, &Capability::root())).unwrap();
        let before = read(changed.clone());
        let other = read(untouched.clone());
        assert!(k.is_cached(&changed, &Capability::root()));
        assert!(k.is_cached(&untouched, &Capability::root()));
        content.write().unwrap().push_str(" ex:y ex:p ex:z .");
        k.cut("urn:test:changed-thread");
        assert!(
            !k.is_cached(&changed, &Capability::root()),
            "dependent result must be invalidated"
        );
        assert!(
            k.is_cached(&untouched, &Capability::root()),
            "unrelated result remains cached"
        );
        let after = read(changed.clone());
        assert_ne!(before.bytes, after.bytes);
        assert!(triples("ex:y ex:q ex:z .").is_subset(&parse(&after.bytes)));
        assert!(k.is_cached(&changed, &Capability::root()));
        assert_eq!(other.bytes, read(untouched).bytes);
        // Restore for the next form so the edit changes its inferred set too.
        *content.write().unwrap() =
            format!("{PREFIXES}ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .");
        k.cut("urn:test:changed-thread");
    }
}
#[test]
fn largest_fixture_measurement() {
    let mut input = String::new();
    for i in 0..100 {
        input.push_str(&format!(
            "ex:C{i} rdfs:subClassOf ex:C{} . ex:x{i} a ex:C{i} .\n",
            i + 1
        ));
    }
    let k = with_graph(&input);
    let start = Instant::now();
    let all = closure(&k);
    eprintln!("largest fixture: 200 input triples, {} closure triples, {:?} (kernel load + reason + serialize + parse, debug)", all.len(), start.elapsed());
    assert!(triples("ex:x0 a ex:C100 . ex:C0 rdfs:subClassOf ex:C100 .").is_subset(&all));
}

#[test]
fn rdf_inputs_are_parsed_by_media_type_and_faces_transrept_without_loss() {
    use oxrdf::{GraphName, Triple};
    use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
    // Include a literal containing newlines, Unicode, and escaping so comparisons
    // exercise the serializers rather than just finding an IRI in their bytes.
    let input =
        triples("ex:p rdfs:subPropertyOf ex:q . ex:x ex:p \"line\\n雪 \\\"quoted\\\"\"@en .");
    let formats = [
        RdfFormat::Turtle,
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::TriG,
        RdfFormat::RdfXml,
        RdfFormat::JsonLd {
            profile: Default::default(),
        },
    ];
    let mut expected = None;
    for format in formats {
        let mut writer = RdfSerializer::from_format(format).for_writer(Vec::new());
        for triple in &input {
            writer
                .serialize_quad(&triple.clone().in_graph(GraphName::DefaultGraph))
                .unwrap();
        }
        let bytes = writer.finish().unwrap();
        let k = kernel(space().bind(
            Exact::new("urn:test:graph"),
            Graph(String::from_utf8(bytes).unwrap(), format.media_type()),
        ));
        let all = closure(&k);
        assert!(input.is_subset(&all), "{format:?}");
        if let Some(expected) = &expected {
            assert_eq!(&all, expected, "{format:?}");
        } else {
            expected = Some(all.clone());
        }
        for output in formats {
            let repr = issue(
                &k,
                "closure",
                &[("graph", "urn:test:graph"), ("as", output.media_type())],
            )
            .unwrap();
            let parsed: std::collections::HashSet<_> = RdfParser::from_format(output)
                .for_slice(&repr.bytes)
                .map(|q| {
                    let q = q.unwrap();
                    Triple::new(q.subject, q.predicate, q.object)
                })
                .collect();
            assert_eq!(parsed, all, "{format:?} -> {output:?}");
        }
    }
}

#[test]
fn http_graphs_are_resolved_through_the_kernel() {
    use async_trait::async_trait;
    use ikigai_core::{Description, Endpoint, Invocation, ReprType, Representation, Result, Verb};
    struct Http;
    #[async_trait]
    impl Endpoint for Http {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            assert_eq!(inv.inline_str("url")?, "https://example.org/schema");
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                format!("{PREFIXES}ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .").into_bytes(),
            )
            .cacheable()
            .depends_on("urn:test:http-thread"))
        }
        fn name(&self) -> &str {
            "http-fixture"
        }
        fn describe(&self) -> Description {
            Description::new("http-fixture").verb(Verb::Source)
        }
    }
    let k = kernel(space().bind(Exact::new("urn:http:get"), Http));
    let req = request("closure", &[("graph", "https://example.org/schema")]);
    let out = block_on(k.issue(req.clone(), &Capability::root())).unwrap();
    assert!(triples("ex:x ex:q ex:y .").is_subset(&parse(&out.bytes)));
    assert!(k.is_cached(&req, &Capability::root()));
    k.cut("urn:test:http-thread");
    assert!(!k.is_cached(&req, &Capability::root()));
}

#[cfg(feature = "rdf-12")]
#[test]
fn rdf12_triple_terms_are_a_typed_refusal() {
    let k = with_graph("ex:x ex:p <<(ex:a ex:q ex:b)>> .");
    assert!(
        matches!(issue(&k, "closure", &[("graph", "urn:test:graph")]),
        Err(Error::InvalidArgument { name, detail }) if name == "graph" && detail.contains("RDF 1.2 triple terms"))
    );
}

#[test]
fn live_sources_keep_derived_faces_uncacheable() {
    use ikigai_core::{Expiry, FnEndpoint, ReprType, Representation};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let k = kernel(space().bind(
        Exact::new("urn:test:graph"),
        FnEndpoint::new("live", move |_| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                format!("{PREFIXES}ex:x ex:p {n} .").into_bytes(),
            ))
        }),
    ));
    for face in ["text/turtle", "application/n-triples"] {
        let req = request("closure", &[("graph", "urn:test:graph"), ("as", face)]);
        let read = || block_on(k.issue(req.clone(), &Capability::root())).unwrap();
        let first = read();
        let second = read();
        assert!(matches!(first.expiry, Expiry::Always));
        assert!(matches!(second.expiry, Expiry::Always));
        assert_ne!(first.bytes, second.bytes);
        assert!(!k.is_cached(&req, &Capability::root()));
    }
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}
