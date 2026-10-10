//! Every answer to caller SPARQL is bounded in SIZE, and refused, never truncated, when it
//! would pass the bound (ledger #970).
//!
//! The claim: a time budget (ledger #964) bounds how LONG a query may run, not how much it
//! may produce in that time. Unbudgeted, a three-pattern cross product produced 1.3 M rows in
//! 0.57 s, and ikigai-cms-web PR 98 measured a 1.3 KB `VALUES` cross product answering 200
//! with 272 MB after 11.2 s on 0.1.11. Every byte of that answer is held in memory before the
//! first one leaves.
//!
//! These tests use `VALUES` cross products: they need no data (no `graph=`, nothing loaded),
//! they are exactly the CMS shape, and their row count is known in advance.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_sparql::budget::{AnswerBound, DEFAULT_BUDGET, DEFAULT_MAX_BYTES, DEFAULT_MAX_ROWS};
use ikigai_sparql::Store;
use std::sync::Arc;
use std::time::Instant;

/// `SELECT * { VALUES ?v0 { 1 … n } VALUES ?v1 { 1 … n } … }`: `n^vars` rows from a text of a
/// few bytes per value.
fn product(vars: usize, n: usize) -> String {
    let values: String = (1..=n).map(|i| format!("{i} ")).collect();
    let groups: String = (0..vars)
        .map(|v| format!("VALUES ?v{v} {{ {values}}} "))
        .collect();
    format!("SELECT * WHERE {{ {groups}}}")
}

fn request(form: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:sparql:{form}")).unwrap(),
        ),
        |req, (name, value)| req.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(kernel: &Kernel, req: Request) -> ikigai_core::Result<Vec<u8>> {
    block_on(kernel.issue(req, &Capability::root())).map(|rep| rep.bytes.to_vec())
}

/// A single `VALUES` list of `n` IRIs as subjects and one of `m` integers as objects, so a
/// `CONSTRUCT` over them makes `n * m` triples.
fn construct_product(n: usize, m: usize) -> String {
    let subjects: String = (1..=n).map(|i| format!("<urn:s{i}> ")).collect();
    let objects: String = (1..=m).map(|i| format!("{i} ")).collect();
    format!(
        "CONSTRUCT {{ ?s <urn:p> ?o }} WHERE {{ VALUES ?s {{ {subjects}}} VALUES ?o {{ {objects}}} }}"
    )
}

fn default_kernel() -> Kernel {
    Kernel::new(Arc::new(ikigai_sparql::space()))
}

fn bounded_kernel(rows: u64, bytes: u64) -> Kernel {
    Kernel::new(Arc::new(ikigai_sparql::space_with_bounds(
        DEFAULT_BUDGET,
        AnswerBound::new(rows, bytes).unwrap(),
    )))
}

fn lines(body: &[u8]) -> usize {
    body.iter().filter(|b| **b == b'\n').count()
}

/// The refusal an over-bound answer must be: `InvalidArgument` on `query`, naming the bound.
fn assert_too_large(outcome: ikigai_core::Result<Vec<u8>>, says: &str) {
    match outcome {
        Ok(body) => panic!(
            "expected a refusal saying `{says}`, got an answer of {} bytes ({} lines)",
            body.len(),
            lines(&body)
        ),
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "query", "{detail}");
            assert!(detail.contains(says), "{detail}");
            assert!(detail.contains("refused, not truncated"), "{detail}");
        }
        Err(other) => panic!("expected InvalidArgument on `query`, got {other:?}"),
    }
}

/// The reproduction: 160,000 rows (two 400-value lists) from a 3 KB query, over the default
/// space. Before ledger #970 the whole answer came back: 1,353,607 bytes, 160,001 lines.
#[test]
fn an_answer_past_the_default_row_bound_is_refused_not_sent() {
    let text = product(2, 400);
    assert!(text.len() < 4096, "{}", text.len());
    assert_too_large(
        issue(
            &default_kernel(),
            request("select", &[("query", &text), ("as", "text/csv")]),
        ),
        &format!("the answer exceeds {DEFAULT_MAX_ROWS} rows; add LIMIT, narrow the query"),
    );
}

/// Rows are not the only measure: 85,184 rows is under the row bound, and as SPARQL JSON
/// results it is over 16 MiB.
#[test]
fn an_answer_past_the_default_byte_bound_is_refused_not_sent() {
    let text = product(3, 44);
    assert_too_large(
        issue(&default_kernel(), request("select", &[("query", &text)])),
        &format!("the answer exceeds {DEFAULT_MAX_BYTES} bytes"),
    );
    // The same rows as CSV are well under it, and come back whole.
    let body = issue(
        &default_kernel(),
        request("select", &[("query", &text), ("as", "text/csv")]),
    )
    .unwrap();
    assert_eq!(lines(&body), 44 * 44 * 44 + 1);
}

