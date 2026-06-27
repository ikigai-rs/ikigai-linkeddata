//! `ikigai-sparql` — SPARQL query as ikigai resources.
//!
//! `urn:sparql:select` / `:ask` / `:describe` / `:construct` run a `query=<sparql>` over
//! one or more `graph=<uri>` **sources resolved through the kernel** — a graph can be any
//! resolvable resource (a remote document via `urn:httpGet`, a file, a store's named
//! graph). Federation is just listing graphs: `graph=` takes a comma/space-separated list,
//! each loaded as a named graph (named by its URI) with the query's default graph set to
//! their union — so simple queries span all of them and `GRAPH <uri> { … }` addresses one.
//!
//! The **ikigai vocabulary** (`urn:ikigai:vocab` — the `ns#` ontology: `ik:Transreptor
//! rdfs:subClassOf ik:Endpoint` and the property defs) is **always loaded** as a named
//! graph and folded into the union default graph, so a catalog query can join endpoint
//! *instances* against the *schema* with no extra `graph=` — e.g.
//! `?e rdf:type/rdfs:subClassOf* ik:Endpoint` walks the class hierarchy (Oxigraph has no
//! reasoner, so the `subClassOf` axiom is traversed explicitly via a property path).
//! Because `graph=` is therefore optional, a query may run against the vocabulary alone.
//!
//! Results content-negotiate via `as=`: SELECT/ASK serialize as `application/sparql-
//! results+json` (default) / `+xml` / `text/csv` / `text/tab-separated-values`;
//! CONSTRUCT/DESCRIBE serialize as RDF (`text/turtle` default / N-Triples / …), which
//! composes with `urn:rdf:transrept` for an HTML view.
//!
//! The result is `.cacheable()` and — because each graph is resolved with `inv.source` —
//! depends on every source's golden thread, so it is cached and auto-invalidated when any
//! source changes. Built on Oxigraph's in-memory store (no rocksdb); runs in the browser.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, NamedNodeRef};
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

/// The four SPARQL verbs as resources. All four resolve identically — the query form
/// (SELECT/ASK/CONSTRUCT/DESCRIBE) determines the result shape — but they're distinct,
/// discoverable IRIs.
pub fn space() -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for verb in ["select", "ask", "describe", "construct"] {
        space = space.bind(Exact::new(format!("urn:sparql:{verb}")), SparqlEndpoint);
    }
    space
}

/// A fresh in-memory store pre-seeded with the bundled ikigai vocabulary, loaded as the
/// named graph `urn:ikigai:vocab`. Bundled (`include_str!` via `ikigai_vocab::VOCABULARY`)
/// rather than resolved through the kernel, so the schema is present even when no host
/// mounts `urn:ikigai:vocab`; it's static, so it carries no golden thread and doesn't
/// affect cacheability. Callers add their `graph=` sources on top, then union the default
/// graph — so `?e rdf:type/rdfs:subClassOf* ik:Endpoint` joins instances to this schema.
fn store_with_vocabulary() -> Result<Store> {
    let store = Store::new().map_err(|e| Error::Endpoint(format!("store init: {e}")))?;
    let vocab_graph = NamedNodeRef::new(ikigai_vocab::VOCAB_IRI)
        .map_err(|e| Error::Endpoint(format!("vocab graph name: {e}")))?;
    store
        .load_from_slice(
            RdfParser::from_format(RdfFormat::Turtle).with_default_graph(vocab_graph),
            ikigai_vocab::VOCABULARY.as_bytes(),
        )
        .map_err(|e| Error::Endpoint(format!("loading the ikigai vocabulary: {e}")))?;
    Ok(store)
}

struct SparqlEndpoint;

