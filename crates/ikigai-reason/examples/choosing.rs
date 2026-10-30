//! Construction vs a SPARQL property path vs OWL 2 RL reasoning (ledger #2640).
//!
//! ```text
//! cargo run --release -p ikigai-reason --example choosing [-- RUNS]
//! ```
//!
//! Prints the Markdown tables behind README's "Choosing: construction, a property path,
//! or a reasoner". Every approach answers the same question over the same generated
//! graph: "what is above this class (or concept)?". Each time is the median of RUNS
//! (default 7) runs; a warm time is the median per-call cost over 1,000 calls.
//!
//! - **construction**: `ikigai_rdf::subclass_axioms` + `Kernel::with_subclass_axioms`, then
//!   `select_action` for the leaf, as the Selection demo does. An edit means a rebuild.
//! - **hybrid**: `urn:reason:closure`, its `subClassOf` pairs fed to the same kernel build.
//! - **sparql closure**: `urn:sparql:construct` of the path's whole closure.
//! - **sparql question**: `urn:sparql:select` of one class's supers (`path*`).
//! - **reason closure / inferred**: `urn:reason:closure` / `urn:reason:inferred`.
//!
//! The graph is a cacheable resource; an edit adds one superclass to the leaf and cuts the
//! graph's golden thread, and is checked to reach the answer.

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Exact, Fallback, FnEndpoint,
    Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_reason::{space_with_limits, Limits, DEFAULT_MAX_OUTPUT_TRIPLES};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GRAPH: &str = "urn:bench:graph";
const ADDED: &str = "urn:bench:Added";
const WARM_CALLS: u32 = 1_000;
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const PREFIXES: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix schema: <https://schema.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix ex: <http://example.org/> .
";
const CATEGORIES: [&str; 6] = [
    "subClassOf*",
    "equivalentClass",
    "subPropertyOf",
    "domain/range typing",
    "inverseOf",
    "broaderTransitive",
];

/// The graph under test: a cacheable resource whose text the harness edits.
#[derive(Clone)]
struct Live(Arc<Mutex<String>>, Arc<AtomicUsize>);
impl Live {
    fn new(ttl: &str) -> Self {
        Self(Arc::new(Mutex::new(ttl.to_string())), Arc::default())
    }
    fn reads(&self) -> usize {
        self.1.load(Ordering::SeqCst)
    }
    fn set(&self, ttl: &str) {
        *self.0.lock().unwrap() = ttl.to_string();
    }
}
#[async_trait]
impl Endpoint for Live {
    async fn invoke(&self, _: &Invocation<'_>) -> Result<Representation> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let bytes = self.0.lock().unwrap().clone().into_bytes();
        Ok(Representation::new(ReprType::new("text/turtle"), bytes).cacheable())
    }
    fn name(&self) -> &str {
        "bench-graph"
    }
    fn describe(&self) -> Description {
        Description::new("bench-graph").verb(Verb::Source)
    }
}

