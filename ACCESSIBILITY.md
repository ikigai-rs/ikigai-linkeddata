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
| Last reviewed | 2026-10-31 |

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

Followed by reading the source:

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

- `html_view_tabulates_the_triples` (unit test in `crates/ikigai-rdf/src/transrept.rs`) checks
  that the HTML face is a table, that its caption carries the count and that the terms appear.
  Nothing pins the caption element, the header cells or the escaping.
- Several tests pin error wording: `crates/ikigai-rdf/src/transrept.rs` and
  `crates/ikigai-sparql/src/lib.rs` check that an unknown target is refused naming the value;
  `crates/ikigai-sparql/tests/sparql_nesting.rs`, `sparql_time_budget.rs` and `src/budget.rs`
  check that bound refusals name the argument, the limit and the number; `crates/ikigai-sniff`'s
  tests check that a missing transreptor is named.
- **No automated accessibility checker, browser or assistive technology has been used on this
  output.**

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

- Several errors name neither the endpoint nor the argument, only the stage and the parser's
  message: for example `RDF parse error: …`, `RDF serialize error: …`, `serialize: …`,
  `query: …` and `store init: …`.
- A few refusals in `ikigai-sparql` cite an internal work-item number in parentheses (in
  `src/budget.rs`, the time-budget, answer-size and bound-argument refusals; in `src/egress.rs`,
  the `SERVICE` refusal), which a reader outside the project cannot look up.
- The table's caption reads `1 triples` for a one-triple graph.
- The table fragment states no language; its English header words take the host page's
  language.
- Each term is shown in its N-Triples form (`<http://…>`, `"…"^^<…>`), which is exact but long
  to listen to; the table has no shorter label for a term.

## Contributing

Read this file before changing any output listed above: the HTML table, an error message, or a
text/plain answer. Keep meaning in words, and keep an error a sentence that names the input and
says what to do. Say in the pull request what was tested, with what, and what was not.

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
