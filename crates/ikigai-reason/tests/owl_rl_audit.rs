//! Small OWL 2 RL/RDF rule probes. Expected conclusions come from W3C section 4.3,
//! not from a snapshot of reasonable's output. See docs/audits/reason-correctness.md.
mod common;
use common::*;
use ikigai_core::{Error, Exact};
use ikigai_reason::{space_with_limits, Limits};
use reasonable::reasoner::Reasoner;

fn engine(input: &str) -> Reasoner {
    let mut reasoner = Reasoner::new();
    reasoner.load_triples(ikigai_rdf::canonical_triples(triples(input), |_| true));
    reasoner.reason();
    reasoner
}

fn check(input: &str, expected: &str, forbidden: &str) {
    let reasoner = engine(input);
    let direct: std::collections::HashSet<_> = reasoner.view_output().iter().cloned().collect();
    let missing: Vec<_> = triples(expected).difference(&direct).cloned().collect();
    let invented: Vec<_> = triples(forbidden).intersection(&direct).cloned().collect();
    assert!(
        missing.is_empty() && invented.is_empty(),
        "reasonable: missing: {missing:?}; invented: {invented:?}"
    );
    let actual = closure(&with_graph(input));
    assert_eq!(actual, direct.union(&triples(input)).cloned().collect());
}

macro_rules! rule {
    (known_failure $name:ident, $input:expr, $expected:expr, $forbidden:expr) => {
        #[test]
        #[ignore = "reasonable 0.4.4 rule defect; run explicitly; see reason-correctness audit"]
        fn $name() {
            check($input, $expected, $forbidden);
        }
    };
    ($name:ident, $input:expr, $expected:expr, $forbidden:expr) => {
        #[test]
        fn $name() {
            check($input, $expected, $forbidden);
        }
    };
}

