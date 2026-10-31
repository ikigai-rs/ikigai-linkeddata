//! Bounded OWL 2 RL materialization through `reasonable`'s in-memory API.
//!
//! `urn:reason:closure` returns the RDF merge of `graph=` sources plus entailments;
//! `urn:reason:inferred` returns that closure minus the input. Both are cacheable
//! and resolve sources through the kernel, inheriting their golden threads.
//! Mount [`ikigai_rdf::space`] beside this space for non-Turtle output faces.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use ikigai_core::{
    space_iri, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, FailureClass,
    Invocation, Iri, ReprType, Representation, Request, Result, Verb,
};
use ikigai_rdf::{canonical_triples, serialize_turtle, BlankNodeLabels};
use oxrdf::Triple;
use oxrdfio::{RdfFormat, RdfParser};
use reasonable::reasoner::Reasoner;
use std::collections::{BTreeMap, HashSet};

/// Default maximum number of parsed input triples (duplicates count).
pub const DEFAULT_MAX_INPUT_TRIPLES: usize = 1_000;
/// Default maximum number of distinct closure triples, including input and axioms.
pub const DEFAULT_MAX_OUTPUT_TRIPLES: usize = 100_000;
/// The identity claimed only by the configuration-free [`space`].
pub const SPACE_ID: &str = "urn:iki:space:reason";

/// Host-configured refusal bounds; neither bound truncates a graph.
///
/// The input bound is checked BEFORE calling `reason()`: that call cannot be
/// interrupted, and wasm has no worker thread on which to time it out. Bounding
/// input therefore bounds the work admitted to the reasoner, rather than promising
/// a wall-clock deadline. The output bound is checked AFTER materialization: it
/// limits the answer, not peak memory or time inside `reasonable`. Closure can
/// grow quadratically (or worse for some rule combinations).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_triples: usize,
    pub max_output_triples: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_triples: DEFAULT_MAX_INPUT_TRIPLES,
            max_output_triples: DEFAULT_MAX_OUTPUT_TRIPLES,
        }
    }
}

/// The two pure, cacheable reasoning resources with default bounds.
pub fn space() -> EndpointSpace {
    space_with_limits(Limits::default()).named(space_iri("reason"))
}

/// The same resources under host-selected bounds. The host names this space.
pub fn space_with_limits(limits: Limits) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new("urn:reason:closure"),
            ReasonEndpoint {
                inferred: false,
                limits,
            },
        )
        .bind(
            Exact::new("urn:reason:inferred"),
            ReasonEndpoint {
                inferred: true,
                limits,
            },
        )
}

const FACES: [&str; 7] = [
    "text/turtle",
    "application/n-triples",
    "application/n-quads",
    "application/trig",
    "application/rdf+xml",
    "application/ld+json",
    "text/html",
];
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}
fn over(which: &str, limit: usize) -> Error {
    Error::OverBudget {
        detail: format!("OWL RL {which} exceeds {limit} triples"),
    }
}

struct ReasonEndpoint {
    inferred: bool,
    limits: Limits,
}

