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
| `omit-thing` | `false` (default); `true` omits inferred `rdf:type owl:Thing` triples, preserving explicit input typings |
| `profile` | `owl-rl` (default and currently the only choice) |
| `as` | `text/turtle` (default), `application/n-triples`, `application/n-quads`, `application/trig`, `application/rdf+xml`, `application/ld+json`, or `text/html` |

Mount `ikigai_reason::space()` and `ikigai_rdf::space()` together; the latter
provides the non-Turtle faces through `urn:rdf:transrept`. Turtle output uses
the shared `ikigai-rdf` serializer backed by Oxigraph. Input media types must name
an RDF syntax supported by `oxrdfio`. Relative IRIs resolve against the source IRI. Dataset inputs are
flattened into one graph. Blank nodes are scoped to their source document;
repeated source IRIs are read once. Output order and blank labels are deterministic
for the same inputs. Turtle retains the input namespace prefixes (resolved against
its base); when sources conflict on a prefix name, the first source wins and
other terms serialize losslessly as full IRIs. Within a document the final
binding wins. Turtle comments identify closure versus inferred output and list
each source graph once. These identify the merged answer, not per-triple
provenance: entailments may depend on several graphs. Other faces preserve the
RDF triples but do not retain those comments.

For a shorter answer in runbook pages, request
`urn:reason:inferred graph=urn:my:data,urn:my:vocabulary omit-thing=true`.
This removes only new `rdf:type owl:Thing` triples, after reasoning and budget
checks, on every output face. Full output remains the default so existing callers
still receive the documented complete closure or exact input subtraction.
The profile delegates to reasonable's OWL 2 RL rules; this
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
reasonable's axiomatic/reflexive conclusions, even on `urn:reason:inferred` or
with `omit-thing=true`.

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

## Choosing: construction, a property path, or a reasoner

A host can answer "what is above this class?" three ways:

1. **Construction**: read the `rdfs:subClassOf` pairs with `ikigai_rdf::subclass_axioms`
   and build them into the kernel with `Kernel::with_subclass_axioms`. The Selection demo
   does this. The closure is a private map that `select_action` and `urn:kernel:actions`
   consult. It covers `subClassOf` only and is fixed when the kernel is built.
2. **A SPARQL property path**: `urn:sparql:construct` or `select` with
   `rdfs:subClassOf+` (or `*`), one query for each question.
3. **A reasoner**: `urn:reason:closure` or `urn:reason:inferred`, the full OWL 2 RL
   closure.

Approaches 2 and 3 are cacheable kernel resources. An edit to their input cuts its golden
thread and recomputes only what depends on it.

### The measurements

`examples/choosing.rs` measures every approach on the same generated graphs:

```text
cargo run --release -p ikigai-reason --example choosing [-- RUNS]
```

The figures below come from a release build, taking the median of 11 runs. The machine
was an 11th Gen Intel Core i7-11800H (16 threads, 62 GiB) running Linux 7.2, with
rustc/cargo 1.99. Runs were sequential, so the kernel used one thread.

The graphs:

- **demo**: the Selection demo's one axiom, `foaf:Person ⊑ schema:Person`.
- **alignment**: a schema.org-sized tree with 800 classes, depth 6 and 20 second
  parents. A FOAF-like vocabulary of 24 classes is aligned to it by 24 `subClassOf` and
  12 `equivalentClass` axioms. A few property axioms (`subPropertyOf`, `domain`, `range`,
  `inverseOf`) and instances complete it. That makes 860 triples.
- **deep**: a chain 100 classes deep.
- **wide**: 1,000 sibling classes.
- **SKOS**: 2,000 concepts at depth 5 linked by `skos:broader`. Each concept is typed and
  in a scheme, and SKOS's `broader ⊑ broaderTransitive` and transitivity axioms are
  included. That makes 6,001 triples.

The question asked is the supers of the leaf class (the broader concepts in SKOS). The
edit adds one superclass to that leaf. The harness checks that the edit reaches every
answer and that the cut re-reads the graph exactly once.

