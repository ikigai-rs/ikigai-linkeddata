use super::*;
use oxrdf::{Literal, NamedNode, Variable};
use spargebra::algebra::PropertyPathExpression;
use spargebra::term::NamedNodePattern;
use spargebra::SparqlParser;

fn parsed(text: &str) -> BTreeSet<RepeatabilityReason> {
    let query = SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|err| panic!("{text}: {err}"));
    repeatability_reasons(&query).unwrap()
}

#[test]
fn finds_all_reasons_in_the_same_query() {
    use RepeatabilityReason::*;
    assert_eq!(
        parsed(
            "CONSTRUCT { _:fresh <urn:p> ?t } WHERE {
                { SELECT REDUCED
                    (SAMPLE(NOW()) AS ?t)
                    (GROUP_CONCAT(STRUUID()) AS ?c)
                  WHERE { FILTER EXISTS { BIND(BNODE(STR(UUID())) AS ?b) } }
                  ORDER BY ASC(RAND()) DESC(NOW()) LIMIT 1 }
            }"
        ),
        [
            Now,
            Rand,
            Uuid,
            StrUuid,
            BNode,
            Slice,
            Sample,
            GroupConcat,
            Reduced,
            ConstructTemplateBlankNode
        ]
        .into()
    );
}

#[test]
fn parsed_query_forms_and_ordering() {
    use RepeatabilityReason::*;
    for (text, reason) in [
        ("SELECT (bNoDe() AS ?id) WHERE {}", BNode),
        ("ASK { FILTER (RAND() > 0.5) }", Rand),
        ("DESCRIBE ?s WHERE { BIND(UUID() AS ?s) }", Uuid),
        ("CONSTRUCT { ?s <urn:p> ?o } WHERE { ?s <urn:p> ?o } OFFSET 1", Slice),
        ("SELECT * WHERE { VALUES ?x { 1 2 } } ORDER BY ?x LIMIT 1", Slice),
        ("SELECT * WHERE { { SELECT * WHERE { VALUES ?x { 1 2 } } LIMIT 0 } } ORDER BY ?x", Slice),
        ("CONSTRUCT { <urn:s> <urn:p> <<( <urn:x> <urn:q> <<( _:nested <urn:r> 1 )>> )>> } WHERE {}", ConstructTemplateBlankNode),
    ] {
        assert_eq!(parsed(text), [reason].into(), "{text}");
    }
    for text in [
        "SELECT (\"NOW() BNODE()\" AS ?x) WHERE { <urn:RAND> ?p ?now } # LIMIT 1",
        "SELECT DISTINCT (COUNT(*) AS ?n) WHERE { _:matching <urn:p>/<urn:q> ?x VALUES ?x { 1 } }",
        "SELECT * WHERE { VALUES ?x { 1 2 } } ORDER BY ?x",
        "CONSTRUCT { <urn:s> <urn:p> <<( <urn:x> <urn:q> 1 )>> } WHERE {}",
    ] {
        assert!(parsed(text).is_empty(), "{text}");
    }
}

fn empty() -> Box<GraphPattern> {
    Box::new(GraphPattern::Bgp { patterns: vec![] })
}

fn call(function: Function) -> Expression {
    Expression::FunctionCall(function, vec![])
}

fn var() -> Variable {
    Variable::new_unchecked("v")
}

fn binding(function: Function) -> Box<GraphPattern> {
    Box::new(GraphPattern::Extend {
        inner: empty(),
        variable: var(),
        expression: call(function),
    })
}

fn classify(pattern: GraphPattern) -> Result<BTreeSet<RepeatabilityReason>, TraversalLimit> {
    repeatability_reasons(&Query::Ask {
        dataset: None,
        base_iri: None,
        pattern,
    })
}

#[test]
fn graph_patterns_visit_every_child() {
    use GraphPattern::*;
    use RepeatabilityReason::*;
    let left = || binding(Function::Now);
    let right = || binding(Function::Uuid);
    let name = || NamedNodePattern::NamedNode(NamedNode::new_unchecked("urn:test"));
    for pattern in [
        Join {
            left: left(),
            right: right(),
        },
        Lateral {
            left: left(),
            right: right(),
        },
        Union {
            left: left(),
            right: right(),
        },
        Minus {
            left: left(),
            right: right(),
        },
        LeftJoin {
            left: left(),
            right: right(),
            expression: None,
        },
        Filter {
            inner: left(),
            expr: call(Function::Uuid),
        },
        Extend {
            inner: left(),
            variable: var(),
            expression: call(Function::Uuid),
        },
        Group {
            inner: left(),
            variables: vec![],
            aggregates: vec![
                (
                    var(),
                    AggregateExpression::CountSolutions { distinct: false },
                ),
                (
                    var(),
                    AggregateExpression::FunctionCall {
                        name: AggregateFunction::Max,
                        expr: call(Function::Uuid),
                        distinct: false,
                    },
                ),
            ],
        },
    ] {
        assert_eq!(
            classify(pattern.clone()).unwrap(),
            [Now, Uuid].into(),
            "{pattern:?}"
        );
    }
    assert_eq!(
        classify(LeftJoin {
            left: left(),
            right: right(),
            expression: Some(call(Function::StrUuid)),
        })
        .unwrap(),
        [Now, Uuid, StrUuid].into()
    );
    assert_eq!(
        classify(OrderBy {
            inner: left(),
            expression: vec![
                OrderExpression::Asc(call(Function::Uuid)),
                OrderExpression::Desc(call(Function::StrUuid)),
            ],
        })
        .unwrap(),
        [Now, Uuid, StrUuid].into()
    );

    for pattern in [
        Graph {
            name: name(),
            inner: left(),
        },
        Service {
            name: name(),
            inner: left(),
            silent: true,
        },
        Project {
            inner: left(),
            variables: vec![var()],
        },
        Distinct { inner: left() },
    ] {
        assert_eq!(
            classify(pattern.clone()).unwrap(),
            [Now].into(),
            "{pattern:?}"
        );
    }
    assert_eq!(
        classify(GraphPattern::Reduced { inner: left() }).unwrap(),
        [Now, Reduced].into()
    );
    assert_eq!(
        classify(GraphPattern::Slice {
            inner: left(),
            start: 1,
            length: None
        })
        .unwrap(),
        [Now, Slice].into()
    );

    let path = Path {
        subject: TermPattern::Variable(var()),
        path: PropertyPathExpression::NamedNode(NamedNode::new_unchecked("urn:p")),
        object: TermPattern::Variable(var()),
    };
    for pattern in [
        *empty(),
        path,
        Values {
            variables: vec![var()],
            bindings: vec![],
        },
    ] {
        assert!(classify(pattern).unwrap().is_empty());
    }
}

