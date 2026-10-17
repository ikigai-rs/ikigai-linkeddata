//! An `http(s)://` `graph=` source is fetched through the HTTP module's GET, by its new name
//! `urn:http:get` (ikigai-http 0.2.0, ledger #11), and through the old `urn:httpGet` only on a
//! host that binds nothing at the new name.
//!
//! The claim this pins: ikigai-sparql has no Cargo edge to ikigai-http, so a host on ikigai-http
//! 0.1.x (which binds only `urn:httpGet`) can take this crate's release with nothing failing to
//! compile. Asking for `urn:http:get` alone would make every `graph=https://…` on that host
//! answer "no endpoint resolved", and only at run time. Before this change the fetch named
//! `urn:httpGet`, which ikigai-http 0.2.x keeps only as a deprecated alias until 0.3.0.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Error, Exact, FnEndpoint, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use std::sync::{Arc, Mutex};

const URL: &str = "https://example.org/people";
const NEW: &str = "urn:http:get";
const OLD: &str = "urn:httpGet";

/// Which GET names a test host binds, and whether its GET refuses.
#[derive(Clone, Copy)]
struct Host {
    new: bool,
    old: bool,
    refuses: bool,
}

/// A GET that records which name served it and the `url` it was asked for, and answers one
/// Turtle triple (or refuses).
fn get(name: &'static str, calls: Arc<Mutex<Vec<String>>>, refuses: bool) -> FnEndpoint {
    FnEndpoint::new(name, move |inv: &Invocation<'_>| {
        let url = inv.inline_str("url")?.to_string();
        calls.lock().unwrap().push(format!("{name} {url}"));
        if refuses {
            return Err(Error::Endpoint("the origin answered 503".to_string()));
        }
        let turtle = "<urn:ex:alice> <urn:ex:knows> <urn:ex:bob> .\n";
        Ok(
            Representation::new(ReprType::new("text/turtle"), turtle.as_bytes().to_vec())
                .cacheable(),
        )
    })
    .with_description(
        Description::new(name)
            .verb(Verb::Source)
            .input(ArgSpec::new("url")),
    )
}

fn kernel(host: Host, calls: &Arc<Mutex<Vec<String>>>) -> Kernel {
    let mut space = ikigai_sparql::space();
    if host.new {
        space = space.bind(Exact::new(NEW), get(NEW, Arc::clone(calls), host.refuses));
    }
    if host.old {
        space = space.bind(Exact::new(OLD), get(OLD, Arc::clone(calls), host.refuses));
    }
    Kernel::new(Arc::new(space))
}

fn run(host: Host) -> (ikigai_core::Result<String>, Vec<String>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let k = kernel(host, &calls);
    let query =
        format!("SELECT ?o WHERE {{ GRAPH <{URL}> {{ <urn:ex:alice> <urn:ex:knows> ?o }} }}");
    let request = Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap())
        .with_arg("query", ArgRef::Inline(query.into_bytes()))
        .with_arg("graph", ArgRef::Inline(URL.as_bytes().to_vec()))
        .with_arg("as", ArgRef::Inline(b"text/csv".to_vec()));
    let answer = block_on(k.issue(request, &Capability::root()))
        .map(|rep| String::from_utf8_lossy(&rep.bytes).into_owned());
    let calls = calls.lock().unwrap().clone();
    (answer, calls)
}

/// The new name is the one asked for.
#[test]
fn an_http_graph_is_fetched_through_urn_http_get() {
    let (answer, calls) = run(Host {
        new: true,
        old: false,
        refuses: false,
    });
    let answer = answer.unwrap_or_else(|e| panic!("query failed: {e}"));
    assert!(answer.contains("urn:ex:bob"), "answer: {answer}");
    assert_eq!(calls, [format!("{NEW} {URL}")]);
}

/// A host binding both names (ikigai-http 0.2.x binds the old one as an alias) is asked once,
/// at the new name.
#[test]
fn a_host_binding_both_names_is_asked_at_the_new_one_only() {
    let (answer, calls) = run(Host {
        new: true,
        old: true,
        refuses: false,
    });
    assert!(answer.unwrap().contains("urn:ex:bob"));
    assert_eq!(calls, [format!("{NEW} {URL}")]);
}

/// A host on ikigai-http 0.1.x binds only `urn:httpGet`, and its `graph=https://…` still works.
#[test]
fn a_host_binding_only_the_old_name_still_fetches() {
    let (answer, calls) = run(Host {
        new: false,
        old: true,
        refuses: false,
    });
    let answer = answer.unwrap_or_else(|e| panic!("query failed: {e}"));
    assert!(answer.contains("urn:ex:bob"), "answer: {answer}");
    assert_eq!(calls, [format!("{OLD} {URL}")]);
}

/// A bound GET's refusal is the answer: the fallback is for a name nothing binds, never a
/// second try at a fetch that failed.
#[test]
fn a_refusal_from_the_new_name_is_not_retried_at_the_old_one() {
    let (answer, calls) = run(Host {
        new: true,
        old: true,
        refuses: true,
    });
    let err = answer.expect_err("a refused fetch must fail the query");
    assert!(err.to_string().contains("503"), "error: {err}");
    assert_eq!(calls, [format!("{NEW} {URL}")]);
}

/// A host binding neither name is told the name to bind: the new one.
#[test]
fn a_host_binding_neither_name_is_told_the_new_one() {
    let (answer, calls) = run(Host {
        new: false,
        old: false,
        refuses: false,
    });
    let err = answer.expect_err("no GET is bound");
    let text = err.to_string();
    assert!(text.contains(NEW), "error: {text}");
    assert!(!text.contains(OLD), "error: {text}");
    assert!(calls.is_empty());
}