struct Scenario {
    name: &'static str,
    ttl: String,
    /// The asserted hierarchy predicate and the one a closure states it with.
    asserted: &'static str,
    entailed: &'static str,
    /// The class (or concept) asked about, and the top an action requires.
    leaf: String,
    root: String,
    limits: Limits,
    probes: Vec<(&'static str, String)>,
    /// Extra SPARQL closures to measure beside the scenario's own path.
    paths: Vec<(&'static str, String)>,
}

impl Scenario {
    fn edited(&self) -> String {
        format!(
            "{}<{}> <{}> <{ADDED}> .\n",
            self.ttl, self.leaf, self.asserted
        )
    }
}

fn iri(prefix: &str, local: impl std::fmt::Display) -> String {
    format!("{prefix}{local}")
}

/// Level sizes of a generated tree; node `i` at a level hangs under `i % parents`.
fn tree(levels: &[usize], node: impl Fn(usize) -> String, edge: &str, out: &mut String) {
    let mut first = 0;
    for pair in levels.windows(2) {
        let (above, here) = (pair[0], pair[1]);
        for i in 0..here {
            let parent = first + i % above;
            writeln!(
                out,
                "<{}> <{edge}> <{}> .",
                node(first + above + i),
                node(parent)
            )
            .unwrap();
        }
        first += above;
    }
}

fn scenarios() -> Vec<Scenario> {
    let mut all = Vec::new();
    let foaf_person = "http://xmlns.com/foaf/0.1/Person";
    let schema_person = "https://schema.org/Person";
    all.push(Scenario {
        name: "selection demo (1 axiom)",
        ttl: format!("<{foaf_person}> <{SUBCLASS}> <{schema_person}> .\n"),
        asserted: SUBCLASS,
        entailed: SUBCLASS,
        leaf: foaf_person.into(),
        root: schema_person.into(),
        limits: Limits::default(),
        probes: vec![(
            "subClassOf*",
            "foaf:Person rdfs:subClassOf schema:Person .".into(),
        )],
        paths: vec![],
    });

    // schema.org-sized: 800 classes, depth 6, 20 extra parents; a FOAF-like vocabulary of
    // 24 classes aligned to it by 24 subClassOf and 12 equivalentClass axioms, plus a few
    // property axioms and instances so the other entailments have something to say.
    let levels = [1, 10, 60, 200, 300, 180, 49];
    let schema = |i: usize| iri("https://schema.org/C", i);
    let mut ttl = String::new();
    tree(&levels, schema, SUBCLASS, &mut ttl);
    for i in 0..20 {
        let (child, parent) = (71 + i * 13, 11 + i * 2); // level 3+ under a second level-2 class
        writeln!(
            ttl,
            "<{}> <{SUBCLASS}> <{}> .",
            schema(child),
            schema(parent)
        )
        .unwrap();
    }
    let foaf = |i: usize| iri("http://xmlns.com/foaf/0.1/F", i);
    let equiv = |i: usize| iri("http://xmlns.com/foaf/0.1/E", i);
    for i in 1..12 {
        writeln!(ttl, "<{}> <{SUBCLASS}> <{}> .", foaf(i), foaf((i - 1) / 3)).unwrap();
    }
    for i in 0..12 {
        writeln!(
            ttl,
            "<{}> <{SUBCLASS}> <{}> .",
            foaf(i),
            schema(271 + i * 40)
        )
        .unwrap();
        writeln!(
            ttl,
            "<{}> <{OWL}equivalentClass> <{}> .",
            equiv(i),
            schema(571 + i * 15)
        )
        .unwrap();
    }
    ttl.push_str(&format!(
        "{PREFIXES}foaf:knows rdfs:domain foaf:F1 ; rdfs:range foaf:F1 .
foaf:made owl:inverseOf foaf:maker .
foaf:maker rdfs:subPropertyOf schema:creator .
ex:alice foaf:knows ex:bob ; foaf:made ex:doc .
ex:carol foaf:maker ex:dave .
"
    ));
    let leaf = foaf(11);
    all.push(Scenario {
        name: "alignment (800 + 24 classes, 36 mapping axioms)",
        ttl,
        asserted: SUBCLASS,
        entailed: SUBCLASS,
        root: schema(0),
        probes: vec![
            (
                "subClassOf*",
                format!("<{leaf}> rdfs:subClassOf schema:C0 ."),
            ),
            (
                "equivalentClass",
                format!("<{}> rdfs:subClassOf schema:C0 .", equiv(0)),
            ),
            ("subPropertyOf", "ex:carol schema:creator ex:dave .".into()),
            (
                "domain/range typing",
                "ex:alice a foaf:F1 . ex:bob a foaf:F1 .".into(),
            ),
            ("inverseOf", "ex:doc foaf:maker ex:alice .".into()),
        ],
        paths: vec![(
            "sparql closure, subClassOf|equivalentClass path",
            "(rdfs:subClassOf|owl:equivalentClass|^owl:equivalentClass)+".into(),
        )],
        leaf,
        limits: Limits {
            max_input_triples: 2_000,
            max_output_triples: DEFAULT_MAX_OUTPUT_TRIPLES,
        },
    });

    let chain = |i: usize| iri("http://example.org/D", i);
    let mut ttl = String::new();
    for i in 0..100 {
        writeln!(ttl, "<{}> <{SUBCLASS}> <{}> .", chain(i), chain(i + 1)).unwrap();
    }
    all.push(Scenario {
        name: "deep chain (depth 100)",
        ttl,
        asserted: SUBCLASS,
        entailed: SUBCLASS,
        leaf: chain(0),
        root: chain(100),
        limits: Limits::default(),
        probes: vec![("subClassOf*", "ex:D0 rdfs:subClassOf ex:D100 .".into())],
        paths: vec![],
    });

    let mut ttl = String::new();
    for i in 0..1_000 {
        writeln!(
            ttl,
            "<http://example.org/W{i}> <{SUBCLASS}> <http://example.org/Top> ."
        )
        .unwrap();
    }
    all.push(Scenario {
        name: "wide (1,000 siblings)",
        ttl,
        asserted: SUBCLASS,
        entailed: SUBCLASS,
        leaf: "http://example.org/W0".into(),
        root: "http://example.org/Top".into(),
        limits: Limits {
            max_input_triples: 2_000,
            max_output_triples: DEFAULT_MAX_OUTPUT_TRIPLES,
        },
        probes: vec![("subClassOf*", "ex:W999 rdfs:subClassOf ex:Top .".into())],
        paths: vec![],
    });

    // 2,000 concepts, depth 5, each typed skos:Concept and in one scheme; SKOS's own
    // broader ⊑ broaderTransitive and transitivity axioms ride along, as a reasoner needs.
    let levels = [1, 5, 25, 125, 600, 1_244];
    let concept = |i: usize| iri("http://example.org/k", i);
    let mut ttl = String::new();
    tree(&levels, concept, &iri(SKOS, "broader"), &mut ttl);
    for i in 0..2_000 {
        writeln!(
            ttl,
            "<{}> a <{SKOS}Concept> ; <{SKOS}inScheme> <http://example.org/scheme> .",
            concept(i)
        )
        .unwrap();
    }
    ttl.push_str(&format!(
        "{PREFIXES}skos:broader rdfs:subPropertyOf skos:broaderTransitive .
skos:broaderTransitive a owl:TransitiveProperty .
"
    ));
    let leaf = concept(1_999);
    all.push(Scenario {
        name: "SKOS scheme (2,000 concepts)",
        ttl,
        asserted: "http://www.w3.org/2004/02/skos/core#broader",
        entailed: "http://www.w3.org/2004/02/skos/core#broaderTransitive",
        root: concept(0),
        probes: vec![(
            "broaderTransitive",
            format!("<{leaf}> skos:broaderTransitive ex:k0 ."),
        )],
        leaf,
        limits: Limits {
            max_input_triples: 8_000,
            max_output_triples: DEFAULT_MAX_OUTPUT_TRIPLES,
        },
        paths: vec![],
    });
    all
}

fn parse(bytes: &[u8]) -> HashSet<Triple> {
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(bytes)
        .map(|q| {
            let q = q.unwrap();
            Triple::new(q.subject, q.predicate, q.object)
        })
        .collect()
}

fn probe(ttl: &str) -> HashSet<Triple> {
    parse(format!("{PREFIXES}{ttl}").as_bytes())
}

/// `(subject, object)` IRI pairs of `predicate`: `ikigai_rdf::subclass_axioms` for
/// `rdfs:subClassOf`, as the Selection demo reads them; the same rule for any other.
fn pairs(ttl: &str, predicate: &str) -> Vec<(String, String)> {
    if predicate == SUBCLASS {
        return ikigai_rdf::subclass_axioms(ttl);
    }
    parse(ttl.as_bytes())
        .into_iter()
        .filter(|t| t.predicate.as_str() == predicate)
        .filter_map(|t| match (t.subject, t.object) {
            (NamedOrBlankNode::NamedNode(s), Term::NamedNode(o)) => {
                Some((s.into_string(), o.into_string()))
            }
            _ => None,
        })
        .collect()
}

/// The map `Kernel::with_subclass_axioms` builds, rebuilt here because the kernel's is
/// private: same DFS, every class mapped to itself and each of its supers.
fn mirror_closure(pairs: &[(String, String)]) -> BTreeMap<String, BTreeSet<String>> {
    let mut direct: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut classes = BTreeSet::new();
    for (sub, sup) in pairs {
        direct.entry(sub).or_default().push(sup);
        classes.insert(sub.as_str());
        classes.insert(sup.as_str());
    }
    classes
        .into_iter()
        .map(|class| {
            let mut supers = BTreeSet::new();
            let mut stack = vec![class];
            while let Some(c) = stack.pop() {
                if supers.insert(c.to_string()) {
                    stack.extend(direct.get(c).into_iter().flatten());
                }
            }
            (class.to_string(), supers)
        })
        .collect()
}

/// A kernel holding one action that requires `root`, with the closure built in.
fn selection_kernel(root: &str, pairs: Vec<(String, String)>) -> Kernel {
    let action = FnEndpoint::new("act", |_| {
        Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
    })
    .with_description(
        Description::new("act")
            .verb(Verb::Source)
            .input(ArgSpec::new("thing").class(root)),
    );
    Kernel::new(Arc::new(
        EndpointSpace::new().bind(Exact::new("urn:bench:act"), action),
    ))
    .with_subclass_axioms(pairs)
}

fn select(kernel: &Kernel, leaf: &str) {
    assert_eq!(
        kernel.select_action(&[leaf]).len(),
        1,
        "{leaf} selects the action"
    );
}

fn request(target: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(target).unwrap()),
        |req, (key, value)| req.with_arg(*key, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(kernel: &Kernel, req: &Request) -> Vec<u8> {
    block_on(kernel.issue(req.clone(), &Capability::root()))
        .unwrap_or_else(|e| panic!("{e:?}"))
        .bytes
        .to_vec()
}

fn kernel_over(space: EndpointSpace, live: &Live) -> Kernel {
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space.bind(Exact::new(GRAPH), live.clone())),
        Arc::new(ikigai_rdf::space()),
    ])))
}

