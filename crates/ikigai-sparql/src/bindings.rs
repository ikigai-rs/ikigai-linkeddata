//! `bindings=` on the four query forms (ledger #2517): a value reaches a query as a TERM bound
//! into the parsed query, never as text spliced into it.
//!
//! A parameterized request (`urn:quakes:near:{region}`, a saved search filled from a form)
//! needs a caller's value in its query. Spliced into the text, a value that holds a `"` or a
//! `>` ends its literal or IRI early and the rest is parsed as SPARQL: an injection and a
//! broken query in one (ledger #2494 found it in the RSS demo). Through `bindings=` the query is
//! parsed first, with the variable in it, and each value is handed to oxigraph's
//! `substitute_variable` as an RDF term, so its bytes never reach the parser.
//!
//! The shape is `ikigai-store`'s `bindings=`, which is the term object of the [SPARQL 1.1 Query
//! Results JSON Format](https://www.w3.org/TR/sparql11-results-json/) with JSON's own scalars as
//! shorthand: a row read out of either crate's query binds into the other's unchanged.
//!
//! ⚠ **Every bound variable must be in the query's projection** (`SELECT *`, or named in the
//! SELECT clause; ASK and CONSTRUCT accept any variable in scope in the pattern, and DESCRIBE
//! the variables it names, measured against oxigraph: `DESCRIBE ?q WHERE { ?q :p ?r }` refuses
//! a binding of `?r`, and `DESCRIBE ?q ?r` takes it).
//! That is oxigraph's rule, and it settles the design question the right way: a binding the
//! query does not mention is refused, never ignored, so a filter the caller thought applied
//! can never silently not be. [`refusal`] turns oxigraph's error into one naming `bindings`.

use ikigai_core::{Error, Invocation, Result};
use oxigraph::model::{Literal, NamedNode, Term, Variable};

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

/// The request's `bindings=`, parsed, or none when the argument is absent.
///
/// ⚠ Absent and UNREADABLE are different answers: a `bindings` present but not inline UTF-8 is
/// refused, never read as "no bindings", which would run the query unfiltered and return every
/// row.
pub(crate) fn from_request(inv: &Invocation<'_>) -> Result<Vec<(Variable, Term)>> {
    if !inv.request.args.contains_key("bindings") {
        return Ok(Vec::new());
    }
    parse(inv.inline_str("bindings")?)
}

/// Refuse a `bindings` argument, present in any form, on a door that cannot honour one:
/// oxigraph binds into a prepared query and offers nothing for a prepared update.
pub(crate) fn refuse(inv: &Invocation<'_>, iri: &str) -> Result<()> {
    if !inv.request.args.contains_key("bindings") {
        return Ok(());
    }
    Err(bad(format!(
        "`{iri}` takes no `bindings`: oxigraph binds terms into a parsed query and has no such \
         door for an update, and a binding honoured by rewriting the text would be the splice \
         it exists to replace. It is refused rather than ignored. Write the terms into the \
         update, or select what the update needs and write the rows"
    )))
}

/// Parse a `bindings=` argument into variable → term pairs.
///
/// | JSON | term |
/// | --- | --- |
/// | `"text"` | a plain (`xsd:string`) literal |
/// | `12` | `xsd:integer` |
/// | `1.5` | `xsd:double` |
/// | `true` | `xsd:boolean` |
/// | `{"type":"uri","value":"urn:…"}` | an IRI |
/// | `{"type":"literal","value":"…","datatype":"…"}` | a typed literal |
/// | `{"type":"literal","value":"…","xml:lang":"en"}` | a language-tagged literal |
///
/// Everything else is refused with the reason, never coerced: `null` (an unbound variable is
/// not a binding), an array or nested object (no term means that), and a blank node (scoped to
/// the document that minted it, so it names nothing a query can match).
pub(crate) fn parse(json: &str) -> Result<Vec<(Variable, Term)>> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        bad(format!(
            "not JSON: {e}. `bindings` is a JSON object of variable name → value, e.g. \
             {{\"title\": \"some text\"}}"
        ))
    })?;
    let serde_json::Value::Object(map) = value else {
        return Err(bad(
            "must be a JSON OBJECT of variable name → value, e.g. {\"title\": \"some text\"}"
                .to_string(),
        ));
    };
    map.into_iter()
        .map(|(name, value)| {
            // Held to `ikigai-store`'s set, so a name one crate accepts the other does too, and
            // checked here so the refusal names the key, which oxigraph's would not.
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Err(bad(format!(
                    "`{name}` is not a SPARQL variable name (letters, digits and `_`); write it \
                     without the leading `?`"
                )));
            }
            let variable = Variable::new(&name)
                .map_err(|e| bad(format!("`{name}` is not a variable name: {e}")))?;
            Ok((variable, term(&name, value)?))
        })
        .collect()
}