#[async_trait]
impl Endpoint for SparqlEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let query_str = inv.inline_str("query").map_err(|_| {
            Error::Endpoint("urn:sparql:* needs a `query=<sparql>` argument".to_string())
        })?;
        // `graph=` is optional: the vocabulary graph (below) is always present, so a query
        // can run against it alone. Listed sources federate on top of it.
        let graph_list = inv.inline_str("graph").unwrap_or("");

        // Build the dataset, pre-seeded with the bundled ikigai vocabulary (see
        // `store_with_vocabulary`). Each listed source is resolved through the kernel and
        // loaded as a named graph (named by its URI). `inv.source` records the source's
        // golden thread, so the cached result invalidates when any source changes.
        let store = store_with_vocabulary()?;
        for uri in graph_list
            .split([',', ' ', '\n', '\t'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let source = resolve_graph(inv, uri).await?;
            let format = rdf_format(source.repr_type.media_type.as_str())
                .unwrap_or_else(|| sniff(&source.bytes));
            let graph = NamedNodeRef::new(uri)
                .map_err(|e| Error::Endpoint(format!("graph name `{uri}` is not an IRI: {e}")))?;
            store
                .load_from_slice(
                    RdfParser::from_format(format).with_default_graph(graph),
                    &source.bytes,
                )
                .map_err(|e| Error::Endpoint(format!("loading <{uri}>: {e}")))?;
        }

        let mut prepared = SparqlEvaluator::new()
            .parse_query(query_str)
            .map_err(|e| Error::Endpoint(format!("SPARQL syntax error: {e}")))?;
        // Default graph = the union of the loaded named graphs, so a query without an
        // explicit GRAPH/FROM spans every source.
        prepared.dataset_mut().set_default_graph_as_union();
        let results = prepared
            .on_store(&store)
            .execute()
            .map_err(|e| Error::Endpoint(format!("query evaluation error: {e}")))?;

        let (media, bytes) = serialize_results(results, inv.inline_str("as").ok())?;
        Ok(
            Representation::new(ReprType::new(&media).with_param("charset", "utf-8"), bytes)
                .cacheable(),
        )
    }

    fn name(&self) -> &str {
        "sparql"
    }

    fn describe(&self) -> Description {
        Description::new("sparql")
            .title("SPARQL query")
            .summary(
                "Run a SPARQL query over one or more resolvable, cacheable graphs \
                 (federation by listing graphs). The ikigai vocabulary (urn:ikigai:vocab) \
                 is always loaded, so endpoint instances can join the class/property schema.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("query").summary("the SPARQL query (SELECT/ASK/DESCRIBE/CONSTRUCT)"),
            )
            .input(ArgSpec::new("graph").summary(
                "optional: one or more graph source IRIs, comma- or space-separated; each \
                 resolved through the kernel and loaded as a named graph. Omit to query the \
                 always-present ikigai vocabulary alone.",
            ))
            .input(ArgSpec::new("as").summary(
                "result representation: SELECT/ASK → application/sparql-results+json \
                 (default), +xml, text/csv, text/tab-separated-values; CONSTRUCT/DESCRIBE \
                 → text/turtle (default), application/n-triples, …",
            ))
            .output("application/sparql-results+json")
    }
}

/// Resolve a graph source through the kernel. An `http(s)://` URL is fetched via the
/// HTTP module (`urn:httpGet`) — a bare URL isn't itself a bound resource — while a
/// `urn:`/`file:` graph resolves directly. Either way the kernel records the source's
/// golden thread, so the query result is cacheable and invalidates when the graph changes.
async fn resolve_graph(inv: &Invocation<'_>, uri: &str) -> Result<Representation> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        let get = Iri::parse("urn:httpGet").expect("urn:httpGet is a valid IRI");
        let request = Request::new(Verb::Source, get)
            .with_arg("url", ArgRef::Inline(uri.as_bytes().to_vec()));
        inv.issue(request).await
    } else {
        let iri =
            Iri::parse(uri).map_err(|e| Error::Endpoint(format!("bad graph IRI `{uri}`: {e}")))?;
        inv.source(&iri).await
    }
}

/// Serialize query results by their kind, honoring the `as` representation.
fn serialize_results(results: QueryResults, as_type: Option<&str>) -> Result<(String, Vec<u8>)> {
    let io = |e: std::io::Error| Error::Endpoint(format!("serialize: {e}"));
    match results {
        QueryResults::Solutions(solutions) => {
            let format = results_format(as_type).unwrap_or(QueryResultsFormat::Json);
            let variables = solutions.variables().to_vec();
            let mut serializer = QueryResultsSerializer::from_format(format)
                .serialize_solutions_to_writer(Vec::new(), variables)
                .map_err(io)?;
            for solution in solutions {
                let solution = solution.map_err(|e| Error::Endpoint(format!("query: {e}")))?;
                serializer.serialize(&solution).map_err(io)?;
            }
            Ok((
                format.media_type().to_string(),
                serializer.finish().map_err(io)?,
            ))
        }
        QueryResults::Boolean(value) => {
            let format = results_format(as_type).unwrap_or(QueryResultsFormat::Json);
            let bytes = QueryResultsSerializer::from_format(format)
                .serialize_boolean_to_writer(Vec::new(), value)
                .map_err(io)?;
            Ok((format.media_type().to_string(), bytes))
        }
        QueryResults::Graph(triples) => {
            let format = rdf_format(as_type.unwrap_or("text/turtle")).unwrap_or(RdfFormat::Turtle);
            let mut serializer = RdfSerializer::from_format(format).for_writer(Vec::new());
            for triple in triples {
                let triple = triple.map_err(|e| Error::Endpoint(format!("query: {e}")))?;
                serializer
                    .serialize_quad(&triple.in_graph(GraphName::DefaultGraph))
                    .map_err(io)?;
            }
            Ok((
                format.media_type().to_string(),
                serializer.finish().map_err(io)?,
            ))
        }
    }
}