fn median(runs: usize, mut sample: impl FnMut() -> Duration) -> Duration {
    let mut times: Vec<_> = (0..runs).map(|_| sample()).collect();
    times.sort();
    times[runs / 2]
}

fn per_call(mut call: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..WARM_CALLS {
        call();
    }
    start.elapsed() / WARM_CALLS
}

fn time<T>(work: impl FnOnce() -> T) -> (Duration, T) {
    let start = Instant::now();
    let out = work();
    (start.elapsed(), out)
}

struct Measured {
    approach: String,
    cold: Duration,
    warm: Duration,
    edit: Duration,
    size: usize,
    unit: &'static str,
    /// The answer as triples, for the coverage table; `None` where it is not a graph.
    answer: Option<HashSet<Triple>>,
}

fn construction(s: &Scenario, runs: usize) -> Measured {
    let build = |ttl: &str| {
        let k = selection_kernel(&s.root, pairs(ttl, s.asserted));
        select(&k, &s.leaf);
        k
    };
    let cold = median(runs, || time(|| build(&s.ttl)).0);
    let k = build(&s.ttl);
    let warm = median(runs, || per_call(|| select(&k, &s.leaf)));
    let edited = s.edited();
    let edit = median(runs, || time(|| build(&edited)).0);
    assert!(mirror_closure(&pairs(&edited, s.asserted))[&s.leaf].contains(ADDED));
    let map = mirror_closure(&pairs(&s.ttl, s.asserted));
    let entailed = oxrdf::NamedNode::new(s.entailed).unwrap();
    let answer = map
        .iter()
        .flat_map(|(c, supers)| supers.iter().map(move |sup| (c, sup)))
        .map(|(c, sup)| {
            Triple::new(
                oxrdf::NamedNode::new(c).unwrap(),
                entailed.clone(),
                oxrdf::NamedNode::new(sup).unwrap(),
            )
        })
        .collect();
    Measured {
        approach: "construction (kernel map)".into(),
        cold,
        warm,
        edit,
        size: map.values().map(BTreeSet::len).sum(),
        unit: "map pairs",
        answer: Some(answer),
    }
}

