//! html5ever's tree builder, driven into an arena this crate owns.
//!
//! The tree builder is the vetted part: every WHATWG rule (implied tags, the adoption agency,
//! foster parenting, foreign content) is html5ever's. This file is only its [`TreeSink`]: a
//! `Vec` of nodes addressed by index, so nothing here recurses and dropping a deep tree is a
//! loop over a vector, not a chain of destructors.
//!
//! Two guards run WHILE the builder inserts, and both are about cost, not the answer: the input
//! is fed in [`CHUNK`]-byte pieces, and between pieces the parse stops if the tree has grown
//! more than twice as deep or as large as the lift admits. The tree builder's own scope checks
//! walk its stack of open elements, so a document nested a million deep would cost a million
//! squared before any later check ran. The exact bounds (the answer's) are [`Tree::depth`],
//! counted iteratively after the parse and before the walk, and the walk's node count.

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{
    local_name, ns, parse_document, parse_fragment, Attribute, LocalName, Namespace, ParseOpts,
    QualName,
};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

/// Input is handed to the tokenizer this many bytes at a time (to the next char boundary).
pub(crate) const CHUNK: usize = 4096;

pub(crate) enum Kind {
    Document,
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        annotation_xml: bool,
    },
    Text(StrTendril),
    /// A comment or processing instruction: built, because the tree builder asks for one,
    /// and never lifted.
    Other,
}

pub(crate) struct Node {
    pub(crate) kind: Kind,
    parent: Option<usize>,
    pub(crate) children: Vec<usize>,
    /// The element depth as inserted: exact until the adoption agency moves a subtree, close
    /// after. Only the in-parse cost guard reads it.
    depth: usize,
}

/// The parsed tree: node 0 is the document.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node>,
}

impl Tree {
    /// The nodes the lift roots at: the document's for a document, the `html` element's that
    /// html5ever's fragment algorithm wraps the fragment in for a fragment.
    pub(crate) fn top(&self, fragment: bool) -> &[usize] {
        let document = &self.nodes[0].children;
        if !fragment {
            return document;
        }
        document
            .iter()
            .find(|&&i| matches!(self.nodes[i].kind, Kind::Element { .. }))
            .map(|&i| self.nodes[i].children.as_slice())
            .unwrap_or(&[])
    }

    /// The deepest element nesting under `top`, counted with an explicit stack.
    pub(crate) fn depth(&self, top: &[usize]) -> usize {
        let mut deepest = 0;
        let mut stack: Vec<(usize, usize)> = top.iter().map(|&i| (i, 0)).collect();
        while let Some((i, above)) = stack.pop() {
            let node = &self.nodes[i];
            if let Kind::Element { .. } = node.kind {
                let depth = above + 1;
                deepest = deepest.max(depth);
                stack.extend(node.children.iter().map(|&c| (c, depth)));
            }
        }
        deepest
    }
}

/// Why a parse stopped early: the tree outgrew the guard.
pub(crate) struct Outgrown;

/// Parse `html` as a document, or as a fragment in a `<body>`, refusing once the tree passes
/// `max_depth` deep or `max_nodes` nodes (elements, attributes, texts).
pub(crate) fn parse(
    html: &str,
    fragment: bool,
    max_depth: usize,
    max_nodes: usize,
) -> Result<Tree, Outgrown> {
    let sink = Sink {
        nodes: RefCell::new(vec![Node {
            kind: Kind::Document,
            parent: None,
            children: Vec::new(),
            depth: 0,
        }]),
        made: Cell::new(0),
        deepest: Cell::new(0),
        max_depth,
        max_nodes,
    };
    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            // No script runs here, so `<noscript>` content is markup, as a reader sees it.
            scripting_enabled: false,
            drop_doctype: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut parser = if fragment {
        let body = QualName::new(None, ns!(html), local_name!("body"));
        parse_fragment(sink, opts, body, Vec::new(), false)
    } else {
        parse_document(sink, opts)
    };
    let mut rest = html;
    while !rest.is_empty() {
        let mut at = CHUNK.min(rest.len());
        while !rest.is_char_boundary(at) {
            at += 1;
        }
        let (chunk, tail) = rest.split_at(at);
        parser.process(StrTendril::from_slice(chunk));
        if parser.tokenizer.sink.sink.outgrown() {
            return Err(Outgrown);
        }
        rest = tail;
    }
    let sink = parser.finish();
    if sink.outgrown() {
        return Err(Outgrown);
    }
    Ok(Tree {
        nodes: sink.nodes.into_inner(),
    })
}

struct Sink {
    nodes: RefCell<Vec<Node>>,
    made: Cell<usize>,
    deepest: Cell<usize>,
    max_depth: usize,
    max_nodes: usize,
}

impl Sink {
    fn outgrown(&self) -> bool {
        self.deepest.get() > self.max_depth || self.made.get() > self.max_nodes
    }

