# ikigai-sniff

Content-type **sniffing and dispatch** as resources, for the
[ikigai-core](https://crates.io/crates/ikigai-core) resolution kernel.

Bytes arrive untyped all the time: an HTTP fetch with a missing or wrong `Content-Type`, a
file read, a pasted payload, a content-addressed blob. Everything downstream — which
transreptor to select, which parser to hand it to, what to render — needs a concrete media
type first. This crate binds the two resources that supply one.

Detection is **heuristic only**: it inspects a bounded opening window and never parses, so
it is cheap, total (it always answers) and infallible. No file, network or clock access —
it runs natively and in the browser (`wasm32`) alike.

## Endpoints

| Resource | Arg | Meaning |
| --- | --- | --- |
| `urn:sniff` | `content` | the bytes to classify — usually piped in (e.g. from `urn:httpGet` or a file) |
| `urn:transrept:auto` | `content` | the bytes to transrept, input type unknown |
| | `as` | target media type (default `text/turtle`) |

`urn:sniff` answers *"what is this blob?"* and returns the detected media type as
`text/plain`. `urn:transrept:auto` is the other half of octet-stream sniff-and-dispatch: it
sniffs the bytes, asks the kernel for a transreptor chain from the **detected** type to the
requested `as=`, and runs it — so a caller can convert opaque bytes without knowing what
they are. When the bytes already are the target it is a pass-through; when nothing converts
them it is a clean error naming the sniffed type, never a mis-parse.

`space()` binds both, and since 0.2.0 names itself `urn:iki:space:sniff` (`ikigai_sniff::SPACE_ID`).

```shell
# What did that server actually send?
source urn:httpGet url=https://example.org/thing | urn:sniff

# Convert it to Turtle without asserting what it was:
source urn:httpGet url=https://example.org/thing | urn:transrept:auto as=text/turtle
```

## What it detects

| bytes start with… | → media type |
| --- | --- |
| a binary magic signature (`%PDF-`, PNG, JPEG, GIF, gzip) | `application/pdf` / `image/*` / `application/gzip` |
| `{` / `[` with a JSON-LD keyword (`@context`/`@id`/`@graph`/`@type`) | `application/ld+json` |
| `{` / `[` otherwise | `application/json` |
| `<!doctype html` / `<html` | `text/html` |
| `<rdf:RDF` or the RDF-syntax namespace | `application/rdf+xml` |
| another XML element (`<?xml`, `<Foo`) | `application/xml` |
| `@prefix` / `@base` / `PREFIX` / `BASE` / a comment / an `<scheme:…>` IRI subject | `text/turtle` |
| other valid UTF-8 text | `text/plain` |
| anything else | `application/octet-stream` |

A leading UTF-8 BOM and leading whitespace are skipped first (except before magic bytes,
which are matched at the very start).

### The one subtle row: `<`

Turtle's `<scheme:…>` IRI subject and XML's `<element` tag open with the same byte, and
telling them apart is the whole of the distinction. The obvious test — "does the token
contain `://`?" — is **wrong**, because `://` marks an *authority*, not a scheme, and
`urn:` has none. Under it every `urn:` IRI read as an element tag, so a Turtle document
whose first subject was `<urn:demo:a>` sniffed as RDF/XML and died with an XML
namespace-prefix error raised from a Turtle document.

Three signals on the bracketed token replace it: the token closes within the scan window
with no character Turtle's `IRIREF` production forbids (which rules out every start tag
carrying attributes, plus `<?xml …?>` and `<!DOCTYPE …>`); the text before its first colon
is a valid URI scheme with something after it; and it is not *also* a well-formed XML QName
(the tie-break that keeps an attribute-less `<rdf:RDF>` as markup). So `<urn:…>`,
`<mailto:…>`, `<tag:…>` and `<did:…>` all sniff as Turtle.

What stays ambiguous is the residue: a single-colon, all-name-character IRI such as
`<urn:x>` reads as a tag. Nothing in the token separates the two shapes, so it is
documented rather than guessed at — ikigai's own URNs are `urn:nid:nss` and are unaffected.

## Extending it

Detectors are pluggable. Implement `Detector` (`detect(&[u8]) -> Option<&'static str>`) and
slot it into the priority order in `detectors()`; the first to answer wins, and the
fallbacks (`text/plain` for valid UTF-8, `application/octet-stream` otherwise) catch the
rest. Magic-byte detectors run first so an unambiguous binary signature beats every text
heuristic.

## Caching

Both endpoints are pure functions of their input bytes and are marked `.cacheable()`.
`urn:sniff` reads nothing but the bytes it is handed, so its result rightly carries an
empty golden-thread set. `urn:transrept:auto` delegates to whatever chain the kernel
selects and the kernel folds those resolutions' expiry and threads in — so it is as
cacheable as the chain it ran, and no more.

## Conformance

Passes [`ikigai-conformance`](https://crates.io/crates/ikigai-conformance)
(`tests/conformance.rs`): both endpoints typed, capability-checked, held to their
`.cacheable()` marking as pure functions of their inputs, and both faces resolved. No
opt-outs and no waivers — including `OUTPUTS`, where `urn:transrept:auto` declares the one
face it chooses for itself (`text/turtle`, the target when no `as=` is given) and pins the
rest by hand, since the full set follows `as=` over whatever transreptors the host has
bound and a closed `outputs` list cannot spell that.

## License

Licensed under either of MIT or Apache-2.0 at your option (`MIT OR Apache-2.0`).
