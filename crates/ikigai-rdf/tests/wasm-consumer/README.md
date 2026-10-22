# Minimal wasm consumer

This separate Cargo workspace depends on ikigai-rdf with default features disabled.
It exports a Turtle nesting check and a Turtle subclass parser so LTO retains both.
The parent workspace's dev-dependencies and SPARQL cannot unify formats back on.

From the repository root:

```sh
bash crates/ikigai-rdf/tests/wasm-consumer/check-graph.sh
cargo test --manifest-path crates/ikigai-rdf/tests/wasm-consumer/Cargo.toml
cargo build --manifest-path crates/ikigai-rdf/tests/wasm-consumer/Cargo.toml --target wasm32-unknown-unknown --release
wc -c crates/ikigai-rdf/tests/wasm-consumer/target/wasm32-unknown-unknown/release/rdf_depth_consumer.wasm
```

The parent `tests/consumer.rs` runs this consumer's native tests in CI with no
features, `rdf-12`, `rdfxml`, and `full`. Release wasm builds were measured locally;
the existing workspace CI also lints the library for wasm.
The graph check asserts that neither the base nor RDF 1.2 configuration contains
oxrdfio, oxjsonld, oxrdfxml or quick-xml; XML scanning adds only quick-xml.
This checks normal target dependencies, not test dependencies.

## Measurement, 2026-10-22

Baseline: `1529d7a9ed1068e3af67d211e8c9205dfc474351` (main before this change).
Identical consumer source and release profile (opt-level=s, LTO, one codegen unit,
strip=true), rustc 1.99.0 (b940084d7), x86_64 Linux host, wasm32-unknown-unknown.
Both builds resolved oxrdf 0.3.5/oxttl 0.2.5; baseline also used oxrdfio 0.2.7,
oxjsonld 0.2.7, oxrdfxml 0.2.5 and quick-xml 0.37.5.

| Measurement | Before | After |
| --- | ---: | ---: |
| Release `.wasm` bytes | 562,538 | 187,032 |
| oxrdfio in normal target graph | yes | no |
| oxjsonld in normal target graph | yes | no |
| oxrdfxml in normal target graph | yes | no |
| quick-xml in normal target graph | yes | no |

The graph assertion failed on the baseline, printing all four unwanted crates,
then passed after this change. Artifact size fell 375,506 bytes (66.8%) for this
consumer; sizes depend on toolchain and what the host retains. The graph removal
is the regression contract, not an exact byte threshold. In particular, the
subclass parser now uses oxttl directly instead of oxrdfio's format dispatcher.

To repeat the baseline measurement without changing the source, use this fixture
against the baseline library with the `[features]` table omitted (the baseline did
not declare `transrept` or `rdfxml`).