#[async_trait]
impl Endpoint for ReasonEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let graph = inv.inline_str("graph")?;
        let profile = optional(inv, "profile")?.unwrap_or("owl-rl");
        if profile != "owl-rl" {
            return Err(invalid("profile", "expected owl-rl"));
        }
        let omit_thing = match optional(inv, "omit-thing")?.unwrap_or("false") {
            "true" => true,
            "false" => false,
            _ => return Err(invalid("omit-thing", "expected true or false")),
        };
        let face = optional(inv, "as")?.unwrap_or("text/turtle");
        if !FACES.contains(&face) {
            return Err(invalid("as", format!("unsupported RDF face: {face}")));
        }
        let uris: Vec<_> = graph
            .split([',', ' ', '\n', '\r', '\t'])
            .filter(|s| !s.is_empty())
            .collect();
        if uris.is_empty() {
            return Err(invalid("graph", "expected one or more graph IRIs"));
        }
        // Validate the entire list before resolving anything.
        for uri in &uris {
            Iri::parse(*uri).map_err(|e| invalid("graph", e.to_string()))?;
        }
        let mut input = HashSet::new();
        let mut count = 0;
        let mut prefixes = BTreeMap::new();
        let mut sources = Vec::new();
        let mut labels = BlankNodeLabels::new();
        // Repeating a graph IRI names the same graph, not a fresh blank-node scope.
        let mut seen = HashSet::new();
        for uri in uris {
            if !seen.insert(uri) {
                continue;
            }
            let source = resolve_graph(inv, uri).await?;
            let media = source
                .repr_type
                .media_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim();
            let format = RdfFormat::from_media_type(media)
                .ok_or_else(|| invalid("graph", format!("<{uri}> is not RDF ({media})")))?;
            let remaining = self.limits.max_input_triples.saturating_sub(count);
            let (triples, source_prefixes) = parse_source(
                uri,
                format,
                &source.bytes,
                remaining,
                self.limits.max_input_triples,
            )?;
            // First source wins when a prefix name has conflicting meanings.
            // Unabbreviated IRIs remain lossless; never reinterpret a triple.
            for (name, iri) in source_prefixes {
                prefixes.entry(name).or_insert(iri);
            }
            sources.push(uri);
            count += triples.len();
            labels.new_document();
            input.extend(triples.into_iter().map(|triple| labels.triple(triple)));
        }
        let triples = materialize(input, &labels, self.inferred, omit_thing, self.limits)?;
        // Comments identify the merged sources without adding provenance triples
        // to the closure. IRIs were validated before resolution.
        let mut bytes = format!(
            "# OWL 2 RL {}\n",
            if self.inferred { "inferred" } else { "closure" }
        )
        .into_bytes();
        for uri in sources {
            bytes.extend_from_slice(format!("# Source graph: <{uri}>\n").as_bytes());
        }
        if omit_thing {
            bytes.extend_from_slice(b"# Inferred rdf:type owl:Thing triples omitted\n");
        }
        bytes.extend(serialize_turtle(triples, &prefixes)?);
        if face == "text/turtle" {
            return Ok(Representation::new(ReprType::new("text/turtle"), bytes).cacheable());
        }
        let request = Request::new(Verb::Source, Iri::parse("urn:rdf:transrept").unwrap())
            .with_arg("content", ArgRef::Inline(bytes))
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        inv.issue(request).await
    }

    fn name(&self) -> &str {
        if self.inferred {
            "reason-inferred"
        } else {
            "reason-closure"
        }
    }

    fn describe(&self) -> Description {
        let desc = Description::new(self.name())
            .summary("Bounded OWL 2 RL materialization of kernel-resolved RDF graphs; closure or input-subtracted entailments")
            .verb(Verb::Source).verb(Verb::Meta)
            .input(ArgSpec::new("graph").class(XSD_STRING)
                .summary("one or more graph IRIs, comma- or whitespace-separated; RDF merge across documents"))
            .input(ArgSpec::new("profile").class(XSD_STRING).optional()
                .one_of(["owl-rl"]).default_value("owl-rl"))
            .input(ArgSpec::new("omit-thing").class(XSD_STRING).optional()
                .summary("omit inferred rdf:type owl:Thing triples; explicit input is preserved")
                .one_of(["true", "false"]).default_value("false"))
            .input(ArgSpec::new("as").class(XSD_STRING).optional()
                .one_of(FACES).default_value("text/turtle"))
            .fails(FailureClass::OverBudget);
        FACES.iter().fold(desc, |desc, face| desc.output(*face))
    }
}

