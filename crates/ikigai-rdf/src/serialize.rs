use ikigai_core::{Error, Result};
use oxrdf::{GraphName, Triple};
use oxrdfio::{RdfFormat, RdfSerializer};
use std::collections::BTreeMap;

/// Serialize triples in caller-provided order with namespace hints from parsed RDF.
///
/// Prefixes are emitted in name order. Oxigraph handles abbreviation and escaping;
/// hints never change RDF terms. Call [`crate::canonical_triples`] first when the
/// source is an unordered set. An empty graph still emits its prefix declarations.
pub fn serialize_turtle(
    triples: impl IntoIterator<Item = Triple>,
    prefixes: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle);
    for (name, iri) in prefixes {
        serializer = serializer
            .with_prefix(name, iri)
            .map_err(|e| Error::Endpoint(format!("RDF prefix error: {e}")))?;
    }
    let mut writer = serializer.for_writer(Vec::new());
    for triple in triples {
        writer
            .serialize_quad(&triple.in_graph(GraphName::DefaultGraph))
            .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| Error::Endpoint(format!("RDF serialize error: {e}")))
}