fn term(name: &str, value: serde_json::Value) -> Result<Term> {
    use serde_json::Value;
    match value {
        Value::String(s) => Ok(Literal::new_simple_literal(s).into()),
        Value::Bool(b) => Ok(Literal::new_typed_literal(b.to_string(), xsd(XSD_BOOLEAN)).into()),
        Value::Number(n) => {
            let datatype = if n.is_f64() { XSD_DOUBLE } else { XSD_INTEGER };
            Ok(Literal::new_typed_literal(n.to_string(), xsd(datatype)).into())
        }
        Value::Null => Err(bad(format!(
            "`{name}` is null, and an unbound variable is not a binding: a query that should \
             match anything must simply not bind that variable"
        ))),
        Value::Array(_) => Err(bad(format!(
            "`{name}` is an array, and there is no RDF term that could mean. Bind one value, or \
             write the alternatives into the query as a VALUES block"
        ))),
        Value::Object(o) => object_term(name, o),
    }
}

fn object_term(name: &str, object: serde_json::Map<String, serde_json::Value>) -> Result<Term> {
    let string = |key: &str| object.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let kind = string("type").ok_or_else(|| {
        bad(format!(
            "`{name}` is an object with no `type`: use the SPARQL results term shape, \
             {{\"type\":\"uri\"|\"literal\",\"value\":…}}, or a bare JSON string for a plain \
             literal"
        ))
    })?;
    let lexical = string("value")
        .ok_or_else(|| bad(format!("`{name}` is a `{kind}` term with no `value`")))?;
    match kind.as_str() {
        "uri" | "iri" => Ok(named_node(name, &lexical)?.into()),
        "literal" | "typed-literal" => match (
            string("xml:lang").or_else(|| string("lang")),
            string("datatype"),
        ) {
            (Some(_), Some(_)) => Err(bad(format!(
                "`{name}` carries both `xml:lang` and `datatype`; a literal has one or the other"
            ))),
            (Some(tag), None) => Literal::new_language_tagged_literal(lexical, &tag)
                .map(Into::into)
                .map_err(|e| bad(format!("`{name}`: `{tag}` is not a language tag: {e}"))),
            (None, Some(dt)) => {
                Ok(Literal::new_typed_literal(lexical, named_node(name, &dt)?).into())
            }
            (None, None) => Ok(Literal::new_simple_literal(lexical).into()),
        },
        "bnode" => Err(bad(format!(
            "`{name}` is a blank node, which is scoped to the document that minted it and so \
             names nothing a query can match. Skolemize it and bind the IRI"
        ))),
        other => Err(bad(format!(
            "`{name}` has type `{other}`; one of `uri`, `literal`"
        ))),
    }
}

fn named_node(name: &str, value: &str) -> Result<NamedNode> {
    NamedNode::new(value).map_err(|e| {
        bad(format!(
            "`{name}`: `{value}` is not an IRI ({e}). It is refused rather than escaped because \
             an IRI has no escape for `<`, `>`, `\"`, `{{`, `}}`, `|`, `^`, `` ` ``, `\\` or a \
             space: percent-encode it"
        ))
    })
}

fn xsd(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

/// The error a query evaluation failed with, as a refusal of `bindings` when the bindings are
/// what it refused: oxigraph's own sentence names the projection but not the argument, nor
/// the fix.
pub(crate) fn refusal(evaluation: String, bound: &[(Variable, Term)]) -> Error {
    if bound.is_empty() || !evaluation.contains("projection") {
        return Error::Endpoint(format!("query evaluation error: {evaluation}"));
    }
    let names: Vec<String> = bound.iter().map(|(v, _)| v.to_string()).collect();
    bad(format!(
        "{evaluation}. Every bound variable ({}) must appear in the query's projection: name it \
         in the SELECT clause or write `SELECT *`; ASK and CONSTRUCT accept any variable in \
         scope in the pattern, and DESCRIBE the variables it names (`DESCRIBE ?s ?v`). A variable that occurs only inside `FILTER (NOT) \
         EXISTS` or on a `MINUS`'s right side is not in scope there, and binds only when a \
         SELECT names it. A binding the query does not mention is refused rather than \
         ignored, so that a filter you thought was applied can never silently not be",
        names.join(", "),
    ))
}

fn bad(detail: String) -> Error {
    Error::InvalidArgument {
        name: "bindings".to_string(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_json_shape_is_its_term() {
        let bound = parse(
            r#"{"s": "a \" b", "n": 3, "x": 1.5, "b": true,
                "i": {"type": "uri", "value": "urn:a"},
                "l": {"type": "literal", "value": "chat", "xml:lang": "fr"},
                "d": {"type": "literal", "value": "2026-10-28T00:00:00Z",
                      "datatype": "http://www.w3.org/2001/XMLSchema#dateTime"}}"#,
        )
        .unwrap();
        let text: std::collections::BTreeMap<String, String> = bound
            .iter()
            .map(|(v, t)| (v.as_str().to_string(), t.to_string()))
            .collect();
        assert_eq!(text["s"], r#""a \" b""#);
        assert_eq!(text["n"], format!("\"3\"^^<{XSD_INTEGER}>"));
        assert_eq!(text["x"], format!("\"1.5\"^^<{XSD_DOUBLE}>"));
        assert_eq!(text["b"], format!("\"true\"^^<{XSD_BOOLEAN}>"));
        assert_eq!(text["i"], "<urn:a>");
        assert_eq!(text["l"], "\"chat\"@fr");
        assert!(text["d"].ends_with("XMLSchema#dateTime>"), "{}", text["d"]);
    }
}