/// SELECT/ASK result format from an `as` media type or short alias.
fn results_format(as_type: Option<&str>) -> Option<QueryResultsFormat> {
    let media = media_base(as_type?);
    if let Some(format) = QueryResultsFormat::from_media_type(media) {
        return Some(format);
    }
    Some(match media {
        "json" => QueryResultsFormat::Json,
        "xml" => QueryResultsFormat::Xml,
        "csv" => QueryResultsFormat::Csv,
        "tsv" => QueryResultsFormat::Tsv,
        _ => return None,
    })
}

/// RDF format (for CONSTRUCT/DESCRIBE output and for loading a source) from a media type
/// or short alias.
fn rdf_format(spec: &str) -> Option<RdfFormat> {
    let media = media_base(spec);
    if let Some(format) = RdfFormat::from_media_type(media) {
        return Some(format);
    }
    Some(match media {
        "turtle" | "ttl" => RdfFormat::Turtle,
        "ntriples" | "nt" | "n-triples" => RdfFormat::NTriples,
        "nquads" | "nq" | "n-quads" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdfxml" | "rdf/xml" | "xml" => RdfFormat::RdfXml,
        "jsonld" | "json-ld" | "json" => {
            RdfFormat::from_media_type("application/ld+json").expect("ld+json")
        }
        _ => return None,
    })
}

/// The bare media type (strip parameters and surrounding whitespace).
fn media_base(media: &str) -> &str {
    media.split(';').next().unwrap_or(media).trim()
}