fn hybrid(s: &Scenario, runs: usize) -> Measured {
    let req = request("urn:reason:closure", &[("graph", GRAPH)]);
    let reasoner = |live: &Live| kernel_over(space_with_limits(s.limits), live);
    let build = |closure: &[u8]| {
        let k = selection_kernel(
            &s.root,
            pairs(&String::from_utf8_lossy(closure), s.entailed),
        );
        select(&k, &s.leaf);
        k
    };
    let cold = median(runs, || {
        let r = reasoner(&Live::new(&s.ttl));
        time(|| build(&issue(&r, &req))).0
    });
    let closure = issue(&reasoner(&Live::new(&s.ttl)), &req);
    let k = build(&closure);
    let warm = median(runs, || per_call(|| select(&k, &s.leaf)));
    let edited = s.edited();
    let edit = median(runs, || {
        let live = Live::new(&s.ttl);
        let r = reasoner(&live);
        issue(&r, &req);
        live.set(&edited);
        time(|| {
            r.cut(GRAPH);
            build(&issue(&r, &req))
        })
        .0
    });
    let pairs = pairs(&String::from_utf8_lossy(&closure), s.entailed);
    Measured {
        approach: "hybrid (reason, then kernel map)".into(),
        cold,
        warm,
        edit,
        size: mirror_closure(&pairs).values().map(BTreeSet::len).sum(),
        unit: "map pairs",
        answer: Some(
            parse(&closure)
                .into_iter()
                .filter(|t| t.predicate.as_str() == s.entailed)
                .collect(),
        ),
    }
}

