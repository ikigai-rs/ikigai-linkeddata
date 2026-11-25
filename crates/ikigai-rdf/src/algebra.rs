//! Repeatability hazards in parsed SPARQL queries. Enable the `sparql-algebra` feature.
//!
//! The feature enables spargebra's LATERAL and SPARQL 1.2 algebra variants so the
//! walk is exhaustive even when a consumer enables them. If the same build links
//! Oxigraph 0.5, enable its `rdf-12` feature as well to keep its term adapters
//! aligned with the shared parser and RDF types.
//!
//! This is a structural check, not an evaluator or cache policy. An empty set means none
//! of the modeled hazards were found. The caller still needs fixed inputs and evaluator
//! configuration, and a policy for extension functions and aggregates, SERVICE, DESCRIBE,
//! result ordering, and blank nodes from the input. Run after the caller's parse and
//! complexity guards. The limits here protect this walk, not parsing, evaluation, or the
//! destruction of the caller's query.

use std::{collections::BTreeSet, fmt};

use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
};
use spargebra::term::{TermPattern, TriplePattern};
use spargebra::Query;

/// Maximum depth of a visited node, with the root graph pattern at depth one.
pub const MAX_DEPTH: usize = 128;
/// Maximum number of graph patterns, expressions, sort keys, aggregates, template triples
/// and template terms visited in total. The contents of BGP, path and VALUES patterns
/// cannot contain the hazards this walk checks and are not visited.
pub const MAX_NODES: usize = 16_384;

/// A dependence on execution time, fresh identity, arbitrary selection, or ordering.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum RepeatabilityReason {
    Now,
    Rand,
    Uuid,
    StrUuid,
    BNode,
    Slice,
    Sample,
    GroupConcat,
    Reduced,
    /// Matters for exact RDF term identity, not for graph isomorphism alone.
    ConstructTemplateBlankNode,
}

impl fmt::Display for RepeatabilityReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Now => "NOW() depends on the query execution instant.",
            Self::Rand => "RAND() draws a pseudo-random value on evaluation.",
            Self::Uuid => "UUID() creates a fresh IRI on evaluation.",
            Self::StrUuid => "STRUUID() creates a fresh UUID string on evaluation.",
            Self::BNode => "BNODE() allocates blank nodes without a stable identity across evaluations.",
            Self::Slice => "LIMIT or OFFSET can select different rows without a proven total order, even with ORDER BY.",
            Self::Sample => "SAMPLE chooses an arbitrary member of the group.",
            Self::GroupConcat => "GROUP_CONCAT concatenates group members in an unspecified order.",
            Self::Reduced => "REDUCED allows the evaluator to vary how many duplicate rows it keeps.",
            Self::ConstructTemplateBlankNode => "A blank node in a CONSTRUCT template creates a fresh blank node on evaluation.",
        })
    }
}

/// Classification could not finish within its traversal budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraversalLimit {
    Depth,
    Nodes,
}

impl fmt::Display for TraversalLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Depth => write!(f, "SPARQL repeatability classification exceeds the algebra depth limit of {MAX_DEPTH}. Reduce query nesting."),
            Self::Nodes => write!(f, "SPARQL repeatability classification exceeds the algebra node limit of {MAX_NODES}. Simplify the query."),
        }
    }
}

impl std::error::Error for TraversalLimit {}

