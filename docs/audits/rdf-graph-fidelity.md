# RDF graph fidelity audit

Synthetic graphs only, audited on 2026-11-11. The tests exercise the public kernel
endpoints, then parse the resulting bytes and compare datasets after blank-node
canonicalization. This is a bounded correctness audit, not a format conformance claim.
The parsers and serializers share the Oxigraph stack; this is not an independent
implementation comparison.

## Reproduced and fixed

With the initial reproduction tests and the original `transrept.rs` from
`631e70ca9abec95734ca75965edcbfb1a04fa7e6`, this command produced eight failures in
`graph_fidelity` and three in `streamed`:

```shell
cargo test -p ikigai-rdf --features rdf-12 \
  --test graph_fidelity --test streamed --no-fail-fast
```

| Defect | Reproduction in `crates/ikigai-rdf/tests` | Original observation |
| --- | --- | --- |
| Named-graph output could not be read back | `graph_fidelity::named_graphs_round_trip_between_dataset_formats`; `streamed::dataset_syntax_survives_every_chunk_boundary_and_late_disambiguation` | N-Quads was sent to the Turtle parser, which expected a dot instead of the graph name; TriG was also sent to Turtle |
| RDF bracket openings were classified as JSON | `graph_fidelity::turtle_and_trig_openings_are_not_mistaken_for_jsonld`; `empty_initial_trig_graphs_are_accepted` | `[] <urn:test:p> ...` and an initial empty TriG graph failed in the JSON parser |
| Escaped or long subject IRIs were classified as XML | `graph_fidelity::escaped_iri_subject_is_not_mistaken_for_xml`; `long_subject_iris_are_accepted`; `unicode_escapes_in_iri_schemes_and_separators_are_recognized`; streamed long-IRI test | `Unknown prefix http:`; the sniff rejected Unicode escapes and stopped scanning at 2,048 bytes |
| Alias responses used aliases as media types | `graph_fidelity::aliases_answer_canonical_media_types` | `as=ttl` answered media type `ttl` |
| Some inline Turtle openings were treated as references | `graph_fidelity::with_accepts_turtle_blank_node_and_sparql_prefix_openings` | `with=_:a ...` failed IRI parsing instead of being parsed as Turtle |
| Retrieval bases were absent | `graph_fidelity::src_uses_the_retrieval_iri_as_the_document_base`; streamed source-base test | Relative terms in a synthetic HTTP resource failed without a parser base |

Turtle-family input now runs through TriG and N-Quads parsers until syntax separates
them. While both remain possible, only matching quads are emitted: TriG may emit a
triple before checking the token that N-Quads recognizes as its graph name. This works
across chunk boundaries and after an arbitrary common N-Triples prefix. Once a parser
fails it is retired; incompatible mixed syntaxes are refused. Buffered parsing feeds
bounded chunks rather than retaining a second full graph.

Sniffing distinguishes RDF bracket openings from JSON and recognizes Unicode escapes
and long IRIs. A stream whose opening remains undecided beyond the sniff hold falls back
to collection under the existing byte budget. Resolved `src=` inputs use their declared
RDF media type when available and their retrieval IRI as the parser base. Inline content
continues to require an in-document base for relative terms.

## Coverage and limits

- All 36 pairs of the six RDF formats preserve a cyclic blank-node graph and literal
  terms: string escapes, Unicode/non-BMP characters, an 80,000-byte literal, language
  tags, custom datatypes, integer/decimal/boolean/double lexical forms, invalid integer
  lexical forms, date/time lexical forms, and XML literals. Each pair is repeated on a
  fresh kernel to check identical bytes. Blank nodes remain blank nodes; no skolemization
  is introduced by transreption.
- Dataset tests compare all nine pairs of TriG, N-Quads and JSON-LD, including default,
  IRI-named and blank-node-named graphs. A blank graph name also appears as a term.
  Each triple-only RDF target refuses named graphs. The HTML face remains the documented
  three-column view and omits graph names; it is not a round-trip RDF format.
- With `rdf-12`, 25 format pairs preserve nested triple terms, reifiers, annotations and
  blank nodes shared inside/outside terms. JSON-LD refuses triple terms. Direction-tagged
  literals round-trip across all 36 pairs, including JSON-LD. Builds where another crate
  unifies RDF 1.2 on without this crate's `rdf-12` feature retain the previously documented
  limitation; this audit does not remove it.
- Prefix IRIs and relative terms resolve against in-document bases. Prefix declarations
  are expanded rather than preserved. Source-base tests use synthetic HTTP endpoints;
  no network is contacted. Streamed source tests also check in-document base overrides.
  Redirect-derived final retrieval bases are not covered: the source API used here does
  not supply a final URL.
- Union and both diff modes implement triple-set operations. Diff followed by removal
  of the removed delta and union of the added delta reconstructs a skolemized graph.
  Explicit labels shared across documents are shared identity; different labels and
  independently anonymous nodes are distinct even for isomorphic structures. This is
  not an isomorphism-based diff. Persistent sync identity requires skolemized IRIs.
- Every RDF face in the six-format matrix reports its media type. Aliases report the
  canonical media type; unknown output types are refused. Existing HTML and conformance
  tests cover the declared human-readable face.
- Streamed tests use 1-, 2-, 3-, 7-, 64- and 4,096-byte chunks, a late named graph after
  100 default-graph triples, shared blank-node graph names, bracket/escape openings,
  an IRI beyond the 64 KiB sniff hold, and one-byte source-base input. Existing streaming
  tests still check incremental consumption, byte budgets and nesting refusals.

The tests do not enumerate every datatype IRI, every graph topology or every lexical
form. They do not prove full RDF canonicalization, cross-implementation interoperability,
HTML accessibility conformance, browser runtime behavior or assistive-technology behavior.
The HTML markup is unchanged. New parse/base refusals are plain text and identify the
input and an action to correct it; no automated accessibility checker was run.
