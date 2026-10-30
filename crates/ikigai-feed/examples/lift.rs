//! Lift a feed file and print the form as Turtle: `cargo run -p ikigai-feed --example lift -- FILE [BASE]`.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a feed file");
    let bytes = std::fs::read(&path).expect("readable");
    let mut request = Request::new(Verb::Source, Iri::parse("urn:iki:feed:graph").unwrap())
        .with_arg("content", ArgRef::Inline(bytes));
    if let Some(base) = args.next() {
        request = request.with_arg("base", ArgRef::Inline(base.into_bytes()));
    }
    let kernel = Kernel::new(Arc::new(ikigai_feed::space()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(rep) => print!("{}", String::from_utf8_lossy(&rep.bytes)),
        Err(e) => eprintln!("error: {e}"),
    }
}