    fn push(&self, kind: Kind) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push(Node {
            kind,
            parent: None,
            children: Vec::new(),
            depth: 0,
        });
        nodes.len() - 1
    }

    fn text(&self, text: StrTendril) -> usize {
        self.made.set(self.made.get() + 1);
        self.push(Kind::Text(text))
    }

    fn detach(nodes: &mut [Node], child: usize) {
        if let Some(parent) = nodes[child].parent.take() {
            nodes[parent].children.retain(|&c| c != child);
        }
    }

    /// Place `child` under `parent` at `index`, and note its depth.
    fn place(&self, parent: usize, index: usize, child: usize) {
        let mut nodes = self.nodes.borrow_mut();
        Self::detach(&mut nodes, child);
        let element = matches!(nodes[child].kind, Kind::Element { .. });
        let depth = nodes[parent].depth + usize::from(element);
        nodes[child].parent = Some(parent);
        nodes[child].depth = depth;
        nodes[parent].children.insert(index, child);
        if depth > self.deepest.get() {
            self.deepest.set(depth);
        }
    }

    /// The text node at `index - 1` under `parent`, if there is one to extend.
    fn text_before(&self, parent: usize, index: usize) -> Option<usize> {
        let nodes = self.nodes.borrow();
        let before = *nodes[parent].children.get(index.checked_sub(1)?)?;
        matches!(nodes[before].kind, Kind::Text(_)).then_some(before)
    }

    fn insert(&self, parent: usize, index: usize, child: NodeOrText<usize>) {
        match child {
            NodeOrText::AppendNode(node) => self.place(parent, index, node),
            NodeOrText::AppendText(text) => match self.text_before(parent, index) {
                Some(before) => {
                    if let Kind::Text(t) = &mut self.nodes.borrow_mut()[before].kind {
                        t.push_tendril(&text);
                    }
                }
                None => {
                    let node = self.text(text);
                    self.place(parent, index, node);
                }
            },
        }
    }
}

/// An element's name, as the tree builder asks for it.
#[derive(Debug)]
pub(crate) struct Name(QualName);

impl html5ever::tree_builder::ElemName for Name {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }
    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl TreeSink for Sink {
    type Handle = usize;
    type Output = Self;
    type ElemName<'a> = Name;

    fn finish(self) -> Self {
        self
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> usize {
        0
    }

    fn elem_name(&self, target: &usize) -> Name {
        match &self.nodes.borrow()[*target].kind {
            Kind::Element { name, .. } => Name(name.clone()),
            _ => panic!("not an element"),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> usize {
        self.made.set(self.made.get() + 1 + attrs.len());
        self.push(Kind::Element {
            name,
            attrs,
            annotation_xml: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, _text: StrTendril) -> usize {
        self.push(Kind::Other)
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> usize {
        self.push(Kind::Other)
    }

    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        let index = self.nodes.borrow()[*parent].children.len();
        self.insert(*parent, index, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &usize,
        prev_element: &usize,
        child: NodeOrText<usize>,
    ) {
        if self.nodes.borrow()[*element].parent.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}

    /// A template's contents are its children here: the lift reads them as written.
    fn get_template_contents(&self, target: &usize) -> usize {
        *target
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &usize, child: NodeOrText<usize>) {
        let (parent, index) = {
            let nodes = self.nodes.borrow();
            let Some(parent) = nodes[*sibling].parent else {
                return;
            };
            let index = nodes[parent]
                .children
                .iter()
                .position(|c| c == sibling)
                .unwrap_or(0);
            (parent, index)
        };
        // A node moving within the same parent shifts the sibling's index once it is detached.
        if let NodeOrText::AppendNode(node) = &child {
            let mut nodes = self.nodes.borrow_mut();
            Self::detach(&mut nodes, *node);
            let index = nodes[parent]
                .children
                .iter()
                .position(|c| c == sibling)
                .unwrap_or(0);
            drop(nodes);
            return self.place(parent, index, *node);
        }
        self.insert(parent, index, child);
    }

    fn add_attrs_if_missing(&self, target: &usize, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        if let Kind::Element { attrs: have, .. } = &mut nodes[*target].kind {
            for attr in attrs {
                if !have.iter().any(|a| a.name == attr.name) {
                    have.push(attr);
                    self.made.set(self.made.get() + 1);
                }
            }
        }
    }

    fn remove_from_parent(&self, target: &usize) {
        Self::detach(&mut self.nodes.borrow_mut(), *target);
    }

    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let children = std::mem::take(&mut self.nodes.borrow_mut()[*node].children);
        for child in children {
            self.nodes.borrow_mut()[child].parent = None;
            let index = self.nodes.borrow()[*new_parent].children.len();
            self.place(*new_parent, index, child);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &usize) -> bool {
        matches!(
            self.nodes.borrow()[*handle].kind,
            Kind::Element {
                annotation_xml: true,
                ..
            }
        )
    }
}
