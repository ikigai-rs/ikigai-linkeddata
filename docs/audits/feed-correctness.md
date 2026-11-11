# Feed correctness audit

Synthetic inputs only; no live feeds or publishing. This is a bounded correctness audit,
not a claim of complete RSS or Atom conformance.

## Reproduced and fixed

`cargo test -p ikigai-feed --test audit` reproduces these failures against the original code:

- `atom_author_parentheses_are_name_text`: `(Editorial)` becomes `Editorial`, and
  `Ada (Editor)` becomes `Ada (Editor`. `rss_contact_delimiters_leave_parentheses_inside_the_name`
  also catches lost parentheses in RSS names. Strip one matched outer pair only for RSS
  contacts with an email; Atom names remain text (RFC 4287 section 3.2.1).
- `invalid_rss_hour_is_omitted_instead_of_reset_to_midnight`: `07 Oct 2026 24:30:00 GMT`
  becomes `2026-10-07T00:30:00Z`. Supply a timestamp parser that validates clock fields.
- `atom_date_requires_a_time_and_zone`: `2026-10-07` becomes midnight UTC, and an RFC 822
  timestamp is accepted as an Atom date. Require complete RFC 3339 dates for Atom
  (RFC 4287 section 3.3). `invalid_atom_updated_cannot_replace_a_valid_version` reproduces
  the consequence: an invalid future date replaces a valid version sharing the same id.
- `rss_comments_and_two_digit_year_boundary_keep_the_instant`: a valid RSS date with
  comments between fields, `07 (day) Oct 49 12:00:00 GMT (zone (nested))`, is dropped.
  Remove RFC 5322 comments before parsing fields and normalizing the zone spelling.
- `atom_registered_relation_iris_mean_the_same_as_short_names`: expanding registered
  names to their equivalent IANA relation IRIs changes channel and item identity, drops
  `rss:link`, source-feed links and enclosure length/type, and substitutes generic relations.
  Normalize relation spelling wherever the lift or pre-scan reads it
  (RFC 4287 section 4.2.7.2).

## Coverage and limits

| Area | Executable evidence |
| --- | --- |
| Identity, guid flags, missing ids, feed scoping, duplicate versions and ties, no blank nodes | Existing `tests/lift.rs` and `tests/identity.rs`; invalid-version regression in `tests/audit.rs` |
| RSS named and numeric zones, `Z`, two-digit year boundaries, comments; Atom fractional seconds and offsets; issued versus modified | `tests/audit.rs`, existing `dates_are_xsd_date_times_from_both_formats` |
| Military zones | `rss_military_zones_follow_the_rfc5322_unknown_offset_policy`: unchanged RFC 5322 section 4.3 interpretation as `-0000`, because RFC 822 reversed the military offsets; written zones are not retained |
| Atom text, HTML, XHTML; RSS description and encoded content, entities and CDATA | `tests/audit.rs`: literal content decoded once, markup preserved; text and HTML both use `xsd:string`, so content kind is not retained |
| Relative links, nested `xml:base`, query/fragment preservation, prefixed Atom | `prefixed_atom_resolves_nested_bases_and_keeps_xhtml_markup`; existing link/enclosure tests |
| Authors, contributors, email and URI | `person_projection_uses_author_name_else_email`: name else email is the projected creator; person URI and contributors are not projected |
| Categories, scheme and term, enclosure length/type, multiple link relations, RSS 1.0 subjects | Existing `tests/lift.rs`; relation-IRI regression in `tests/audit.rs` |
| RSS 0.91/0.92/2.0 and RSS 1.0 | Version loop in `tests/audit.rs`; existing RSS 1.0 fixtures and river tests |
| Determinism | `identical_input_produces_byte_identical_turtle`: repeated endpoint calls including a missing-id entry and multiple categories; existing duplicate-version test |
| External relative content | `relative_external_content_is_a_documented_loss`: relative Atom content src is still omitted, as the README describes |

The form is a lossy projection, not an Atom validator. Feed/source author inheritance,
all possible date productions, all URI schemes, arbitrary extensions, and protocol serving
by an external HTTP host were not exhaustively evaluated. No accessibility checker, browser,
or assistive technology was used; the changes affect RDF data and documentation, not the HTML
view or error messages.

Specifications: [RSS 2.0](https://www.rssboard.org/rss-specification),
[Atom RFC 4287](https://www.rfc-editor.org/rfc/rfc4287), and
[RFC 5322 obsolete dates](https://www.rfc-editor.org/rfc/rfc5322#section-4.3).
