# Reasoner correctness audit

Audited on 2026-11-11 against `reasonable` 0.4.4. The reference is
[W3C OWL 2 Profiles, section 4.3, tables 4–9](https://www.w3.org/TR/owl2-profiles/#Reasoning_in_OWL_2_RL_and_RDF_Graphs).
This is a bounded synthetic audit, not an OWL conformance claim. Each rule graph
has at most a dozen input triples. The equality availability reproduction and its
pending decision are outside this audit and were not run.

## Reproduce

The tests live in `crates/ikigai-reason/tests/owl_rl_audit.rs`. Rule names replace
hyphens with underscores. Each entailment probe first calls the engine directly,
compares hand-written required and forbidden conclusions, then checks that our
endpoint preserves the engine output plus the input. This attributes rule defects
to the library without implementing replacement inference rules in this crate.
Named list cells avoid differences due only to blank-node labels.

```shell
# Passing rule probes and wrapper regressions:
cargo test -p ikigai-reason --test owl_rl_audit

# Known library defects: deliberately FAILS, currently 23 failed tests.
# This only runs the tiny correctness fixtures, not closure_expansion.
cargo test -p ikigai-reason --test owl_rl_audit -- --ignored

# Individual unsound inference reproduction:
cargo test -p ikigai-reason --test owl_rl_audit prp_ifp -- --ignored --exact
```

Before the wrapper changes, running the initial 48 probes against repository base
`5eea456a948f867e0dc6a553e17d4a1417c31b42` gave **26 passed, 22 failed**:
20 entailment probes, the missing-library-diagnostics probe, and the wrapper's
silently discarded diagnostics probe failed. A separate blank-output regression
then failed with `closure text/turtle contains blank nodes` before skolemization
was added. After the fixes and additional controls, the suite has **33 passing
and 23 explicitly ignored tests**. Running the ignored tests gives **0 passed,
23 failed**; they retain normative assertions rather than asserting buggy output
as correct. No library defect below has been patched here or filed separately.

## Rule results

“Pass” means the stated fixture's required conclusions appear and its negative
controls do not. It does not establish the rule for every graph or prove that the
entire closure contains no other unsound triples. Additional exact predicate and
class projections check all data edges/memberships for symmetry, transitivity,
inverse, subproperty, has-value, domain and range. The full endpoint answer is
also compared exactly to the library output union input on passing rule probes.

| Rule | Result in reasonable 0.4.4 | Observation |
| --- | --- | --- |
| eq-ref | Fail | Plain `ex:x ex:p ex:y` does not produce any of the three required reflexive equalities |
| eq-sym | Pass | Reverse equality; unrelated node stays unrelated |
| eq-trans | Pass | Three-node chain closes; disconnected node stays unrelated |
| eq-rep-s | Pass | Subject substitution without reversing the property edge |
| eq-rep-p | Pass | Predicate substitution without reversing the edge |
| eq-rep-o | Pass | Object substitution without reversing the edge |
| prp-dom | Pass | Only the matching edge's subject receives the domain type |
| prp-rng | Pass (entailment) | Only the matching edge's object receives the range type; see false diagnostic below |
| prp-symp | Pass | Reverse edge for the symmetric predicate only |
| prp-trp | Pass | Forward reachability without symmetry or crossing another predicate |
| prp-inv1 | Pass | Forward use of inverse axiom |
| prp-inv2 | Pass | Reverse use of inverse axiom |
| prp-spo1 | Pass | Superproperty edge; no reverse subproperty inference |
| prp-fp | Pass | Same subject's objects equated; different subject's object stays separate |
| prp-ifp | **Fail, unsound** | Subjects with different objects are equated; details below |
| cax-sco | Pass | Subclass instances inherit the superclass; no converse |
| cax-eqc1 | Pass | First class to second |
| cax-eqc2 | Pass | Second class to first |
| cls-int1 | Pass | Both memberships required; one member alone does not suffice |
| cls-int2 | Pass | Intersection membership yields both member classes |
| cls-uni | Pass | Either member gives union membership; union does not give members |
| cls-svf1 | Pass | Matching property and typed filler required |
| cls-avf | Pass | Only edges from restriction instances type their objects |
| cls-hv1 | Pass | Restriction membership yields the fixed property value |
| cls-hv2 | Pass | Matching fixed property value yields restriction membership |
| scm-cls | Fail | All four expected schema conclusions for an explicit class are missing |
| scm-sco | Pass | Subclass transitive closure |
| scm-eqc1 | Pass | Equivalent class gives both subclass directions |
| scm-eqc2 | Fail | Mutual subclass does not yield equivalentClass |
| scm-op | Fail | Object property lacks reflexive subPropertyOf/equivalentProperty |
| scm-dp | Fail | Datatype property lacks reflexive subPropertyOf/equivalentProperty |
| scm-spo | Fail | Subproperty chain does not yield its transitive schema edge |
| scm-eqp1 | Fail | equivalentProperty does not yield the two schema subproperty edges |
| scm-eqp2 | Fail | Mutual subproperty does not yield equivalentProperty |
| scm-dom1 | Fail | Domain does not propagate up the class hierarchy |
| scm-dom2 | Fail | Domain does not propagate down the property hierarchy |
| scm-rng1 | Fail | Range does not propagate up the class hierarchy |
| scm-rng2 | Fail | Range does not propagate down the property hierarchy |
| scm-hv | Fail | hasValue restrictions lack the entailed subclass edge |
| scm-svf1 | Fail | Filler subclass does not relate someValuesFrom restrictions |
| scm-svf2 | Fail | Subproperty does not relate someValuesFrom restrictions |
| scm-avf1 | Fail | Filler subclass does not relate allValuesFrom restrictions |
| scm-avf2 | Fail | Subproperty does not yield the contravariant restriction subclass |
| scm-int | Fail | Intersection does not yield its member subclass edges |
| scm-uni | Fail | Union does not yield its member superclass edges |

### Unsound inverse-functional property inference

The `prp_ifp` fixture includes:

```turtle
ex:p a owl:InverseFunctionalProperty .
ex:x ex:p ex:z .
ex:y ex:p ex:z .
ex:w ex:p ex:v .
```

`ex:x owl:sameAs ex:y` is required. `ex:x owl:sameAs ex:w` is **not**
entailed: the objects differ, and nothing equates them. The engine nevertheless
emits the latter equality. In the dependency's `prp-ifp` join, `match y1 { y2 => … }`
binds a new variable rather than comparing the two object IDs. The failing test
checks the observable inference directly; this audit does not patch the dependency.

### Inconsistency and diagnostics

| Rule concluding false | Engine diagnostic | Endpoint before / after |
| --- | --- | --- |
| cax-dw | Present | Successful graph / typed refusal |
| prp-irp | Present | Successful graph / typed refusal |
| prp-asyp | Present | Successful graph / typed refusal |
| cls-nothing2 | Present | Successful graph / typed refusal |
| eq-diff1 | **Missing** | Successful graph / still successful graph |
| cls-com | **Missing** | Successful graph / still successful graph |
| prp-pdw | Present | Successful graph / typed refusal |

`engine_detects_all_requested_inconsistencies` fails with the missing rule IDs
`eq-diff1` and `cls-com`. The separately ignored
`missing_engine_diagnostics_cannot_provide_a_consistency_verdict` also fails for
both endpoints, which successfully answer these inconsistent graphs. Their fixtures explicitly assert both sameAs and
differentFrom, or both complementary class memberships, respectively. No
closed-world assumptions are needed. These remain library defects, and a
successful endpoint answer must not be read as a consistency verdict.

The old README explicitly disclaimed a consistency verdict; it did not promise
these refusals. The observed loss of available diagnostics is now addressed at
the wrapper boundary. The wrapper now refuses engine-reported OWL errors with `InvalidArgument` on
`graph`, listing sorted, distinct rule IDs. Tests cover both resources, all seven
output faces, derived contradictions across two source graphs, and `omit-thing`.
Controls with different individuals, properties or values are accepted.

`ordinary_object_range_has_no_datatype_clash` separately reproduces a false
`rdfs-datatype-range` diagnostic for the consistent graph
`ex:p rdfs:range ex:C . ex:x ex:p ex:y .`. The dependency checks whether the object
is a literal before establishing that the range is a supported datatype. The
wrapper therefore uses only OWL diagnostic codes for refusal. It does not claim
to validate datatypes, and it does not repair the library's datatype checks.

## Wrapper contract and fixes

- **Blank nodes: reproduced and fixed.** Parser-order relabeling previously kept
  blank nodes in answers. Nodes are now named before reasoning, using SHA-256 of
  length-delimited source IRI, declared media type without parameters, and source
  bytes, followed by a document-local deterministic label. Both resources use the
  same skolemized input set. Lists/restrictions still work. Tests check all six
  RDF faces, independent source scopes for identical bytes, repeated-source
  deduplication, anonymous nodes, eight fresh-kernel recomputations, changed
  document isolation, and exact inferred set subtraction. Names are not stable
  identity across edits or changes of syntax. This deliberately changes the old
  documented blank-node output behavior to meet the blank-node-free contract.
- **Formats:** existing `reason::rdf_inputs_are_parsed_by_media_type_and_faces_transrept_without_loss`
  checks all 36 pairs of six RDF formats, including Unicode/escaped literals.
  Existing tests check relative bases, namespace retention, source comments and
  conflicting prefixes. HTML remains the shared three-column table and is not
  an RDF round-trip format. Markup was not changed.
- **Determinism:** named inputs and anonymous/list inputs produce byte-identical
  Turtle over eight fresh kernels. Existing cut/recompute tests still pass.
  This is an identical-input promise, not RDF canonicalization across reordered
  or isomorphic documents.
- **Input and axioms:** the closure preserves the skolemized input, and inferred
  output is exact set subtraction. The engine's unconditional `owl:Thing` and
  `owl:Nothing` class assertions are included. A plain property use does not
  introduce `ex:p rdf:type rdf:Property`; that RDF/RDFS axiom is not promised by
  this wrapper, and section 4.3 makes inclusion of RDF/RDFS axioms optional.
  Missing OWL schema/equality conclusions are listed separately above.
- **Bounds:** parsed occurrences, including duplicate statements, count toward
  the input bound before reasoning. Distinct full closure triples count toward
  the output bound before inferred subtraction or `omit-thing`. Tests cover
  one input triple with a literal longer than the numeric limit, duplicate input,
  multiple sources, exactly at the output limit and one below/above it. Refusals
  are typed `OverBudget`; answers are never truncated. Skolemization does not
  change the triple count. These are answer/input bounds, not execution limits.
- **Accessibility:** the new refusal is plain text, names `graph` through its
  typed argument field, lists the violated rules and asks the caller to correct
  the inconsistent graph. Tests inspect the typed error on every face. No
  browser, accessibility checker or assistive technology was used.

## Verification limits

The rule fixtures use named resources and small RDF lists. They do not exhaust
all literals, datatype value spaces, malformed/cyclic lists, OWL property chains,
keys, cardinalities, interactions between every rule, or generalized RDF with
literal subjects. The tests and endpoint parser share Oxigraph; this is not a
cross-parser interoperability assessment. Library failures are intentionally
ignored by normal gates and must be run explicitly. Green CI proves the passing
regressions and workspace gates, not complete OWL 2 RL correctness or consistency.

The workspace gates used for this change are:

```shell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo clippy --workspace --lib --target wasm32-unknown-unknown -- -D warnings
# For each of http-client-probe and rdf-12:
cargo clippy --workspace --all-targets --features FEATURE -- -D warnings
cargo test --workspace --features FEATURE
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --features FEATURE
```

The pull request's CI runs these gates. The wasm gate is a compile/lint check,
not a browser runtime test. No package version was changed or published.