The approaches:

- **construction**: `subclass_axioms`, a kernel build, and one `select_action`. After an
  edit the host must parse again and **build a new kernel**. That new kernel starts with
  an empty cache.
- **hybrid**: `urn:reason:closure`, with its `subClassOf` pairs fed to the same kernel
  build. In SKOS, construction and hybrid read `broader` and `broaderTransitive` pairs as
  though they were `subClassOf`.
- **sparql closure**: `CONSTRUCT` of `path+` over the whole graph.
- **sparql question**: `SELECT` of `<leaf> path* ?s`.
- **reason closure** and **reason inferred**: the two resources this crate provides.

The reasoner used `Limits { max_input_triples: 1_000, max_output_triples: 100_000 }`, the
defaults, for demo and deep. Alignment and wide used `max_input_triples: 2_000`, and SKOS
used `8_000`. Wide has exactly 1,000 triples, so the default admits it and refuses it
after the edit. SKOS needs 6,001 triples, so the default refuses it.

**Cold** is the first answer on a new kernel, including any build, load or closure:

| approach | demo | alignment | deep | wide | SKOS |
| --- | ---: | ---: | ---: | ---: | ---: |
| construction | **33 µs** | **1.66 ms** | **0.80 ms** | **1.51 ms** | **7.90 ms** |
| hybrid | 0.28 ms | 10.84 ms | 22.76 ms | 5.69 ms | 50.52 ms |
| sparql closure | 2.47 ms | 13.83 ms | 12.97 ms | 5.57 ms | 31.24 ms |
| sparql question | 2.41 ms | 4.51 ms | 2.78 ms | 4.31 ms | 15.51 ms |
| reason closure | 0.23 ms | 6.98 ms | 10.38 ms | 3.85 ms | 32.78 ms |
| reason inferred | 0.10 ms | 6.32 ms | 10.61 ms | 3.50 ms | 27.15 ms |

**After an edit**: for the kernel resources this is a cut plus the next answer. For
construction and hybrid it is a rebuild:

| approach | demo | alignment | deep | wide | SKOS |
| --- | ---: | ---: | ---: | ---: | ---: |
| construction (rebuild) | **36 µs** | **1.64 ms** | **0.78 ms** | **1.48 ms** | **7.78 ms** |
| hybrid (cut + rebuild) | 0.28 ms | 10.93 ms | 22.80 ms | 5.68 ms | 53.52 ms |
| sparql closure | 2.43 ms | 20.00 ms | 14.28 ms | 5.68 ms | 33.27 ms |
| sparql question | 2.41 ms | 4.05 ms | 2.71 ms | 4.26 ms | 16.91 ms |
| reason closure | 0.15 ms | 6.96 ms | 10.37 ms | 3.80 ms | 32.84 ms |
| reason inferred | 0.17 ms | 6.78 ms | 10.50 ms | 3.50 ms | 26.83 ms |

**Warm** is the cost of one call, measured over 1,000 calls. For construction and hybrid
it is a whole `select_action`. On a kernel with no closure, that call alone costs
**27.8 µs**, so the map lookup is at most 15 µs of each figure. For the resources it is
the kernel's cached answer:

| approach | demo | alignment | deep | wide | SKOS |
| --- | ---: | ---: | ---: | ---: | ---: |
| construction (`select_action`) | 27.3 µs | 33.4 µs | 42.4 µs | 32.1 µs | 32.3 µs |
| hybrid (`select_action`) | 32.3 µs | 34.5 µs | 40.3 µs | 32.1 µs | 32.6 µs |
| sparql closure (cache) | 1.4 µs | 6.8 µs | 6.5 µs | 5.0 µs | 18.5 µs |
| sparql question (cache) | 1.1 µs | 1.1 µs | 1.2 µs | 1.1 µs | 1.6 µs |
| reason closure (cache) | 1.2 µs | 8.0 µs | 6.9 µs | 6.1 µs | 63.5 µs |
| reason inferred (cache) | 1.3 µs | 7.1 µs | 6.4 µs | 3.6 µs | 24.5 µs |
| a *different* SPARQL question | 2.43 ms | 4.03 ms | 2.69 ms | 4.23 ms | 11.85 ms |