/// An answer resolved through the kernel: cold on a new kernel, warm from its cache, and
/// after an edit that cuts the graph's golden thread.
fn resolved(
    approach: String,
    s: &Scenario,
    runs: usize,
    space: impl Fn() -> EndpointSpace,
    req: Request,
    graph_answer: bool,
) -> Measured {
    let cold = median(runs, || {
        let k = kernel_over(space(), &Live::new(&s.ttl));
        time(|| issue(&k, &req)).0
    });
    let k = kernel_over(space(), &Live::new(&s.ttl));
    let first = issue(&k, &req);
    let warm = median(runs, || per_call(|| drop(issue(&k, &req))));
    let edited = s.edited();
    let edit = median(runs, || {
        let live = Live::new(&s.ttl);
        let k = kernel_over(space(), &live);
        issue(&k, &req);
        live.set(&edited);
        let reads = live.reads();
        let (took, after) = time(|| {
            k.cut(GRAPH);
            issue(&k, &req)
        });
        assert_eq!(
            live.reads(),
            reads + 1,
            "{approach}: the cut re-reads the graph"
        );
        // `inferred` need not name the added class: it is input, not an entailment.
        assert!(
            String::from_utf8_lossy(&after).contains(ADDED) || approach == "reason inferred",
            "{approach}: the edit reaches the answer"
        );
        took
    });
    let (size, unit, answer) = if graph_answer {
        let triples = parse(&first);
        (triples.len(), "triples", Some(triples))
    } else {
        let rows = String::from_utf8_lossy(&first).lines().count() - 1;
        (rows, "rows", None)
    };
    Measured {
        approach,
        cold,
        warm,
        edit,
        size,
        unit,
        answer,
    }
}

