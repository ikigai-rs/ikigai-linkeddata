//! Turtle/TriG and N-Triples/N-Quads share prefixes of arbitrary length. Keep both
//! parsers until the syntax distinguishes them, without buffering the whole document.

use ikigai_core::{Error, Result};
use oxrdf::Quad;
use oxttl::{nquads::LowLevelNQuadsParser, trig::LowLevelTriGParser, NQuadsParser, TriGParser};
use std::collections::VecDeque;

pub(crate) struct TurtleFamilyParser {
    trig: Option<LowLevelTriGParser>,
    nquads: Option<LowLevelNQuadsParser>,
    trig_error: Option<String>,
    trig_pending: VecDeque<Quad>,
    nquads_pending: VecDeque<Quad>,
}

impl TurtleFamilyParser {
    pub(crate) fn new(base: Option<&str>) -> Result<Self> {
        let mut trig = TriGParser::new();
        if let Some(base) = base {
            trig = trig.with_base_iri(base).map_err(|e| {
                Error::Endpoint(format!(
                    "urn:rdf:transrept: src base IRI is invalid: {e}. Supply an absolute src IRI."
                ))
            })?;
        }
        Ok(Self {
            trig: Some(trig.low_level()),
            nquads: Some(NQuadsParser::new().low_level()),
            trig_error: None,
            trig_pending: VecDeque::new(),
            nquads_pending: VecDeque::new(),
        })
    }

    /// Feed bounded chunks so the buffered path, too, retains only the parser's
    /// current statement and one chunk of completed quads, not a second full graph.
    pub(crate) fn for_slice<'a>(
        input: &'a [u8],
        base: Option<&str>,
    ) -> Result<impl Iterator<Item = Result<Quad>> + 'a> {
        let mut parser = Self::new(base)?;
        let mut chunks = input.chunks(8192);
        let mut pending = VecDeque::new();
        let mut ended = false;
        Ok(std::iter::from_fn(move || loop {
            if let Some(quad) = pending.pop_front() {
                return Some(Ok(quad));
            }
            if ended {
                return None;
            }
            if let Some(chunk) = chunks.next() {
                parser.extend_from_slice(chunk);
            } else {
                parser.end();
                ended = true;
            }
            match parser.drain() {
                Ok(quads) => pending.extend(quads),
                Err(error) => {
                    ended = true;
                    return Some(Err(error));
                }
            }
        }))
    }

    pub(crate) fn extend_from_slice(&mut self, bytes: &[u8]) {
        if let Some(parser) = &mut self.trig {
            parser.extend_from_slice(bytes);
        }
        if let Some(parser) = &mut self.nquads {
            parser.extend_from_slice(bytes);
        }
    }

    pub(crate) fn end(&mut self) {
        if let Some(parser) = &mut self.trig {
            parser.end();
        }
        if let Some(parser) = &mut self.nquads {
            parser.end();
        }
    }

    pub(crate) fn drain(&mut self) -> Result<Vec<Quad>> {
        if let Some(parser) = &mut self.trig {
            while let Some(quad) = parser.parse_next() {
                match quad {
                    Ok(quad) => self.trig_pending.push_back(quad),
                    Err(error) => {
                        self.trig_error = Some(error.to_string());
                        self.trig = None;
                        self.trig_pending.clear();
                        break;
                    }
                }
            }
        }
        if let Some(parser) = &mut self.nquads {
            while let Some(quad) = parser.parse_next() {
                match quad {
                    Ok(quad) => self.nquads_pending.push_back(quad),
                    Err(_) => {
                        self.nquads = None;
                        self.nquads_pending.clear();
                        break;
                    }
                }
            }
        }
        match (self.trig.is_some(), self.nquads.is_some()) {
            (true, true) => {
                // TriG can emit a default-graph triple before checking its final
                // dot. Do not commit that triple until N-Quads agrees: the next
                // token may instead name its graph, even in a later input chunk.
                let mut quads = Vec::new();
                while let (Some(a), Some(b)) =
                    (self.trig_pending.front(), self.nquads_pending.front())
                {
                    if a != b {
                        break;
                    }
                    quads.push(self.trig_pending.pop_front().expect("front exists"));
                    self.nquads_pending.pop_front();
                }
                Ok(quads)
            }
            (true, false) => Ok(self.trig_pending.drain(..).collect()),
            (false, true) => Ok(self.nquads_pending.drain(..).collect()),
            (false, false) => Err(Error::Endpoint(format!(
                "urn:rdf:transrept: RDF parse error in input: {}. Supply a valid Turtle, TriG, N-Triples, or N-Quads document.",
                self.trig_error.as_deref().unwrap_or("invalid RDF")
            ))),
        }
    }
}