rule!(known_failure eq_ref, "ex:x ex:p ex:y .", "ex:x owl:sameAs ex:x . ex:p owl:sameAs ex:p . ex:y owl:sameAs ex:y .", "ex:x owl:sameAs ex:y .");
rule!(
    eq_sym,
    "ex:x owl:sameAs ex:y . ex:z ex:p ex:w .",
    "ex:y owl:sameAs ex:x .",
    "ex:x owl:sameAs ex:z ."
);
rule!(
    eq_trans,
    "ex:x owl:sameAs ex:y . ex:y owl:sameAs ex:z . ex:w ex:p ex:v .",
    "ex:x owl:sameAs ex:z .",
    "ex:x owl:sameAs ex:w ."
);
rule!(
    eq_rep_s,
    "ex:x owl:sameAs ex:y . ex:x ex:p ex:z .",
    "ex:y ex:p ex:z .",
    "ex:z ex:p ex:y ."
);
rule!(
    eq_rep_p,
    "ex:p owl:sameAs ex:q . ex:x ex:p ex:y .",
    "ex:x ex:q ex:y .",
    "ex:y ex:q ex:x ."
);
rule!(
    eq_rep_o,
    "ex:y owl:sameAs ex:z . ex:x ex:p ex:y .",
    "ex:x ex:p ex:z .",
    "ex:z ex:p ex:x ."
);
rule!(
    prp_dom,
    "ex:p rdfs:domain ex:C . ex:x ex:p ex:y . ex:z ex:q ex:y .",
    "ex:x a ex:C .",
    "ex:y a ex:C . ex:z a ex:C ."
);
rule!(
    prp_rng,
    "ex:p rdfs:range ex:C . ex:x ex:p ex:y . ex:x ex:q ex:z .",
    "ex:y a ex:C .",
    "ex:x a ex:C . ex:z a ex:C ."
);
rule!(
    prp_symp,
    "ex:p a owl:SymmetricProperty . ex:x ex:p ex:y . ex:x ex:q ex:z .",
    "ex:y ex:p ex:x .",
    "ex:z ex:q ex:x . ex:x ex:p ex:x ."
);
rule!(
    prp_trp,
    "ex:p a owl:TransitiveProperty . ex:x ex:p ex:y . ex:y ex:p ex:z . ex:z ex:q ex:w .",
    "ex:x ex:p ex:z .",
    "ex:x ex:p ex:w . ex:z ex:p ex:x ."
);
rule!(
    prp_inv1,
    "ex:p owl:inverseOf ex:q . ex:x ex:p ex:y .",
    "ex:y ex:q ex:x .",
    "ex:x ex:q ex:y ."
);
rule!(
    prp_inv2,
    "ex:p owl:inverseOf ex:q . ex:x ex:q ex:y .",
    "ex:y ex:p ex:x .",
    "ex:x ex:p ex:y ."
);
rule!(
    prp_spo1,
    "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y . ex:z ex:q ex:w .",
    "ex:x ex:q ex:y .",
    "ex:z ex:p ex:w ."
);
rule!(
    prp_fp,
    "ex:p a owl:FunctionalProperty . ex:x ex:p ex:y, ex:z . ex:w ex:p ex:v .",
    "ex:y owl:sameAs ex:z . ex:z owl:sameAs ex:y .",
    "ex:y owl:sameAs ex:v . ex:x owl:sameAs ex:w ."
);
rule!(known_failure prp_ifp, "ex:p a owl:InverseFunctionalProperty . ex:x ex:p ex:z . ex:y ex:p ex:z . ex:w ex:p ex:v .", "ex:x owl:sameAs ex:y . ex:y owl:sameAs ex:x .", "ex:x owl:sameAs ex:w . ex:z owl:sameAs ex:v .");
rule!(
    cax_sco,
    "ex:A rdfs:subClassOf ex:B . ex:x a ex:A . ex:y a ex:B .",
    "ex:x a ex:B .",
    "ex:y a ex:A ."
);
rule!(
    cax_eqc1,
    "ex:A owl:equivalentClass ex:B . ex:x a ex:A . ex:y a ex:C .",
    "ex:x a ex:B .",
    "ex:y a ex:A ."
);
rule!(
    cax_eqc2,
    "ex:A owl:equivalentClass ex:B . ex:x a ex:B . ex:y a ex:C .",
    "ex:x a ex:A .",
    "ex:y a ex:B ."
);
// Named list cells keep comparisons independent of parser-generated blank labels.
const INTERSECTION: &str = "ex:C owl:intersectionOf ex:l . ex:l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ex:A ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> ex:m . ex:m <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ex:B ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .";
#[test]
fn cls_int1() {
    check(
        &format!("{INTERSECTION} ex:x a ex:A, ex:B . ex:y a ex:A ."),
        "ex:x a ex:C .",
        "ex:y a ex:C .",
    );
}
#[test]
fn cls_int2() {
    check(
        &format!("{INTERSECTION} ex:x a ex:C . ex:y a ex:A ."),
        "ex:x a ex:A, ex:B .",
        "ex:y a ex:B .",
    );
}
#[test]
fn cls_uni() {
    check(
        &format!(
            "{} ex:x a ex:A . ex:y a ex:B . ex:z a ex:C .",
            INTERSECTION.replace("intersectionOf", "unionOf")
        ),
        "ex:x a ex:C . ex:y a ex:C .",
        "ex:z a ex:A, ex:B . ex:x a ex:B .",
    );
}
rule!(cls_svf1, "ex:R owl:onProperty ex:p ; owl:someValuesFrom ex:C . ex:x ex:p ex:y . ex:y a ex:C . ex:z ex:p ex:w .", "ex:x a ex:R .", "ex:z a ex:R . ex:y a ex:R .");
rule!(cls_avf, "ex:R owl:onProperty ex:p ; owl:allValuesFrom ex:C . ex:x a ex:R ; ex:p ex:y . ex:z ex:p ex:w .", "ex:y a ex:C .", "ex:w a ex:C . ex:x a ex:C . ex:z a ex:R .");
rule!(
    cls_hv1,
    "ex:R owl:onProperty ex:p ; owl:hasValue ex:v . ex:x a ex:R . ex:y a ex:C .",
    "ex:x ex:p ex:v .",
    "ex:y ex:p ex:v ."
);
rule!(
    cls_hv2,
    "ex:R owl:onProperty ex:p ; owl:hasValue ex:v . ex:x ex:p ex:v . ex:y ex:p ex:w .",
    "ex:x a ex:R .",
    "ex:y a ex:R ."
);
rule!(known_failure scm_cls, "ex:C a owl:Class .", "ex:C rdfs:subClassOf ex:C, owl:Thing ; owl:equivalentClass ex:C . owl:Nothing rdfs:subClassOf ex:C .", "owl:Thing rdfs:subClassOf ex:C .");
rule!(
    scm_sco,
    "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .",
    "ex:A rdfs:subClassOf ex:C .",
    "ex:C rdfs:subClassOf ex:A ."
);
rule!(
    scm_eqc1,
    "ex:A owl:equivalentClass ex:B .",
    "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A .",
    "ex:A owl:sameAs ex:B ."
);
rule!(known_failure scm_eqc2, "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A .", "ex:A owl:equivalentClass ex:B .", "ex:A owl:sameAs ex:B .");
rule!(known_failure scm_op, "ex:p a owl:ObjectProperty .", "ex:p rdfs:subPropertyOf ex:p ; owl:equivalentProperty ex:p .", "ex:p a owl:DatatypeProperty .");
rule!(known_failure scm_dp, "ex:p a owl:DatatypeProperty .", "ex:p rdfs:subPropertyOf ex:p ; owl:equivalentProperty ex:p .", "ex:p a owl:ObjectProperty .");
rule!(known_failure scm_spo, "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:r .", "ex:p rdfs:subPropertyOf ex:r .", "ex:r rdfs:subPropertyOf ex:p .");
rule!(known_failure scm_eqp1, "ex:p owl:equivalentProperty ex:q .", "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:p .", "ex:p owl:sameAs ex:q .");
rule!(known_failure scm_eqp2, "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:p .", "ex:p owl:equivalentProperty ex:q .", "ex:p owl:sameAs ex:q .");
rule!(known_failure scm_dom1, "ex:p rdfs:domain ex:A . ex:A rdfs:subClassOf ex:B .", "ex:p rdfs:domain ex:B .", "ex:A rdfs:domain ex:p .");
rule!(known_failure scm_dom2, "ex:q rdfs:domain ex:A . ex:p rdfs:subPropertyOf ex:q .", "ex:p rdfs:domain ex:A .", "ex:A rdfs:domain ex:p .");
rule!(known_failure scm_rng1, "ex:p rdfs:range ex:A . ex:A rdfs:subClassOf ex:B .", "ex:p rdfs:range ex:B .", "ex:A rdfs:range ex:p .");
rule!(known_failure scm_rng2, "ex:q rdfs:range ex:A . ex:p rdfs:subPropertyOf ex:q .", "ex:p rdfs:range ex:A .", "ex:A rdfs:range ex:p .");
rule!(known_failure scm_hv, "ex:A owl:hasValue ex:v ; owl:onProperty ex:p . ex:B owl:hasValue ex:v ; owl:onProperty ex:q . ex:p rdfs:subPropertyOf ex:q .", "ex:A rdfs:subClassOf ex:B .", "ex:B rdfs:subClassOf ex:A .");
rule!(known_failure scm_svf1, "ex:A owl:someValuesFrom ex:X ; owl:onProperty ex:p . ex:B owl:someValuesFrom ex:Y ; owl:onProperty ex:p . ex:X rdfs:subClassOf ex:Y .", "ex:A rdfs:subClassOf ex:B .", "ex:B rdfs:subClassOf ex:A .");
rule!(known_failure scm_svf2, "ex:A owl:someValuesFrom ex:X ; owl:onProperty ex:p . ex:B owl:someValuesFrom ex:X ; owl:onProperty ex:q . ex:p rdfs:subPropertyOf ex:q .", "ex:A rdfs:subClassOf ex:B .", "ex:B rdfs:subClassOf ex:A .");
rule!(known_failure scm_avf1, "ex:A owl:allValuesFrom ex:X ; owl:onProperty ex:p . ex:B owl:allValuesFrom ex:Y ; owl:onProperty ex:p . ex:X rdfs:subClassOf ex:Y .", "ex:A rdfs:subClassOf ex:B .", "ex:B rdfs:subClassOf ex:A .");
rule!(known_failure scm_avf2, "ex:A owl:allValuesFrom ex:X ; owl:onProperty ex:p . ex:B owl:allValuesFrom ex:X ; owl:onProperty ex:q . ex:p rdfs:subPropertyOf ex:q .", "ex:B rdfs:subClassOf ex:A .", "ex:A rdfs:subClassOf ex:B .");
#[test]
#[ignore = "reasonable 0.4.4 rule defect; run explicitly; see reason-correctness audit"]
fn scm_int() {
    check(
        INTERSECTION,
        "ex:C rdfs:subClassOf ex:A, ex:B .",
        "ex:A rdfs:subClassOf ex:C .",
    );
}
#[test]
#[ignore = "reasonable 0.4.4 rule defect; run explicitly; see reason-correctness audit"]
fn scm_uni() {
    check(
        &INTERSECTION.replace("intersectionOf", "unionOf"),
        "ex:A rdfs:subClassOf ex:C . ex:B rdfs:subClassOf ex:C .",
        "ex:C rdfs:subClassOf ex:A .",
    );
}

