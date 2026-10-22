//! Exercise a real consumer in its own workspace, outside dev-dependency feature unification.
use std::{path::Path, process::Command};

fn run(command: &mut Command) {
    let output = command.output().expect("run consumer verification");
    assert!(
        output.status.success(),
        "{command:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn standalone_consumer_keeps_formats_optional() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wasm-consumer");
    run(Command::new("bash").arg(fixture.join("check-graph.sh")));
    for features in ["", "rdf-12", "rdfxml", "full"] {
        // Separate target directory avoids waiting for the parent cargo test's build lock.
        run(Command::new(env!("CARGO"))
            .arg("test")
            .arg("--manifest-path")
            .arg(fixture.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(fixture.join("target"))
            .arg("--features")
            .arg(features));
    }
}