fn optional<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    match inv.inline_str(name) {
        Ok(value) => Ok(Some(value)),
        Err(Error::MissingArgument(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn resolve_graph(inv: &Invocation<'_>, uri: &str) -> Result<Representation> {
    let result = if uri.starts_with("http://") || uri.starts_with("https://") {
        inv.issue(
            Request::new(Verb::Source, Iri::parse("urn:http:get").unwrap())
                .with_arg("url", ArgRef::Inline(uri.as_bytes().to_vec())),
        )
        .await
    } else {
        inv.source(&Iri::parse(uri).map_err(|e| invalid("graph", e.to_string()))?)
            .await
    };
    match result {
        Err(Error::Unresolved(iri)) if iri.as_str() == uri => Err(Error::NotFound(uri.into())),
        other => other,
    }
}

fn parse_source(
    uri: &str,
    format: RdfFormat,
    bytes: &[u8],
    remaining: usize,
    limit: usize,
) -> Result<(Vec<Triple>, BTreeMap<String, String>)> {
    match format {
        RdfFormat::RdfXml => ikigai_rdf::check_rdfxml_nesting(bytes, "graph")?,
        RdfFormat::JsonLd { .. } => ikigai_rdf::check_json_nesting(bytes, "graph")?,
        _ => ikigai_rdf::check_turtle_nesting(&String::from_utf8_lossy(bytes), "graph")?,
    }
    let parse = || {
        let mut triples = Vec::new();
        let mut parser = RdfParser::from_format(format)
            .with_base_iri(uri)
            .map_err(|e| invalid("graph", e.to_string()))?
            .for_slice(bytes);
        for quad in parser.by_ref() {
            let quad = quad.map_err(|e| invalid("graph", format!("<{uri}>: {e}")))?;
            if triples.len() == remaining {
                return Err(over("input", limit));
            }
            #[cfg(feature = "rdf-12")]
            if matches!(quad.object, oxrdf::Term::Triple(_)) {
                return Err(invalid(
                    "graph",
                    "OWL 2 RL reasoning accepts RDF 1.1, not RDF 1.2 triple terms",
                ));
            }
            triples.push(Triple::new(quad.subject, quad.predicate, quad.object));
        }
        let prefixes = parser
            .prefixes()
            .map(|(name, iri)| (name.to_owned(), iri.to_owned()))
            .collect();
        Ok((triples, prefixes))
    };
    // As in ikigai-sparql: depth scans precede recursive parsers, and native
    // parsing gets the stack needed by bounded JSON-LD nesting in debug builds.
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(ikigai_rdf::JSON_LD_STACK)
                .spawn_scoped(scope, parse)
                .map_err(|e| Error::Endpoint(e.to_string()))?
                .join()
                .map_err(|_| Error::Endpoint("RDF parser panicked".into()))?
        })
    }
    #[cfg(target_family = "wasm")]
    parse()
}

fn materialize(
    input: HashSet<Triple>,
    labels: &BlankNodeLabels,
    inferred: bool,
    omit_thing: bool,
    limits: Limits,
) -> Result<Vec<Triple>> {
    let mut reasoner = Reasoner::new();
    // Sort the seed as well as the answer: hash iteration must not affect output.
    let seed = canonical_triples(input.iter().cloned(), |_| true);
    reasoner.load_triples(seed);
    reasoner.reason();
    let mut closure = input.clone();
    for triple in reasoner.view_output() {
        closure.insert(triple.clone());
        if closure.len() > limits.max_output_triples {
            return Err(over("closure", limits.max_output_triples));
        }
    }
    if closure.len() > limits.max_output_triples {
        return Err(over("closure", limits.max_output_triples));
    }
    Ok(canonical_triples(
        closure
            .into_iter()
            .filter(|t| !inferred || !input.contains(t))
            .filter(|t| {
                !omit_thing
                    || input.contains(t)
                    || t.predicate.as_str() != "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    || t.object
                        != oxrdf::NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#Thing")
                            .into()
            }),
        |node| labels.is_issued(node),
    ))
}