enum Node<'a> {
    Pattern(&'a GraphPattern),
    Expr(&'a Expression),
    Order(&'a OrderExpression),
    Aggregate(&'a AggregateExpression),
    TemplateTriple(&'a TriplePattern),
    TemplateTerm(&'a TermPattern),
}

#[derive(Default)]
struct Walk<'a> {
    stack: Vec<(Node<'a>, usize)>,
    scheduled: usize,
}

impl<'a> Walk<'a> {
    fn push(&mut self, node: Node<'a>, depth: usize) -> Result<(), TraversalLimit> {
        if depth > MAX_DEPTH {
            return Err(TraversalLimit::Depth);
        }
        // Charge at enqueue time so a single wide argument list cannot grow
        // the stack past the budget before any of its children is visited.
        if self.scheduled == MAX_NODES {
            return Err(TraversalLimit::Nodes);
        }
        self.scheduled += 1;
        self.stack.push((node, depth));
        Ok(())
    }
}

/// Collect all modeled hazards in `query`, once per reason. Every slice is flagged,
/// including ordered and nested slices. On a limit error, treat classification as
/// refused. No partial set is returned that could be mistaken for a complete answer.
pub fn repeatability_reasons(
    query: &Query,
) -> Result<BTreeSet<RepeatabilityReason>, TraversalLimit> {
    use Node::*;
    use RepeatabilityReason::*;

    let mut walk = Walk::default();
    let pattern = match query {
        Query::Select { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
        Query::Construct {
            template, pattern, ..
        } => {
            for triple in template {
                walk.push(TemplateTriple(triple), 1)?;
            }
            pattern
        }
    };
    walk.push(Pattern(pattern), 1)?;
    let mut reasons = BTreeSet::new();
    while let Some((node, depth)) = walk.stack.pop() {
        let mut push = |child| walk.push(child, depth + 1);
        match node {
            Pattern(pattern) => match pattern {
                GraphPattern::Bgp { .. }
                | GraphPattern::Path { .. }
                | GraphPattern::Values { .. } => {}
                GraphPattern::Graph { inner, .. }
                | GraphPattern::Service { inner, .. }
                | GraphPattern::Project { inner, .. }
                | GraphPattern::Distinct { inner } => push(Pattern(inner))?,
                GraphPattern::Reduced { inner } => {
                    reasons.insert(Reduced);
                    push(Pattern(inner))?;
                }
                GraphPattern::Slice { inner, .. } => {
                    reasons.insert(Slice);
                    push(Pattern(inner))?;
                }
                GraphPattern::Join { left, right }
                | GraphPattern::Lateral { left, right }
                | GraphPattern::Union { left, right }
                | GraphPattern::Minus { left, right } => {
                    push(Pattern(left))?;
                    push(Pattern(right))?;
                }
                GraphPattern::LeftJoin {
                    left,
                    right,
                    expression,
                } => {
                    push(Pattern(left))?;
                    push(Pattern(right))?;
                    if let Some(expr) = expression {
                        push(Expr(expr))?;
                    }
                }
                GraphPattern::Filter { expr, inner }
                | GraphPattern::Extend {
                    expression: expr,
                    inner,
                    ..
                } => {
                    push(Pattern(inner))?;
                    push(Expr(expr))?;
                }
                GraphPattern::OrderBy { inner, expression } => {
                    push(Pattern(inner))?;
                    for order in expression {
                        push(Order(order))?;
                    }
                }
                GraphPattern::Group {
                    inner, aggregates, ..
                } => {
                    push(Pattern(inner))?;
                    for (_, aggregate) in aggregates {
                        push(Aggregate(aggregate))?;
                    }
                }
            },
            Expr(expr) => match expr {
                Expression::NamedNode(_)
                | Expression::Literal(_)
                | Expression::Variable(_)
                | Expression::Bound(_) => {}
                Expression::Exists(pattern) => push(Pattern(pattern))?,
                Expression::FunctionCall(function, args) => {
                    let reason = match function {
                        Function::Now => Some(Now),
                        Function::Rand => Some(Rand),
                        Function::Uuid => Some(Uuid),
                        Function::StrUuid => Some(StrUuid),
                        Function::BNode => Some(BNode),
                        // All call arguments are still traversed, including extensions.
                        _ => None,
                    };
                    reasons.extend(reason);
                    for arg in args {
                        push(Expr(arg))?;
                    }
                }
                Expression::Coalesce(args) => {
                    for arg in args {
                        push(Expr(arg))?;
                    }
                }
                Expression::In(e, members) => {
                    push(Expr(e))?;
                    for member in members {
                        push(Expr(member))?;
                    }
                }
                Expression::If(a, b, c) => {
                    push(Expr(a))?;
                    push(Expr(b))?;
                    push(Expr(c))?;
                }
                Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
                    push(Expr(e))?;
                }
                Expression::Or(a, b)
                | Expression::And(a, b)
                | Expression::Equal(a, b)
                | Expression::SameTerm(a, b)
                | Expression::Greater(a, b)
                | Expression::GreaterOrEqual(a, b)
                | Expression::Less(a, b)
                | Expression::LessOrEqual(a, b)
                | Expression::Add(a, b)
                | Expression::Subtract(a, b)
                | Expression::Multiply(a, b)
                | Expression::Divide(a, b) => {
                    push(Expr(a))?;
                    push(Expr(b))?;
                }
            },
            Order(OrderExpression::Asc(expr) | OrderExpression::Desc(expr)) => push(Expr(expr))?,
            Aggregate(aggregate) => match aggregate {
                AggregateExpression::CountSolutions { .. } => {}
                AggregateExpression::FunctionCall { name, expr, .. } => {
                    match name {
                        AggregateFunction::Sample => {
                            reasons.insert(Sample);
                        }
                        AggregateFunction::GroupConcat { .. } => {
                            reasons.insert(GroupConcat);
                        }
                        AggregateFunction::Count
                        | AggregateFunction::Sum
                        | AggregateFunction::Avg
                        | AggregateFunction::Min
                        | AggregateFunction::Max
                        | AggregateFunction::Custom(_) => {}
                    }
                    push(Expr(expr))?;
                }
            },
            TemplateTriple(triple) => {
                push(TemplateTerm(&triple.subject))?;
                push(TemplateTerm(&triple.object))?;
            }
            TemplateTerm(term) => match term {
                TermPattern::BlankNode(_) => {
                    reasons.insert(ConstructTemplateBlankNode);
                }
                TermPattern::Triple(triple) => push(TemplateTriple(triple))?,
                TermPattern::NamedNode(_) | TermPattern::Literal(_) | TermPattern::Variable(_) => {}
            },
        }
    }
    Ok(reasons)
}

#[cfg(test)]
mod tests;
