//! No SPARQL text handed to this crate reaches the network: `SERVICE` and `LOAD` are refused,
//! typed, before evaluation, and the evaluator could not fetch even if one got past that
//! (ledger #1083, the history in ledger #145).
//!
//! The claim: this crate takes oxigraph with `default-features = false`, so `SERVICE <http://…>`
//! and `LOAD <http://…>` fail with "not supported" in its own build. But Cargo unifies features
//! across a host's whole graph, and rudof_rdf turns on `oxigraph/http-client` natively, so in
//! every host that links ikigai-shacl (ikigai-cli, ikigai-web-demo's server) oxigraph's
//! `SparqlEvaluator` installs its default HTTP service handler and `LOAD` gets an HTTP client.
//! Then a query string makes an outbound request that never passes through `urn:httpGet`, and no
//! `urn:cap:net:*` is consulted. It held: the two `oxigraph_alone_*` tests below are that
//! reproduction, and they still pass, because they use oxigraph without this crate.
//!
//! The file runs in BOTH builds. Under `--features ikigai-sparql/http-client-probe` (CI's
//! `features:` job) the client is compiled in, the `oxigraph_alone_*` tests prove it is live
//! (without them, "the stub saw nothing" would be vacuous), and every door is shown to send the
//! stub nothing. In the default build the doors' refusals are pinned the same way, so neither
//! build regresses the other.
//!
//! The stub is a plain TCP listener on 127.0.0.1 at an ephemeral port: it answers any request
//! with a SPARQL JSON result or an N-Triples document, so a leak is a SUCCESS that brings data
//! in from the network, and counts every connection. No real host is ever named.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, EndpointSpace, Error, Iri, Kernel, Request, Verb};
use ikigai_sparql::Store;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A local HTTP stub that counts the connections it accepts.
struct Stub {
    base: String,
    hits: Arc<AtomicUsize>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        // Detached: the thread blocks in `accept` and dies with the test process.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = std::io::Read::read_exact(&mut reader, &mut body);
                let (media, payload) = if request_line.contains("/load") {
                    (
                        "application/n-triples",
                        "<urn:stub:s> <urn:stub:p> \"from-the-network\" .\n".to_string(),
                    )
                } else {
                    (
                        "application/sparql-results+json",
                        r#"{"head":{"vars":["s","p","o"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:stub:s"},"p":{"type":"uri","value":"urn:stub:p"},"o":{"type":"literal","value":"from-the-network"}}]}}"#
                            .to_string(),
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
        });
        Stub { base, hits }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// The SPARQL texts that would fetch, each with the door it goes to and the argument its text
/// travels in. `{s}` is the stub's base IRI.
fn cases(s: &str) -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "select",
            "query",
            format!("SELECT * WHERE {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        (
            "ask",
            "query",
            format!("ASK {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        (
            "construct",
            "query",
            format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        (
            "describe",
            "query",
            format!("DESCRIBE ?s WHERE {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        (
            "select",
            "query",
            format!("SELECT * WHERE {{ SERVICE SILENT <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        // The service named by a variable, bound at evaluation time: no IRI in the text.
        (
            "select",
            "query",
            format!(
                "SELECT * WHERE {{ VALUES ?svc {{ <{s}/sparql> }} SERVICE ?svc {{ ?s ?p ?o }} }}"
            ),
        ),
        // Buried in an expression, where a walk of graph patterns alone would not look.
        (
            "select",
            "query",
            format!(
                "SELECT * WHERE {{ BIND(1 AS ?x) FILTER EXISTS {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }} }}"
            ),
        ),
        (
            "update",
            "update",
            format!("INSERT {{ ?s ?p ?o }} WHERE {{ SERVICE <{s}/sparql> {{ ?s ?p ?o }} }}"),
        ),
        ("update", "update", format!("LOAD <{s}/load>")),
        ("update", "update", format!("LOAD SILENT <{s}/load> INTO GRAPH <urn:g>")),
        (
            "update",
            "update",
            format!("INSERT DATA {{ <urn:a> <urn:b> <urn:c> }} ; LOAD <{s}/load>"),
        ),
    ]
}

/// Both query doors (the per-query dataset and the shared store); the update door exists only
/// on the shared store.
fn doors() -> Vec<(&'static str, Kernel, Arc<Store>)> {
    let store = Arc::new(Store::new().unwrap());
    let per_query: EndpointSpace = ikigai_sparql::space();
    vec![
        (
            "space_with_store",
            Kernel::new(Arc::new(ikigai_sparql::space_with_store(Arc::clone(
                &store,
            )))),
            store,
        ),
        (
            "space",
            Kernel::new(Arc::new(per_query)),
            Arc::new(Store::new().unwrap()),
        ),
    ]
}

fn issue(kernel: &Kernel, form: &str, arg: &str, text: &str) -> ikigai_core::Result<Vec<u8>> {
    let verb = if form == "update" {
        Verb::Sink
    } else {
        Verb::Source
    };
    let request = Request::new(verb, Iri::parse(format!("urn:sparql:{form}")).unwrap())
        .with_arg(arg, ArgRef::Inline(text.as_bytes().to_vec()));
    block_on(kernel.issue(request, &Capability::root())).map(|r| r.bytes)
}

/// Every door, every fetching text: the stub sees no connection, the answer is a typed
/// refusal naming the argument the text came in, and an update leaves the store untouched.
#[test]
fn no_door_reaches_the_network_and_each_refusal_is_typed() {
    let stub = Stub::start();
    let mut failures = Vec::new();
    for (door, kernel, store) in doors() {
        for (form, arg, text) in cases(&stub.base) {
            if form == "update" && door == "space" {
                continue;
            }
            let before = stub.hits();
            let got = issue(&kernel, form, arg, &text);
            if stub.hits() != before {
                failures.push(format!(
                    "{door} urn:sparql:{form}: `{text}` CONNECTED to the stub ({} request(s))",
                    stub.hits() - before
                ));
            }
            match got {
                Err(Error::InvalidArgument { name, detail }) if name == arg => {
                    if !detail.contains("urn:cap:net") {
                        failures.push(format!(
                            "{door} urn:sparql:{form}: `{text}` refused without naming \
                             urn:cap:net: {detail}"
                        ));
                    }
                }
                Err(other) => failures.push(format!(
                    "{door} urn:sparql:{form}: `{text}` failed, but not as a typed refusal of \
                     `{arg}`: {other:?}"
                )),
                Ok(bytes) => failures.push(format!(
                    "{door} urn:sparql:{form}: `{text}` SUCCEEDED: {}",
                    String::from_utf8_lossy(&bytes)
                )),
            }
        }
        if store.len().unwrap() != 0 {
            failures.push(format!(
                "{door}: the store holds {} quad(s) after only refused updates",
                store.len().unwrap()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The refusal is about the text, not the store: the same doors still answer a query that
/// does not fetch.
#[test]
fn a_query_without_service_is_untouched() {
    for (door, kernel, _) in doors() {
        let csv = issue(
            &kernel,
            "select",
            "query",
            "SELECT ?x WHERE { BIND(\"local\" AS ?x) }",
        )
        .unwrap_or_else(|e| panic!("{door}: {e}"));
        assert!(String::from_utf8(csv).unwrap().contains("local"), "{door}");
    }
}

/// The reproduction, kept as the proof that the probe build really has the client: oxigraph's
/// own `SparqlEvaluator`, built the way this crate built it before ledger #1083, sends
/// `SERVICE` to the stub and returns the stub's row.
#[cfg(feature = "http-client-probe")]
#[test]
fn oxigraph_alone_sends_service_to_the_network_when_the_client_is_compiled_in() {
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    let stub = Stub::start();
    let store = Store::new().unwrap();
    let query = format!(
        "SELECT ?o WHERE {{ SERVICE <{}/sparql> {{ ?s ?p ?o }} }}",
        stub.base
    );
    let results = SparqlEvaluator::new()
        .parse_query(&query)
        .unwrap()
        .on_store(&store)
        .execute()
        .unwrap();
    let QueryResults::Solutions(solutions) = results else {
        panic!("not solutions")
    };
    let rows: Vec<_> = solutions.collect::<Result<_, _>>().unwrap();
    assert_eq!(
        stub.hits(),
        1,
        "oxigraph alone contacts the SERVICE endpoint"
    );
    assert_eq!(
        rows[0].get("o").unwrap().to_string(),
        "\"from-the-network\""
    );
}

/// The same for `LOAD`, which takes no service handler at all: oxigraph fetches it with its
/// own client whenever the feature is on, so only refusing it before evaluation can stop it.
#[cfg(feature = "http-client-probe")]
#[test]
fn oxigraph_alone_fetches_load_when_the_client_is_compiled_in() {
    use oxigraph::sparql::SparqlEvaluator;
    let stub = Stub::start();
    let store = Store::new().unwrap();
    SparqlEvaluator::new()
        .parse_update(&format!("LOAD <{}/load>", stub.base))
        .unwrap()
        .on_store(&store)
        .execute()
        .unwrap();
    assert_eq!(stub.hits(), 1, "oxigraph alone fetches the LOAD source");
    assert_eq!(store.len().unwrap(), 1, "and loads what it fetched");
}
