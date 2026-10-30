use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Exact, Fallback, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Result, Verb,
};
use oxrdf::Triple;
use std::{collections::HashSet, sync::Arc};

pub const PREFIXES: &str = "@prefix ex: <http://example.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix owl: <http://www.w3.org/2002/07/owl#> . @prefix skos: <http://www.w3.org/2004/02/skos/core#> . ";
pub struct Graph(pub String, pub &'static str);
#[async_trait]
impl Endpoint for Graph {
    async fn invoke(&self, _: &Invocation<'_>) -> Result<Representation> {
        Ok(Representation::new(ReprType::new(self.1), self.0.as_bytes().to_vec()).cacheable())
    }
    fn name(&self) -> &str {
        "fixture"
    }
    fn describe(&self) -> Description {
        Description::new("fixture").verb(Verb::Source)
    }
}
pub fn fixture(ttl: &str) -> Graph {
    Graph(format!("{PREFIXES}{ttl}"), "text/turtle")
}
pub fn kernel(space: EndpointSpace) -> Kernel {
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space),
        Arc::new(ikigai_rdf::space()),
    ])))
}
pub fn with_graph(ttl: &str) -> Kernel {
    kernel(ikigai_reason::space().bind(Exact::new("urn:test:graph"), fixture(ttl)))
}
pub fn request(form: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:reason:{form}")).unwrap(),
        ),
        |req, (key, value)| req.with_arg(*key, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}
pub fn issue(kernel: &Kernel, form: &str, args: &[(&str, &str)]) -> Result<Representation> {
    block_on(kernel.issue(request(form, args), &Capability::root()))
}
pub fn parse(bytes: &[u8]) -> HashSet<Triple> {
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(bytes)
        .map(|q| {
            let q = q.unwrap();
            Triple::new(q.subject, q.predicate, q.object)
        })
        .collect()
}
pub fn triples(ttl: &str) -> HashSet<Triple> {
    parse(format!("{PREFIXES}{ttl}").as_bytes())
}
pub fn closure(kernel: &Kernel) -> HashSet<Triple> {
    parse(
        &issue(kernel, "closure", &[("graph", "urn:test:graph")])
            .unwrap()
            .bytes,
    )
}