const INCONSISTENCIES: &[(&str, &str)] = &[
    ("cax-dw", "ex:A owl:disjointWith ex:B . ex:x a ex:A, ex:B ."),
    (
        "prp-irp",
        "ex:p a owl:IrreflexiveProperty . ex:x ex:p ex:x .",
    ),
    (
        "prp-asyp",
        "ex:p a owl:AsymmetricProperty . ex:x ex:p ex:y . ex:y ex:p ex:x .",
    ),
    ("cls-nothing2", "ex:x a owl:Nothing ."),
    (
        "eq-diff1",
        "ex:x owl:sameAs ex:y ; owl:differentFrom ex:y .",
    ),
    (
        "cls-com",
        "ex:A owl:complementOf ex:B . ex:x a ex:A, ex:B .",
    ),
    (
        "prp-pdw",
        "ex:p owl:propertyDisjointWith ex:q . ex:x ex:p ex:y ; ex:q ex:y .",
    ),
];

#[test]
fn inconsistency_diagnostics_are_not_silently_discarded() {
    let mut failures = Vec::new();
    for &(rule, input) in INCONSISTENCIES {
        let reasoner = engine(input);
        let diagnostics: Vec<_> = reasoner.diagnostics().iter().map(|d| d.rule()).collect();
        eprintln!("{rule}: engine diagnostics {diagnostics:?}");
        if !["eq-diff1", "cls-com"].contains(&rule) {
            assert!(diagnostics.contains(&rule), "engine lost {rule} diagnostic");
            for form in ["closure", "inferred"] {
                for face in [
                    "text/turtle",
                    "application/n-triples",
                    "application/n-quads",
                    "application/trig",
                    "application/rdf+xml",
                    "application/ld+json",
                    "text/html",
                ] {
                    let result = issue(
                        &with_graph(input),
                        form,
                        &[("graph", "urn:test:graph"), ("as", face)],
                    );
                    if !matches!(result, Err(Error::InvalidArgument { ref name, ref detail }) if name == "graph" && detail.contains(rule))
                    {
                        failures.push(format!(
                            "{rule}: {form} {face} did not refuse with the rule ID"
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "reasonable 0.4.4 rule defect; run explicitly; see reason-correctness audit"]
fn engine_detects_all_requested_inconsistencies() {
    let missing: Vec<_> = INCONSISTENCIES
        .iter()
        .filter_map(|&(rule, input)| {
            (!engine(input).diagnostics().iter().any(|d| d.rule() == rule)).then_some(rule)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "missing inconsistency diagnostics: {missing:?}"
    );
}

#[test]
fn input_occurrences_and_distinct_closure_boundaries() {
    let input = "ex:x ex:p \"a literal longer than the triple limit\" .";
    let size = closure(&with_graph(input)).len();
    for form in ["closure", "inferred"] {
        for output_limit in [size - 1, size, size + 1] {
            let k = kernel(
                space_with_limits(Limits {
                    max_input_triples: 1,
                    max_output_triples: output_limit,
                })
                .bind(Exact::new("urn:test:graph"), fixture(input)),
            );
            let result = issue(&k, form, &[("graph", "urn:test:graph")]);
            if output_limit < size {
                assert!(matches!(result, Err(Error::OverBudget { .. })));
            } else {
                assert!(result.is_ok());
            }
        }
        let k = kernel(
            space_with_limits(Limits {
                max_input_triples: 1,
                max_output_triples: 1000,
            })
            .bind(
                Exact::new("urn:test:graph"),
                fixture(&format!("{input} {input}")),
            ),
        );
        assert!(matches!(
            issue(&k, form, &[("graph", "urn:test:graph")]),
            Err(Error::OverBudget { .. })
        ));
    }
}

#[test]
#[ignore = "reasonable 0.4.4 false datatype-range diagnostic; see reason-correctness audit"]
fn ordinary_object_range_has_no_datatype_clash() {
    let reasoner = engine("ex:p rdfs:range ex:C . ex:x ex:p ex:y .");
    let errors: Vec<_> = reasoner
        .diagnostics()
        .iter()
        .map(|d| d.to_string())
        .collect();
    assert!(errors.is_empty(), "false diagnostics: {errors:?}");
}

#[test]
fn blank_nodes_do_not_escape_on_rdf_faces() {
    let k = with_graph(
        "ex:R owl:intersectionOf (ex:A ex:B) . ex:x a ex:A, ex:B . _:a ex:p [ ex:q ex:y ] . ex:p rdfs:subPropertyOf ex:r .",
    );
    for form in ["closure", "inferred"] {
        for face in [
            "text/turtle",
            "application/n-triples",
            "application/n-quads",
            "application/trig",
            "application/rdf+xml",
            "application/ld+json",
        ] {
            let result = issue(&k, form, &[("graph", "urn:test:graph"), ("as", face)]).unwrap();
            let graph = ikigai_conformance::rdf::parse(face, &result.bytes).unwrap();
            assert!(
                ikigai_conformance::rdf::blank_nodes(&graph).is_empty(),
                "{form} {face} contains blank nodes"
            );
        }
    }
}

#[test]
fn noncontradictory_controls_are_accepted() {
    for input in [
        "ex:A owl:disjointWith ex:B . ex:x a ex:A . ex:y a ex:B .",
        "ex:p a owl:IrreflexiveProperty . ex:x ex:p ex:y .",
        "ex:p a owl:AsymmetricProperty . ex:x ex:p ex:y . ex:y ex:q ex:x .",
        "ex:A owl:complementOf ex:B . ex:x a ex:A . ex:y a ex:B .",
        "ex:p owl:propertyDisjointWith ex:q . ex:x ex:p ex:y ; ex:q ex:z .",
        "ex:p rdfs:range ex:C . ex:x ex:p ex:y .",
    ] {
        for form in ["closure", "inferred"] {
            assert!(
                issue(&with_graph(input), form, &[("graph", "urn:test:graph")]).is_ok(),
                "{input}"
            );
        }
    }
}

#[test]
fn merged_derived_contradictions_refuse_before_output_filters() {
    let k = kernel(ikigai_reason::space()
        .bind(Exact::new("urn:test:data"), fixture("ex:x a ex:A . ex:A rdfs:subClassOf ex:B ."))
        .bind(Exact::new("urn:test:schema"), fixture("ex:A owl:disjointWith ex:B . ex:x ex:p ex:x . ex:p a owl:IrreflexiveProperty .")));
    for form in ["closure", "inferred"] {
        let args = [
            ("graph", "urn:test:data urn:test:schema"),
            ("omit-thing", "true"),
        ];
        let result = issue(&k, form, &args);
        assert!(
            matches!(result, Err(Error::InvalidArgument { name, detail }) if name == "graph" && detail.ends_with("cax-dw, prp-irp."))
        );
    }
}

#[test]
fn exact_data_projections_do_not_invent_edges_or_class_membership() {
    // Compare every triple in each selected predicate/class projection, rather
    // than checking just one positive and one negative conclusion.
    let cases = [
        (
            "ex:p a owl:SymmetricProperty . ex:x ex:p ex:y . ex:z ex:q ex:w .",
            "ex:x ex:p ex:y . ex:y ex:p ex:x .",
            "http://example.org/p",
        ),
        (
            "ex:p a owl:TransitiveProperty . ex:x ex:p ex:y . ex:y ex:p ex:z . ex:z ex:q ex:w .",
            "ex:x ex:p ex:y . ex:y ex:p ex:z . ex:x ex:p ex:z .",
            "http://example.org/p",
        ),
        (
            "ex:p owl:inverseOf ex:q . ex:x ex:p ex:y . ex:z ex:q ex:w .",
            "ex:y ex:q ex:x . ex:z ex:q ex:w .",
            "http://example.org/q",
        ),
        (
            "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y . ex:z ex:q ex:w .",
            "ex:x ex:q ex:y . ex:z ex:q ex:w .",
            "http://example.org/q",
        ),
        (
            "ex:R owl:onProperty ex:p ; owl:hasValue ex:v . ex:x a ex:R . ex:y ex:p ex:w .",
            "ex:x ex:p ex:v . ex:y ex:p ex:w .",
            "http://example.org/p",
        ),
    ];
    for (input, expected, predicate) in cases {
        let actual = closure(&with_graph(input))
            .into_iter()
            .filter(|t| t.predicate.as_str() == predicate)
            .collect();
        assert_eq!(triples(expected), actual, "{input}");
    }
    let input = "ex:p rdfs:domain ex:A ; rdfs:range ex:B . ex:x ex:p ex:y . ex:z ex:q ex:w .";
    let actual = closure(&with_graph(input))
        .into_iter()
        .filter(|t| {
            t.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                && ["http://example.org/A", "http://example.org/B"]
                    .iter()
                    .any(|class| t.object == oxrdf::NamedNode::new(*class).unwrap().into())
        })
        .collect();
    assert_eq!(triples("ex:x a ex:A . ex:y a ex:B ."), actual);
}

#[test]
fn skolem_identity_is_scoped_and_subtraction_is_exact() {
    let source = "_:a ex:p [ ex:q ex:y ] . ex:p rdfs:subPropertyOf ex:r .";
    let read = |sources: &str, form| {
        let k = kernel(
            ikigai_reason::space()
                .bind(Exact::new("urn:test:first"), fixture(source))
                .bind(Exact::new("urn:test:second"), fixture(source)),
        );
        issue(&k, form, &[("graph", sources)]).unwrap().bytes
    };
    let first = parse(&read("urn:test:first", "closure"));
    let second = parse(&read("urn:test:second", "closure"));
    let merged = parse(&read(
        "urn:test:first urn:test:second urn:test:first",
        "closure",
    ));
    assert_eq!(merged, first.union(&second).cloned().collect());
    let nodes = |graph: &std::collections::HashSet<oxrdf::Triple>| -> std::collections::HashSet<_> {
        graph
            .iter()
            .filter(|t| t.predicate.as_str() == "http://example.org/p")
            .map(|t| (t.subject.clone(), t.object.clone()))
            .collect()
    };
    assert!(nodes(&first).is_disjoint(&nodes(&second)));
    let bytes = read("urn:test:first", "closure");
    for _ in 0..8 {
        assert_eq!(bytes, read("urn:test:first", "closure"));
    }
    let inferred = parse(&read("urn:test:first", "inferred"));
    // The three input triples are recognizable without depending on hash spelling.
    let input: std::collections::HashSet<_> = first
        .iter()
        .filter(|t| {
            [
                "http://example.org/p",
                "http://example.org/q",
                "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            ]
            .contains(&t.predicate.as_str())
        })
        .cloned()
        .collect();
    assert_eq!(input.len(), 3);
    assert_eq!(inferred, first.difference(&input).cloned().collect());
    let changed = parse(
        &issue(
            &kernel(ikigai_reason::space().bind(
                Exact::new("urn:test:first"),
                fixture("_:a ex:p [ ex:q ex:z ] ."),
            )),
            "closure",
            &[("graph", "urn:test:first")],
        )
        .unwrap()
        .bytes,
    );
    assert!(nodes(&first).is_disjoint(&nodes(&changed)));
}

#[test]
fn axioms_and_named_input_are_deterministic() {
    let input = "ex:x ex:p ex:y .";
    let first = issue(
        &with_graph(input),
        "closure",
        &[("graph", "urn:test:graph")],
    )
    .unwrap();
    for _ in 0..8 {
        assert_eq!(
            first.bytes,
            issue(
                &with_graph(input),
                "closure",
                &[("graph", "urn:test:graph")]
            )
            .unwrap()
            .bytes
        );
    }
    let all = parse(&first.bytes);
    assert!(triples("owl:Thing a owl:Class . owl:Nothing a owl:Class .").is_subset(&all));
    assert!(
        !triples("ex:p a <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> .").is_subset(&all)
    );
    assert!(triples(input).is_subset(&all));
}

#[test]
#[ignore = "reasonable 0.4.4 misses eq-diff1 and cls-com; successful endpoint answers remain possible"]
fn missing_engine_diagnostics_cannot_provide_a_consistency_verdict() {
    let mut accepted = Vec::new();
    for &(rule, input) in INCONSISTENCIES {
        if ["eq-diff1", "cls-com"].contains(&rule) {
            for form in ["closure", "inferred"] {
                if issue(&with_graph(input), form, &[("graph", "urn:test:graph")]).is_ok() {
                    accepted.push(format!(
                        "{rule}: {form} silently accepted the inconsistent graph"
                    ));
                }
            }
        }
    }
    assert!(accepted.is_empty(), "{}", accepted.join("\n"));
}