/// Counting stops AT the bound, not at the deadline: a billion-row product is refused for its
/// size long before the time budget would have stopped it.
#[test]
fn an_unbounded_product_is_refused_at_the_bound_not_at_the_deadline() {
    let text = product(3, 1000);
    let started = Instant::now();
    assert_too_large(
        issue(
            &default_kernel(),
            request("select", &[("query", &text), ("as", "text/csv")]),
        ),
        "rows",
    );
    assert!(
        started.elapsed() < DEFAULT_BUDGET,
        "{:?}",
        started.elapsed()
    );
}

/// A host's bound is exact: the answer AT it is served whole, and one row past it is refused.
#[test]
fn a_hosts_row_bound_admits_the_answer_at_it_and_refuses_one_past_it() {
    let kernel = bounded_kernel(1000, DEFAULT_MAX_BYTES);
    let at = issue(
        &kernel,
        request(
            "select",
            &[("query", &product(1, 1000)), ("as", "text/csv")],
        ),
    )
    .unwrap();
    assert_eq!(lines(&at), 1001);
    assert_too_large(
        issue(
            &kernel,
            request(
                "select",
                &[("query", &product(1, 1001)), ("as", "text/csv")],
            ),
        ),
        "the answer exceeds 1000 rows",
    );
}

/// The byte bound is exact too, and counts every byte the serializer writes, its header and
/// trailer included.
#[test]
fn a_byte_bound_admits_the_answer_at_it_and_refuses_one_byte_less() {
    let text = product(2, 30);
    let whole = issue(&default_kernel(), request("select", &[("query", &text)])).unwrap();
    let size = whole.len() as u64;
    let at = issue(
        &bounded_kernel(DEFAULT_MAX_ROWS, size),
        request("select", &[("query", &text)]),
    )
    .unwrap();
    assert_eq!(at, whole);
    assert_too_large(
        issue(
            &bounded_kernel(DEFAULT_MAX_ROWS, size - 1),
            request("select", &[("query", &text)]),
        ),
        &format!("the answer exceeds {} bytes", size - 1),
    );
}

/// A request's `max_rows=` and `max_bytes=` lower the space's bound, and cannot raise it.
#[test]
fn a_request_can_lower_the_bound_and_cannot_raise_it() {
    let kernel = default_kernel();
    let ten = product(1, 10);
    let rows = |max: &str, text: &str| {
        issue(
            &kernel,
            request(
                "select",
                &[("query", text), ("as", "text/csv"), ("max_rows", max)],
            ),
        )
    };
    assert_eq!(lines(&rows("10", &ten).unwrap()), 11);
    assert_too_large(rows("9", &ten), "the answer exceeds 9 rows");
    // Asking for more than the space allows gets the space's bound.
    assert_too_large(
        rows("999999999", &product(2, 400)),
        &format!("the answer exceeds {DEFAULT_MAX_ROWS} rows"),
    );

    let whole = issue(&kernel, request("select", &[("query", &ten)])).unwrap();
    let bytes = |max: String| {
        issue(
            &kernel,
            request("select", &[("query", &ten), ("max_bytes", &max)]),
        )
    };
    assert_eq!(bytes(whole.len().to_string()).unwrap(), whole);
    assert_too_large(bytes((whole.len() - 1).to_string()), "bytes");

    // A bound that is not a positive whole number is refused, naming its argument.
    for (name, value) in [
        ("max_rows", "0"),
        ("max_rows", "ten"),
        ("max_bytes", "1MiB"),
    ] {
        match issue(
            &kernel,
            request("select", &[("query", &ten), (name, value)]),
        ) {
            Err(Error::InvalidArgument { name: got, .. }) => assert_eq!(got, name),
            other => panic!("{name}={value}: {other:?}"),
        }
    }
}

/// `CONSTRUCT` and `DESCRIBE` count TRIPLES against the same `max_rows=`, and say so.
#[test]
fn a_graph_answer_counts_triples() {
    let kernel = default_kernel();
    let text = construct_product(10, 10);
    let at = issue(
        &kernel,
        request(
            "construct",
            &[
                ("query", &text),
                ("as", "application/n-triples"),
                ("max_rows", "100"),
            ],
        ),
    )
    .unwrap();
    assert_eq!(lines(&at), 100);
    assert_too_large(
        issue(
            &kernel,
            request("construct", &[("query", &text), ("max_rows", "99")]),
        ),
        "the answer exceeds 99 triples",
    );
    assert_too_large(
        issue(
            &kernel,
            request("construct", &[("query", &text), ("max_bytes", "100")]),
        ),
        "the answer exceeds 100 bytes",
    );
    // DESCRIBE over the always-loaded vocabulary: hundreds of triples.
    assert_too_large(
        issue(
            &kernel,
            request(
                "describe",
                &[
                    ("query", "DESCRIBE ?s WHERE { ?s ?p ?o }"),
                    ("max_rows", "10"),
                ],
            ),
        ),
        "the answer exceeds 10 triples",
    );
}

