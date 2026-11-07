pub use ikigai_reason::space;

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use ikigai_core::{
        ArgRef, Capability, Error, Exact, FnEndpoint, Iri, Kernel, ReprType, Representation,
        Request, Verb,
    };
    use std::sync::Arc;

    #[test]
    fn sibling_rdf12_triple_terms_are_refused_without_reasoner_features() {
        let kernel = Kernel::new(Arc::new(super::space().bind(
            Exact::new("urn:test:graph"),
            FnEndpoint::new("graph", |_| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    b"<urn:s> <urn:p> <<(<urn:a> <urn:q> <urn:b>)>> .".to_vec(),
                ))
            }),
        )));
        for form in ["closure", "inferred"] {
            let request = Request::new(
                Verb::Source,
                Iri::parse(format!("urn:reason:{form}")).unwrap(),
            )
            .with_arg("graph", ArgRef::Inline(b"urn:test:graph".to_vec()));
            let result = block_on(kernel.issue(request, &Capability::root()));
            assert!(matches!(result,
                Err(Error::InvalidArgument { name, detail })
                if name == "graph" && detail.contains("RDF 1.2 triple terms")));
        }
    }
}