#[test]
fn expressions_visit_every_child() {
    use Expression::*;
    use RepeatabilityReason::*;
    let now = || Box::new(call(Function::Now));
    let uuid = || Box::new(call(Function::Uuid));
    let filter = |expr| {
        classify(GraphPattern::Filter {
            expr,
            inner: empty(),
        })
        .unwrap()
    };
    for constructor in [
        Or,
        And,
        Equal,
        SameTerm,
        Greater,
        GreaterOrEqual,
        Less,
        LessOrEqual,
        Add,
        Subtract,
        Multiply,
        Divide,
    ] {
        assert_eq!(filter(constructor(now(), uuid())), [Now, Uuid].into());
    }
    for constructor in [UnaryPlus, UnaryMinus, Not] {
        assert_eq!(filter(constructor(now())), [Now].into());
    }
    assert_eq!(
        filter(In(now(), vec![*uuid(), call(Function::Rand)])),
        [Now, Uuid, Rand].into()
    );
    assert_eq!(
        filter(If(now(), uuid(), Box::new(call(Function::Rand)))),
        [Now, Uuid, Rand].into()
    );
    assert_eq!(filter(Coalesce(vec![*now(), *uuid()])), [Now, Uuid].into());
    assert_eq!(
        filter(FunctionCall(Function::BNode, vec![*now(), *uuid()])),
        [BNode, Now, Uuid].into()
    );
    assert_eq!(filter(Exists(binding(Function::Now))), [Now].into());
    for expr in [
        NamedNode(oxrdf::NamedNode::new_unchecked("urn:uuid")),
        Literal(oxrdf::Literal::new_simple_literal("UUID()")),
        Variable(var()),
        Bound(var()),
    ] {
        assert!(filter(expr).is_empty());
    }
}

#[test]
fn graph_depth_is_bounded_at_the_exact_limit() {
    let mut pattern = empty();
    for _ in 1..MAX_DEPTH {
        pattern = Box::new(GraphPattern::Distinct { inner: pattern });
    }
    assert!(classify((*pattern).clone()).unwrap().is_empty());
    assert_eq!(
        classify(GraphPattern::Distinct { inner: pattern }),
        Err(TraversalLimit::Depth)
    );
}

#[test]
fn wide_expression_counts_every_scheduled_node() {
    let classify_width = |width| {
        classify(GraphPattern::Filter {
            inner: empty(),
            expr: Expression::Coalesce(vec![Expression::Literal(Literal::from(true)); width]),
        })
    };
    // Filter, its BGP and COALESCE consume three slots before the arguments.
    assert!(classify_width(MAX_NODES - 3).unwrap().is_empty());
    assert_eq!(classify_width(MAX_NODES - 2), Err(TraversalLimit::Nodes));
}

#[test]
fn diagnostics_name_the_hazard_and_explain_it() {
    use RepeatabilityReason::*;
    for (reason, name, why) in [
        (Now, "NOW()", "instant"),
        (Rand, "RAND()", "pseudo-random"),
        (Uuid, "UUID()", "fresh IRI"),
        (StrUuid, "STRUUID()", "fresh UUID string"),
        (BNode, "BNODE()", "without a stable identity"),
        (Slice, "LIMIT or OFFSET", "without a proven total order"),
        (Sample, "SAMPLE", "arbitrary member"),
        (GroupConcat, "GROUP_CONCAT", "unspecified order"),
        (Reduced, "REDUCED", "duplicate rows"),
        (
            ConstructTemplateBlankNode,
            "CONSTRUCT template",
            "fresh blank node",
        ),
    ] {
        let message = reason.to_string();
        assert!(message.contains(name) && message.contains(why), "{message}");
    }
    assert!(TraversalLimit::Depth
        .to_string()
        .contains(&format!("depth limit of {MAX_DEPTH}")));
    assert!(TraversalLimit::Nodes
        .to_string()
        .contains(&format!("node limit of {MAX_NODES}")));
}