/// `ASK` is exempt: its answer is one boolean, however large what it asks about.
#[test]
fn an_ask_is_exempt() {
    let body = issue(
        &bounded_kernel(1, 1),
        request(
            "ask",
            &[("query", "ASK { VALUES ?a { 1 2 3 } VALUES ?b { 1 2 3 } }")],
        ),
    )
    .unwrap();
    assert!(String::from_utf8(body).unwrap().contains("true"));
}

/// The shared-store space is bounded the same way.
#[test]
fn the_shared_store_space_is_bounded_too() {
    let store = Arc::new(Store::new().unwrap());
    ikigai_sparql::load_vocabulary(&store).unwrap();
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space_with_store_and_bounds(
        store,
        DEFAULT_BUDGET,
        AnswerBound::new(5, DEFAULT_MAX_BYTES).unwrap(),
    )));
    assert_too_large(
        issue(
            &kernel,
            request("select", &[("query", "SELECT * WHERE { ?s ?p ?o }")]),
        ),
        "the answer exceeds 5 rows",
    );
    issue(
        &kernel,
        request(
            "select",
            &[("query", "SELECT * WHERE { ?s ?p ?o } LIMIT 5")],
        ),
    )
    .unwrap();
}

/// A host cannot set a bound past the ceiling: that is a configuration mistake, said out loud.
#[test]
fn a_host_bound_past_the_ceiling_is_refused() {
    assert!(AnswerBound::new(AnswerBound::CEILING.rows() + 1, DEFAULT_MAX_BYTES).is_err());
    assert!(AnswerBound::new(DEFAULT_MAX_ROWS, AnswerBound::CEILING.bytes() + 1).is_err());
    assert_eq!(
        AnswerBound::new(AnswerBound::CEILING.rows(), AnswerBound::CEILING.bytes()).unwrap(),
        AnswerBound::CEILING
    );
}

/// A bound given BY REFERENCE (or as anything but inline UTF-8 text) is refused, naming it.
/// Before ledger #970 `budget=` given that way was ignored, and the space's ceiling applied: a
/// host door that stamps a budget only when none is present saw one and stamped nothing.
#[test]
fn a_bound_given_by_reference_is_refused_not_ignored() {
    let shared = Arc::new(Store::new().unwrap());
    let kernels = [
        default_kernel(),
        Kernel::new(Arc::new(ikigai_sparql::space_with_store(shared))),
    ];
    let elsewhere = Iri::parse("urn:example:a-large-number").unwrap();
    for kernel in &kernels {
        for name in ["budget", "max_rows", "max_bytes"] {
            for value in [
                ArgRef::Reference(elsewhere.clone()),
                ArgRef::Inline(vec![0xff, 0xfe]),
            ] {
                let req = request(
                    "select",
                    &[("query", "SELECT * WHERE { ?s ?p ?o } LIMIT 1")],
                )
                .with_arg(name, value.clone());
                match issue(kernel, req) {
                    Err(Error::InvalidArgument { name: got, detail }) => {
                        assert_eq!(got, name, "{detail}");
                        assert!(detail.contains("refused rather than ignored"), "{detail}");
                    }
                    other => panic!("{name} as {value:?}: {:?}", other.map(|b| b.len())),
                }
            }
        }
    }
    // And the update's `budget=`.
    let req = Request::new(Verb::Sink, Iri::parse("urn:sparql:update").unwrap())
        .with_arg(
            "content",
            ArgRef::Inline(b"INSERT DATA { <urn:a> <urn:b> <urn:c> }".to_vec()),
        )
        .with_arg("budget", ArgRef::Reference(elsewhere));
    match issue(&kernels[1], req) {
        Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "budget"),
        other => panic!("update budget by reference: {other:?}"),
    }
}

/// The time budget still applies beside the size bound: an inline `budget=` is honored.
#[test]
fn an_inline_budget_still_applies() {
    let body = issue(
        &default_kernel(),
        request(
            "select",
            &[
                ("query", &product(1, 3)),
                ("as", "text/csv"),
                ("budget", "1000"),
            ],
        ),
    )
    .unwrap();
    assert_eq!(lines(&body), 4);
}

/// The realistic measurement behind the report, not a gate: a 939-byte, 1,000,000-row `VALUES`
/// product serialized as JSON through the default space. Run it in a release build:
/// `cargo test --release -p ikigai-sparql --test sparql_answer_size -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_a_values_product() {
    let kernel = Kernel::new(Arc::new(ikigai_sparql::space()));
    let text = product(3, 100);
    let started = std::time::Instant::now();
    let outcome = issue(&kernel, request("select", &[("query", &text)]));
    let took = started.elapsed();
    match outcome {
        Ok(body) => println!(
            "MEASURE {} B query: answered {} B in {} ms",
            text.len(),
            body.len(),
            took.as_millis()
        ),
        Err(e) => println!(
            "MEASURE {} B query: refused in {} ms: {e}",
            text.len(),
            took.as_millis()
        ),
    }
}