fn sparql_closure(s: &Scenario, label: &str, path: &str, runs: usize) -> Measured {
    let query = format!(
        "{}CONSTRUCT {{ ?c <{}> ?s }} FROM <{GRAPH}> WHERE {{ ?c {path} ?s }}",
        PREFIXES.replace("@prefix", "PREFIX").replace(" .\n", "\n"),
        s.entailed
    );
    let req = request(
        "urn:sparql:construct",
        &[("query", &query), ("graph", GRAPH)],
    );
    resolved(label.into(), s, runs, ikigai_sparql::space, req, true)
}

fn question(class: &str, predicate: &str) -> Request {
    let query = format!("SELECT ?s FROM <{GRAPH}> WHERE {{ <{class}> <{predicate}>* ?s }}");
    request(
        "urn:sparql:select",
        &[("query", &query), ("graph", GRAPH), ("as", "text/csv")],
    )
}

fn sparql_question(s: &Scenario, runs: usize) -> Measured {
    let req = question(&s.leaf, s.asserted);
    let label = "sparql question (SELECT, one class, path*)".into();
    resolved(label, s, runs, ikigai_sparql::space, req, false)
}

/// A new question on a kernel that has answered another: the cache keys on the request,
/// so it loads the graph again.
fn another_question(s: &Scenario, runs: usize) -> Duration {
    median(runs, || {
        let k = kernel_over(ikigai_sparql::space(), &Live::new(&s.ttl));
        issue(&k, &question(&s.leaf, s.asserted));
        time(|| issue(&k, &question(&s.root, s.asserted))).0
    })
}

fn reason(s: &Scenario, form: &str, runs: usize) -> Measured {
    let req = request(&format!("urn:reason:{form}"), &[("graph", GRAPH)]);
    let limits = s.limits;
    let label = format!("reason {form}");
    resolved(label, s, runs, move || space_with_limits(limits), req, true)
}

fn fmt(d: Duration) -> String {
    let us = d.as_secs_f64() * 1e6;
    if us < 1_000.0 {
        format!("{us:.1} µs")
    } else if us < 1_000_000.0 {
        format!("{:.2} ms", us / 1e3)
    } else {
        format!("{:.2} s", us / 1e6)
    }
}

/// A triple's N-Triples spelling with the vocabularies' prefixes, for a table cell.
fn short(triple: &str) -> String {
    let named = [
        ("rdf:", RDF),
        ("rdfs:", RDFS),
        ("owl:", OWL),
        ("skos:", SKOS),
        ("ex:", "http://example.org/"),
        ("foaf:", "http://xmlns.com/foaf/0.1/"),
        ("schema:", "https://schema.org/"),
    ];
    let terms: Vec<_> = triple
        .split(' ')
        .map(|term| {
            let bare = term.trim_start_matches('<').trim_end_matches('>');
            match named.iter().find(|(_, ns)| bare.starts_with(ns)) {
                Some((prefix, ns)) => format!("{prefix}{}", &bare[ns.len()..]),
                None => term.to_string(),
            }
        })
        .collect();
    terms.join(" ").replace("rdf:type", "a")
}

