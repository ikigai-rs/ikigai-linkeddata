# ikigai-reason

OWL 2 RL closure as two stateless ikigai resources, using
[`reasonable` 0.4.4](https://github.com/gtfierro/reasonable) (BSD-3-Clause)
through its in-memory API. No files, network clients, stores, or inference rules
are implemented here. Sources are read through the kernel under the caller's
capability; HTTP(S) graph IRIs are fetched through `urn:http:get`.

| Resource | Answer |
| --- | --- |
| `urn:reason:closure` | Input RDF merge plus every triple materialized by reasonable |
| `urn:reason:inferred` | The same closure minus the input triples |

Both take these ArgSpecs:

| Argument | Meaning |
| --- | --- |
| `graph` (required) | One or more graph IRIs, separated by commas or whitespace |
| `profile` | `owl-rl` (default and currently the only choice) |
| `as` | `text/turtle` (default), `application/n-triples`, `application/n-quads`, `application/trig`, `application/rdf+xml`, `application/ld+json`, or `text/html` |

Mount `ikigai_reason::space()` and `ikigai_rdf::space()` together; the latter
provides the non-Turtle faces through `urn:rdf:transrept`. Turtle output uses
Oxigraph's serializer. Input media types must name an RDF syntax supported by
`oxrdfio`. Relative IRIs resolve against the source IRI. Dataset inputs are
flattened into one graph. Blank nodes are scoped to their source document;
repeated source IRIs are read once. Output order and blank labels are deterministic
for the same inputs. The profile delegates to reasonable's OWL 2 RL rules; this
module does not provide a consistency verdict or an RDFS-only mode.

```rust
use ikigai_core::{Fallback, Kernel};
use std::sync::Arc;

let kernel = Kernel::new(Arc::new(Fallback::new(vec![
    Arc::new(ikigai_reason::space()),
    Arc::new(ikigai_rdf::space()),
    // Add the host's graph source spaces here.
])));
```

For example, a kernel request can name
`urn:reason:closure graph=urn:my:data,urn:my:vocabulary`. A SKOS vocabulary
containing `skos:broader rdfs:subPropertyOf skos:broaderTransitive` and
`skos:broaderTransitive a owl:TransitiveProperty` makes broader chains available
in the closure without changing the input graph.

Both resources are `.cacheable()`. The kernel folds every source's golden
threads and expiry into the answer. Cutting an input's thread recomputes its
closure and downstream readers; closures over untouched inputs stay cached.
Uncacheable source graphs keep their derived answers uncacheable.

## Bounds and errors

`space()` defaults to **1,000 input triples** and **100,000 closure triples**.
`space_with_limits(Limits { max_input_triples, max_output_triples })` lets the
host configure both; this parameterized space stays anonymous for the host to
name. Bounds refuse with `OverBudget`, never truncate. Parsed input occurrences
count toward the input bound, including duplicate statements, across all sources.
The output bound counts distinct closure triples, including the input and
reasonable's axiomatic/reflexive conclusions, even on `urn:reason:inferred`.

The input bound is checked before calling `reason()`: reasonable cannot be
interrupted, and wasm has no thread on which to time it out. This limits the
work admitted to reasoning, **not a wall-clock deadline**. The output bound is
checked after materialization, so it does not limit peak memory inside the
engine. Even a small schema can produce a quadratic closure. Graph resolution
and representation bytes remain subject to the host kernel's own budgets.
Parser depth scans reuse ikigai-rdf's bounds; native parsing uses a sized stack.

A missing graph is `NotFound`; a non-RDF or malformed RDF input is
`InvalidArgument` naming `graph`. Missing `graph` is `MissingArgument`.
Unsupported profiles and output faces are `InvalidArgument` on their own
argument. Source authorization and transport errors propagate unchanged.

## Features and verification

The crate compiles for `wasm32-unknown-unknown` with the workspace's
`getrandom_backend="wasm_js"` configuration and `getrandom/wasm_js` feature.
CI checks wasm library Clippy; this is a compile check, not a browser runtime test.

Hosts enabling RDF 1.2 elsewhere in their dependency graph must also enable
`ikigai-reason/rdf-12` (and `ikigai-sparql/rdf-12` when using SPARQL), so all
crates agree on oxrdf's term variants. Reasoning is RDF 1.1: triple terms are
refused with `InvalidArgument`, rather than passed into the engine's RDF-star
unsupported paths.

Small fixtures cover subclass chains, subproperties, domain/range, inverse,
equivalent classes/sameAs, and SKOS transitivity. Other tests cover exact inferred
set subtraction, RDF merging, selective thread invalidation, deterministic
recomputation, both bounds, and typed failures. `ikigai-conformance` checks both
resources and every RDF face without waivers. `largest_fixture_measurement`
prints closure size and elapsed time for a generated 200-triple class hierarchy.
