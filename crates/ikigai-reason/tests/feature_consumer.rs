//! Compile a real host separately so workspace dev-dependencies cannot mask
//! feature mismatches between the reasoner and a sibling RDF dependency.
#[test]
#[cfg(not(target_family = "wasm"))]
fn sibling_rdf12_host() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/feature-consumer/Cargo.toml");
    let status = std::process::Command::new(env!("CARGO"))
        .arg("test")
        .arg("--manifest-path")
        .arg(manifest)
        .status()
        .expect("run the isolated RDF 1.2 host tests");
    assert!(status.success(), "the isolated RDF 1.2 host tests failed");
}