The cache keys on the request, so each distinct question loads the graph again. The last
row shows that cost.

**Answer size**:

| | demo | alignment | deep | wide | SKOS |
| --- | ---: | ---: | ---: | ---: | ---: |
| input triples | 1 | 860 | 100 | 1,000 | 6,001 |
| construction map pairs, incl. `C ⊑ C` | 3 | 4,191 | 5,151 | 2,001 | 11,050 |
| hybrid map pairs | 3 | 4,292 | 5,151 | 2,001 | 11,050 |
| sparql closure triples (`path+`) | 1 | 3,379 | 5,050 | 1,000 | 9,050 |
| reason closure triples | 6 | 4,370 | 5,154 | 2,004 | 17,057 |
| reason inferred triples | 5 | 3,510 | 5,054 | 1,004 | 11,056 |

The construction map is private to the kernel. The harness counts the same depth-first
closure that the kernel builds.

**Coverage** comes from probing each answer for one entailment of each kind. A dash
means the scenario has no such axiom.

| entailment | construction | hybrid | sparql `subClassOf+` | sparql `(subClassOf\|equivalentClass\|^equivalentClass)+` | reason |
| --- | :---: | :---: | :---: | :---: | :---: |
| subClassOf\* (all scenarios) | yes | yes | yes | yes | yes |
| equivalentClass (alignment) | no | yes | no | yes | yes |
| subPropertyOf (alignment) | no | no | no | no | yes |
| domain/range typing (alignment) | no | no | no | no | yes |
| inverseOf (alignment) | no | no | no | no | yes |
| broaderTransitive (SKOS) | yes, if `broader` is fed as `subClassOf` | yes | yes (`skos:broader+`) | – | yes |

**What OWL RL adds that nobody asked for.** These are counts in `urn:reason:inferred`.
reasonable 0.4.4 emits **no reflexive `owl:sameAs`** in any scenario. It does emit the
following:

| unrequested triples | demo | alignment | deep | wide | SKOS |
| --- | ---: | ---: | ---: | ---: | ---: |
| `x a owl:Thing`, for every class and individual (also `owl:Nothing a owl:Thing`) | 2 | 832 | 101 | 1,001 | 2,003 |
| `owl:Thing a owl:Class`, `owl:Nothing a owl:Class` | 2 | 2 | 2 | 2 | 2 |
| reflexive: `owl:Thing a owl:Thing`, and `C ⊑ C` for classes in `equivalentClass` | 1 | 25 | 1 | 1 | 1 |
| reflexive `owl:sameAs` | 0 | 0 | 0 | 0 | 0 |
| the entailments asked for, and their consequences | 0 | 2,651 | 4,950 | 0 | 9,050 |

In wide, every inferred triple is unrequested: the input is already its own
`subClassOf` closure, and the 1,004 inferred triples are noise.

### Where Brian's expectation holds and where it does not

The expectation was that reasoning wins.

**On time, it does not win against construction.** A full kernel rebuild after an edit is
2.6 to 13 times faster than the reasoner in every scenario: 1.64 ms against 6.96 ms for the
alignment, 0.78 ms against 10.37 ms for the deep chain, and 7.78 ms against 32.84 ms for
SKOS.

**On time, it usually wins against a SPARQL path closure.** The reasoner is faster on
demo, alignment, deep and wide, and about even on SKOS (32.8 ms against 31.2 ms). A single
SPARQL question is cheaper than one closure. Every distinct question reloads the graph,
though, so the closure costs less from about the third distinct question on SKOS
(11.9 ms each against 32.8 ms once).

