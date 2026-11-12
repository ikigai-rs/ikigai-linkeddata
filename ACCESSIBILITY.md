# Accessibility

What the crates in `ikigai-linkeddata` emit that people read, the rules that output follows, how
it is tested, and how to report a barrier. Most of what these crates answer is data for programs
(RDF syntaxes, SPARQL result sets); the parts people read are one HTML table and the text of
errors. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA for the HTML this repository writes (one table fragment) |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-linkeddata/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-linkeddata/security/advisories/new) |
| Last reviewed | 2026-11-12 |

## What it emits that people read

- **An HTML table of triples.** `urn:rdf:transrept as=text/html` (`to_html` in
  `crates/ikigai-rdf/src/transrept.rs`) renders a graph as one `<table>` with a caption giving
  the triple count, a header row (`subject`, `predicate`, `object`) and one row per triple, each
  term in its N-Triples form. The `text/html` faces that `ikigai-feed`, `ikigai-html` and
  `ikigai-reason` declare are this same table, reached through that transreptor. It is a
  fragment: no `<html>`, `<head>` or `lang`; the page around it is the host's.
- **Error and refusal messages**: the sentences in `Error::Endpoint(format!(…))`,
  `Error::InvalidArgument`, `Error::Timeout` and the others across `crates/*/src`, which a host
  shows to whoever made the request (in a REPL, a terminal, or an HTTP door's error page).
- **Short text/plain answers**: `urn:sniff` answers the detected media type
  (`crates/ikigai-sniff/src/lib.rs`), and `urn:sparql:update` answers a one-line receipt,
  `updated: <before> -> <after> quads` (`crates/ikigai-sparql/src/lib.rs`).

Not this repository's: the page, error page or terminal a host puts any of this in, and HTML that
`ikigai-html` reads (it lifts HTML into RDF; it writes none).

## The rules that output follows

Source observations, reviewed on 2026-11-12; these are not browser or assistive-technology
results. Verification and gaps are recorded below:

- The table has a `<caption>`, a `<thead>` of `<th>` cells naming the three columns, and a
  `<tbody>`; term text is escaped (`&`, `<`, `>`) before it reaches a cell. No style, no script,
  no color.
- An unknown output format is refused, never substituted, and the refusal names the value given
  and lists the ones that work (`urn:rdf:transrept`, and the SPARQL query forms, which also say
  which query forms answer with a graph and which with a result set).
- The refusals for bounds (depth, size, answer size, time budget) name the argument or limit
  they apply to and the number, and the time-budget refusal says what to do: narrow the query, or
  ask the host for a larger budget.
- Nothing is written in color; every answer is plain text or markup.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with evidence. Pass covers only the
stated assertions, not overall accessibility. The test runs below used main commit
`f4fbc31b8a016f1506482921c90111b8ff8eca9a` on 2026-11-12, with Cargo 1.99.1 and
rustc 1.99.1. Run them with `cargo test --workspace`; the repository's
[CI workflow](.github/workflows/ci.yml) runs workspace tests through the shared Rust workflow.
The source observations dated 2026-10-31 in the previous record were not browser runs or
conformance evidence.

| Check | Result | Evidence |
|---|---|---|
| HTML table, triple count and term presence | Pass | `html_view_tabulates_the_triples` in `crates/ikigai-rdf/src/transrept.rs`, run by the command above. Asserts a table opening, `2 triples` and a term; the escaping assertion accepts either `&lt;` or `Ada`. Does not pin a caption element, headers or complete escaping. |
| Caption element, column headers, table body and complete escaping | Blocked | Source review of `to_html` and `esc` in `crates/ikigai-rdf/src/transrept.rs` finds these structures and escaping of `&`, `<`, `>`, but no dedicated assertions pin all of them. CI does not repeat that source review; no browser semantics check was run. |
| Unknown output format refusal | Pass | `unknown_target_is_a_clean_error` in `crates/ikigai-rdf/src/transrept.rs` and `an_unknown_as_is_refused_not_substituted` in `crates/ikigai-sparql/src/lib.rs`, run by the command above. Checks the tested refusals, not every error or host presentation. |
| Depth, size and time-budget refusals | Pass | `crates/ikigai-sparql/tests/sparql_nesting.rs`, `sparql_answer_size.rs`, `sparql_time_budget.rs` and unit tests in `src/budget.rs`, run by the command above. Cover tested argument/limit names, numbers and refusal behavior; `answer_refusals_preserve_consumer_clauses` and `refusals_do_not_expose_internal_ledger_numbers` pin remedies. Do not establish that every error is actionable. |
| Missing transreptor refusal | Pass | `auto_errors_cleanly_when_no_transreptor_reaches_the_target` in `crates/ikigai-sniff/src/lib.rs`, run by the command above. Pins the missing-transreptor message for that request, not all dispatch errors. |
| Plain-text update receipt | Pass | `update_reports_the_quad_delta_and_is_uncacheable` in `crates/ikigai-sparql/src/lib.rs`, run by the command above. Checks exact quad-delta receipts, not their presentation by a host. |
| Plain-text sniff answer | Blocked | Sniff classification unit tests cover media-type selection, but `the_endpoint_reports_the_media_type` checks endpoint metadata, not the answer body. No dedicated check of its presentation was run. |
| Meaning without color, style or script | Blocked | Source review of `to_html` finds no style, script or color; output is text or markup. CI does not repeat a repository-wide audit for color-only meaning, and no rendered host output was evaluated. |
| Error clarity across all endpoints | Blocked | No comprehensive wording evaluation was run. The tests above cover selected refusals only; parser-stage errors remain a known limitation below. |
| Automated accessibility checker (axe or equivalent) | Blocked | No automated accessibility checker was run; no browser checker is configured here. Rust tests do not evaluate WCAG conformance. |
| Browser rendering, zoom, reflow and contrast | Blocked | No browser run was performed. Host styles, language and the long N-Triples terms require evaluation in a consuming page. |
| Manual keyboard | Blocked | No person performed a keyboard pass in a host page or terminal for this review. |
| Screen reader | Blocked | No screen reader session was available or used on this output for this review. |
| Screen magnifier | Blocked | No magnifier session was available or used on this output for this review. |
| Speech input | Blocked | No speech-input session was available or used on this output for this review. |
| Switch access | Blocked | No switch-access session was available or used on this output for this review. |
| Braille display | Blocked | No braille display was available or used on this output for this review. |

## Known limitations

Source observations from 2026-10-31, reviewed again on 2026-11-12; not browser or
assistive-technology findings:

- Several errors name neither the endpoint nor the argument, only the stage and the parser's
  message: for example `RDF parse error: …`, `RDF serialize error: …`, `serialize: …`,
  `query: …` and `store init: …`.
- The table's caption reads `1 triples` for a one-triple graph.
- The table fragment states no language; its English header words take the host page's
  language.
- Each term is shown in its N-Triples form (`<http://…>`, `"…"^^<…>`), which is exact but long
  to listen to; the table has no shorter label for a term.

## Contributing

Read this file before changing any output listed above: the HTML table, an error message, or a
text/plain answer. Keep meaning in words, and keep an error a sentence that names the input and
says what to do. In the pull request, give each accessibility check a row in a
`Check | Result | Evidence` table: Pass, Fail or Blocked, with the command or test, tool and
version, and scope, or why it could not run. Keep unrun checks as Blocked, including assistive
technology nobody used. Historical checks CI does not repeat stay Blocked with their dates.
Keep this record true of main; proposed improvements are not evidence of current behavior.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-linkeddata/issues/new?labels=accessibility)
with the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your browser or terminal and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-linkeddata/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-07 | Removed internal work-item numbers from SPARQL refusals and moved remedies first. |
| 2026-11-12 | Recorded test results and explicit blocked checks, with scope and tool versions. |