/// What OWL RL adds that the question did not ask for, by kind.
fn unrequested(inferred: &HashSet<Triple>) -> Vec<(&'static str, Vec<String>)> {
    let mut kinds: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    kinds.insert("reflexive owl:sameAs", Vec::new());
    let vocab = |s: &str| [RDF, RDFS, OWL, XSD].iter().any(|ns| s.starts_with(ns));
    for t in inferred {
        let s = t.subject.to_string();
        let o = t.object.to_string();
        let p = t.predicate.as_str();
        let kind = if s == o && p == iri(OWL, "sameAs") {
            "reflexive owl:sameAs"
        } else if s == o {
            "other reflexive (C ⊑ C, C ≡ C, p ⊑ p, ...)"
        } else if o == format!("<{OWL}Thing>") {
            "x a owl:Thing / C ⊑ owl:Thing"
        } else if p == iri(RDF, "type") && vocab(&o[1..]) {
            "typed with an RDF/RDFS/OWL class (a owl:Class, ...)"
        } else if vocab(&s[1..]) {
            "about the RDF/RDFS/OWL vocabulary itself"
        } else {
            "the rest (what was asked, and its consequences)"
        };
        kinds.entry(kind).or_default().push(t.to_string());
    }
    for triples in kinds.values_mut() {
        triples.sort();
    }
    kinds.into_iter().collect()
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .map(|r| r.parse().expect("RUNS is a number"))
        .unwrap_or(7);
    println!(
        "Release: {}. Median of {runs} runs; warm = per call over {WARM_CALLS} calls.\n",
        !cfg!(debug_assertions)
    );
    let bare = selection_kernel("http://example.org/Top", Vec::new());
    let baseline = median(runs, || {
        per_call(|| select(&bare, "http://example.org/Top"))
    });
    println!(
        "select_action on a kernel with no closure, asked for the action's own class: {} per call.\n",
        fmt(baseline)
    );
    for s in scenarios() {
        let input = parse(s.ttl.as_bytes());
        println!("### {}\n", s.name);
        println!(
            "{} input triples; reasoner Limits {{ max_input_triples: {}, max_output_triples: {} }}.\n",
            input.len(),
            s.limits.max_input_triples,
            s.limits.max_output_triples
        );
        let path = format!("<{}>+", s.asserted);
        let mut rows = vec![
            construction(&s, runs),
            hybrid(&s, runs),
            sparql_closure(&s, "sparql closure (CONSTRUCT, path+)", &path, runs),
        ];
        for (label, path) in &s.paths {
            rows.push(sparql_closure(&s, label, path, runs));
        }
        rows.push(sparql_question(&s, runs));
        rows.push(reason(&s, "closure", runs));
        rows.push(reason(&s, "inferred", runs));
        println!("| approach | cold | warm | after an edit | answer |");
        println!("| --- | ---: | ---: | ---: | ---: |");
        for m in &rows {
            println!(
                "| {} | {} | {} | {} | {} {} |",
                m.approach,
                fmt(m.cold),
                fmt(m.warm),
                fmt(m.edit),
                m.size,
                m.unit
            );
        }
        // `inferred` is the closure minus the input by construction; its column would only
        // say which probes the input already states.
        println!(
            "\nA second, different SPARQL question on the same warm kernel: {}.",
            fmt(another_question(&s, runs))
        );
        let graphs: Vec<_> = rows
            .iter()
            .filter(|m| m.answer.is_some() && m.approach != "reason inferred")
            .collect();
        println!(
            "\n| entailment | {} |",
            graphs
                .iter()
                .map(|m| m.approach.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        );
        println!("| --- |{}", " :---: |".repeat(graphs.len()));
        for category in CATEGORIES {
            let wanted: Vec<_> = s.probes.iter().filter(|(c, _)| *c == category).collect();
            let cells: Vec<_> = graphs
                .iter()
                .map(|m| {
                    if wanted.is_empty() {
                        "–"
                    } else if wanted
                        .iter()
                        .all(|(_, ttl)| probe(ttl).is_subset(m.answer.as_ref().unwrap()))
                    {
                        "yes"
                    } else {
                        "no"
                    }
                })
                .collect();
            println!("| {category} | {} |", cells.join(" | "));
        }
        let closure = rows
            .iter()
            .find(|m| m.approach == "reason closure")
            .unwrap();
        let inferred: HashSet<_> = closure
            .answer
            .as_ref()
            .unwrap()
            .difference(&input)
            .cloned()
            .collect();
        println!("\nOWL RL inferred {} triples:\n", inferred.len());
        println!("| kind | triples | e.g. |\n| --- | ---: | --- |");
        for (kind, triples) in unrequested(&inferred) {
            let shown: Vec<_> = triples
                .iter()
                .take(2)
                .map(|t| format!("`{}`", short(t)))
                .collect();
            println!("| {kind} | {} | {} |", triples.len(), shown.join(" "));
        }
        println!();
    }
}