/// Sniff an input graph's syntax when its content-type isn't a known RDF media type:
/// `{`/`[` ⇒ JSON-LD; a leading IRI `<scheme://…>` ⇒ Turtle; a leading XML element ⇒
/// RDF/XML; else Turtle (subsumes N-Triples). Same discriminator as ikigai-rdf.
fn sniff(bytes: &[u8]) -> RdfFormat {
    let rest = &bytes[bytes.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
    match rest.first() {
        Some(b'{') | Some(b'[') => {
            RdfFormat::from_media_type("application/ld+json").expect("ld+json")
        }
        Some(b'<') => {
            let token: Vec<u8> = rest
                .iter()
                .take_while(|&&b| b != b'>' && !b.is_ascii_whitespace())
                .copied()
                .collect();
            if token.windows(3).any(|w| w == b"://") {
                RdfFormat::Turtle
            } else {
                RdfFormat::RdfXml
            }
        }
        _ => RdfFormat::Turtle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"@prefix ex: <http://ex/> . ex:a ex:name "Ada" ; ex:knows ex:b ."#;
    const B: &str = r#"@prefix ex: <http://ex/> . ex:b ex:name "Bob" ."#;

    /// Test helper: load named graphs from inline Turtle, run a query, return (media, body).
    /// Pre-seeds the bundled vocabulary exactly as production does (`store_with_vocabulary`).
    fn run(graphs: &[(&str, &str)], query: &str, as_type: Option<&str>) -> (String, String) {
        let store = store_with_vocabulary().unwrap();
        for (uri, ttl) in graphs {
            store
                .load_from_slice(
                    RdfParser::from_format(RdfFormat::Turtle)
                        .with_default_graph(NamedNodeRef::new(uri).unwrap()),
                    ttl.as_bytes(),
                )
                .unwrap();
        }
        let mut prepared = SparqlEvaluator::new().parse_query(query).unwrap();
        prepared.dataset_mut().set_default_graph_as_union();
        let results = prepared.on_store(&store).execute().unwrap();
        let (media, bytes) = serialize_results(results, as_type).unwrap();
        (media, String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn select_over_a_graph_returns_json_solutions() {
        let (media, body) = run(
            &[("http://g/a", A)],
            "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
            None,
        );
        assert!(media.contains("sparql-results+json"));
        assert!(body.contains("Ada"));
    }

    #[test]
    fn ask_returns_a_boolean() {
        let (_, yes) = run(
            &[("http://g/a", A)],
            "ASK { ?s <http://ex/name> \"Ada\" }",
            None,
        );
        assert!(yes.contains("true"));
        let (_, no) = run(
            &[("http://g/a", A)],
            "ASK { ?s <http://ex/name> \"Nope\" }",
            None,
        );
        assert!(no.contains("false"));
    }

    #[test]
    fn construct_emits_rdf_for_transreption() {
        let (media, ttl) = run(
            &[("http://g/a", A)],
            "CONSTRUCT { ?s <http://ex/label> ?n } WHERE { ?s <http://ex/name> ?n }",
            Some("text/turtle"),
        );
        assert!(media.contains("turtle"));
        assert!(ttl.contains("http://ex/label"));
        assert!(ttl.contains("Ada"));
    }

    #[test]
    fn federation_unions_graphs_yet_keeps_them_addressable() {
        // The union default graph spans both sources…
        let (_, both) = run(
            &[("http://g/a", A), ("http://g/b", B)],
            "SELECT ?name WHERE { ?s <http://ex/name> ?name }",
            Some("text/csv"),
        );
        assert!(
            both.contains("Ada") && both.contains("Bob"),
            "union default spans graphs"
        );
        // …and each graph stays addressable by its URI.
        let (_, only_b) = run(
            &[("http://g/a", A), ("http://g/b", B)],
            "SELECT ?name WHERE { GRAPH <http://g/b> { ?s <http://ex/name> ?name } }",
            Some("text/csv"),
        );
        assert!(
            only_b.contains("Bob") && !only_b.contains("Ada"),
            "named graph isolates"
        );
    }

    /// A catalog-like graph holds endpoint *instances*; the always-loaded vocabulary holds
    /// the `ik:Transreptor rdfs:subClassOf ik:Endpoint` axiom. A property-path query joins
    /// them — finding the transreptor as an ik:Endpoint with no reasoner and no extra graph.
    const CATALOG: &str = r#"@prefix ik: <https://ikigai-rs.dev/ns#> .
        <urn:fn:toUpper>     a ik:Endpoint ; ik:id "toUpper" .
        <urn:rdf:transrept>  a ik:Endpoint, ik:Transreptor ; ik:id "rdf-transrept" ;
                             ik:transreptsFrom "text/turtle" ; ik:transreptsTo "text/html" ."#;

    #[test]
    fn vocabulary_is_always_present_for_schema_queries() {
        // No `graph=` at all: the bundled vocabulary alone answers a schema question.
        let (_, body) = run(
            &[],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             ASK { ik:Transreptor rdfs:subClassOf ik:Endpoint }",
            None,
        );
        assert!(
            body.contains("true"),
            "vocab carries the subClassOf axiom: {body}"
        );
    }

    #[test]
    fn subclass_path_finds_transreptors_as_endpoints() {
        // Walk rdf:type/rdfs:subClassOf*: every endpoint, transreptors included, via the
        // axiom from the always-present vocabulary joined to the catalog instances.
        let (_, all) = run(
            &[("urn:kernel:catalog", CATALOG)],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             SELECT ?id WHERE { ?e a/rdfs:subClassOf* ik:Endpoint ; ik:id ?id }",
            Some("text/csv"),
        );
        assert!(all.contains("toUpper"), "plain endpoint: {all}");
        assert!(
            all.contains("rdf-transrept"),
            "transreptor counts as an endpoint: {all}"
        );

        // And the transreptor is selectable by its declared conversion.
        let (_, html_producers) = run(
            &[("urn:kernel:catalog", CATALOG)],
            "PREFIX ik: <https://ikigai-rs.dev/ns#> \
             SELECT ?id WHERE { ?e a ik:Transreptor ; ik:id ?id ; ik:transreptsTo \"text/html\" }",
            Some("text/csv"),
        );
        assert!(
            html_producers.contains("rdf-transrept") && !html_producers.contains("toUpper"),
            "only the transreptor that produces text/html: {html_producers}"
        );
    }
}