**It wins outright on coverage.** It is the only approach here that produces
`subPropertyOf`, `domain`/`range` and `inverseOf` entailments. It is also the only one
whose edit path needs no host code: a cut recomputes the closure and its dependents, and
leaves the rest of the kernel's cache alone. Construction needs a parse, a new kernel, and
a swap. Any answer the old kernel had cached is lost, and nothing does this automatically.

### The strategy, by scenario

- **A static, small hierarchy on a hot path**, like the Selection demo: use
  **construction**. Its whole cold cost (33 µs) is less than one cold reasoner answer
  (0.1–0.23 ms) or SPARQL answer (2.4 ms). Selection consults only the map, and the
  lookup is lost in `select_action`'s own 28 µs.
- **A hierarchy that changes while the host runs**: use **the reasoner** as the source of
  truth. A cut costs 0.15 ms to 33 ms, depending on the graph, and needs no rebuild. If
  selection must follow the edit, today that still means a rebuild, the hybrid row. The
  hybrid edit costs 11 ms for the alignment and 54 ms for SKOS. Construction rebuilds
  faster, but only covers `subClassOf`. Prefer construction's rebuild while the
  hierarchy is `subClassOf` only and edits are rare and initiated by the host.
- **Alignments that need more than `subClassOf`** (`equivalentClass`, property
  alignment, typing): use **the reasoner**. Construction silently misses
  `equivalentClass`. A widened SPARQL path recovers equivalence, at 24 ms against the
  reasoner's 7 ms, but nothing else. For selection over an alignment with
  `equivalentClass`, use the hybrid: reason, then build the map. It costs 10.8 ms cold,
  and warm selection is the same as construction's.
- **A large vocabulary** (thousands of terms): construction stays the fastest to build,
  at 7.9 ms for 2,000 concepts. The reasoner is still interactive at 33 ms, but raise
  `max_input_triples` above the 1,000 default, to 8,000 here. The output budget had room
  to spare: 17,057 of 100,000. A deep hierarchy costs the reasoner much more than a wide
  one of the same size: 10.4 ms for 100 triples deep, against 3.9 ms for 1,000 wide. Many
  distinct questions favor one cached closure over a SPARQL query for each question.
- **Browser and wasm**: these numbers are native. No wasm runtime was available to
  measure, and CI only compile-checks wasm. One fact does carry over: `reason()` cannot
  be interrupted, and wasm has no thread to run it on, so a closure blocks the page for
  its full duration. Prefer construction, or a small SPARQL question, on the page. Run
  large closures off the page, or once, and serve them cached.
- **An answer the caller reads as a graph, against one that only selection consults**: if
  anyone reads, renders, queries or exports the entailments, use **`urn:reason:closure`
  or `urn:reason:inferred`**. They are cacheable and invalidated by the golden thread,
  and the cached answer costs 1–64 µs. Construction yields no graph at all. If only
  selection consults it, use **construction**, or the hybrid when the alignment needs
  more than `subClassOf`.

### Reading the closure from a resource: a core design question

For `urn:kernel:actions` to re-select after an edit without a rebuild, the kernel would
have to hold an IRI where it now holds a map. That would be something like
`with_subclass_source("urn:reason:closure graph=...")`. Selection would resolve that IRI
through the kernel itself, so the closure's golden thread becomes a dependency of every
`urn:kernel:actions` answer. The kernel would memoize the derived map for each version of
that thread, so warm selection stays a map lookup and only a cut pays the 7–33 ms measured
above.

ikigai-core has no RDF parser. The resource would need to answer in a form core can read,
such as a plain pair list, or the host would supply the parser. `select_action` is
synchronous while resolution is async, so either selection becomes async or the kernel
refreshes a snapshot from its cut listeners. Three more questions follow. Under whose
capability is the closure resolved? What does selection do when the closure fails: keep
the last good map, or fall back to identity? How is a cycle refused when the closure
resource is itself selected? Those belong to the hub, not to this crate.
