//! Regression coverage for caller-owned namespaces and shared-store read authority.
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Expiry, Fallback, Iri, Kernel, Request, Resolution,
    Scope, Space, Verb,
};
use ikigai_sparql::{
    budget::{AnswerBound, DEFAULT_BUDGET},
    space, space_with_store, space_with_store_and_bounds, space_with_store_and_budget,
    space_with_store_at, space_with_store_at_and_bounds, space_with_store_at_and_budget, Store,
    CAP_QUERY, CAP_UPDATE,
};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use std::{collections::HashSet, sync::Arc, time::Duration};

const FORMS: [(&str, &str); 4] = [
    ("select", "SELECT ?o WHERE { ?s <urn:p> ?o }"),
    ("ask", "ASK { <urn:s> <urn:p> ?o }"),
    (
        "construct",
        "CONSTRUCT { ?s <urn:p> ?o } WHERE { ?s <urn:p> ?o }",
    ),
    ("describe", "DESCRIBE <urn:s>"),
];

fn store_with_value(value: &str) -> Arc<Store> {
    let store = Arc::new(Store::new().unwrap());
    store
        .insert(&Quad::new(
            NamedNode::new("urn:s").unwrap(),
            NamedNode::new("urn:p").unwrap(),
            Literal::new_simple_literal(value),
            GraphName::NamedNode(NamedNode::new("urn:private:graph").unwrap()),
        ))
        .unwrap();
    store
}

fn request(verb: Verb, target: &str, arg: &str, text: &str) -> Request {
    Request::new(verb, Iri::parse(target).unwrap())
        .with_arg(arg, ArgRef::Inline(text.as_bytes().to_vec()))
}

fn shared_spaces(store: &Arc<Store>) -> Vec<(&str, EndpointSpace)> {
    vec![
        ("urn:sparql:", space_with_store(store.clone())),
        (
            "urn:sparql:",
            space_with_store_and_budget(store.clone(), DEFAULT_BUDGET),
        ),
        (
            "urn:sparql:",
            space_with_store_and_bounds(store.clone(), DEFAULT_BUDGET, AnswerBound::DEFAULT),
        ),
        (
            "urn:host:store:",
            space_with_store_at("urn:host:store:", store.clone()),
        ),
        (
            "https://example.com/store/",
            space_with_store_at_and_budget(
                "https://example.com/store/",
                store.clone(),
                DEFAULT_BUDGET,
            ),
        ),
        (
            "urn:bounded:",
            space_with_store_at_and_bounds(
                "urn:bounded:",
                store.clone(),
                DEFAULT_BUDGET,
                AnswerBound::DEFAULT,
            ),
        ),
    ]
}

#[test]
fn shared_store_queries_require_read_authority() {
    let store = store_with_value("private");
    for (prefix, space) in shared_spaces(&store) {
        let kernel = Kernel::new(Arc::new(space));
        for (form, query) in FORMS {
            let req = request(Verb::Source, &format!("{prefix}{form}"), "query", query);
            for cap in [
                Capability::scoped(["urn:cap:unrelated"]),
                Capability::scoped([CAP_UPDATE]),
            ] {
                let error = block_on(kernel.issue(req.clone(), &cap)).unwrap_err();
                assert!(
                    matches!(error, Error::Denied(_)),
                    "{prefix}{form}: {error:?}"
                );
                assert!(error.to_string().contains(CAP_QUERY));
                assert!(!error.is_transient());
            }
            let out = block_on(kernel.issue(req, &Capability::scoped([CAP_QUERY]))).unwrap();
            assert_eq!(out.expiry, Expiry::Always);
            let text = String::from_utf8(out.bytes).unwrap();
            assert!(
                text.contains(if form == "ask" { "true" } else { "private" }),
                "{form}: {text}"
            );
        }
        let req = request(
            Verb::Sink,
            &format!("{prefix}update"),
            "update",
            "CLEAR ALL",
        );
        assert!(matches!(
            block_on(kernel.issue(req, &Capability::scoped([CAP_QUERY]))),
            Err(Error::Denied(_))
        ));
        assert_eq!(
            store.len().unwrap(),
            1,
            "read authority cannot clear the store"
        );
    }
}

#[test]
fn federated_queries_do_not_require_shared_store_authority() {
    let kernel = Kernel::new(Arc::new(space()));
    for (form, query) in FORMS {
        block_on(kernel.issue(
            request(Verb::Source, &format!("urn:sparql:{form}"), "query", query),
            &Capability::scoped(["urn:cap:unrelated"]),
        ))
        .unwrap();
    }
}

#[test]
fn multiple_mounts_keep_data_and_catalog_identities_distinct() {
    let left = space_with_store_at("urn:host:left:", store_with_value("left"));
    let right = space_with_store_at("urn:host:right:", store_with_value("right"));
    assert!(left.id().is_none());
    assert!(right.id().is_none());
    let spaces = Fallback::new(vec![Arc::new(left), Arc::new(right)]);
    let entries = spaces.entries().unwrap();
    assert_eq!(entries.len(), 10);
    let ids: HashSet<_> = entries.iter().map(|entry| &entry.endpoint).collect();
    assert_eq!(
        ids.len(),
        10,
        "mounts must not collide in id-keyed catalogs"
    );
    for entry in &entries {
        let req = Request::new(Verb::Meta, Iri::parse(&entry.pattern).unwrap());
        let Resolution::Hit(resolved) = spaces.resolve(&req, &Scope::empty()) else {
            panic!("listed binding does not resolve: {}", entry.pattern);
        };
        let desc = resolved.endpoint.describe();
        assert_eq!(desc.id, entry.endpoint);
        assert!(!desc.inputs.iter().any(|arg| arg.name == "graph"));
        let expected = if entry.pattern.ends_with("update") {
            CAP_UPDATE
        } else {
            CAP_QUERY
        };
        let action = desc
            .action_specs()
            .into_iter()
            .find(|action| action.verb != Verb::Meta)
            .unwrap();
        assert_eq!(action.requires, [expected]);
    }
    let kernel = Kernel::new(Arc::new(spaces));
    for side in ["left", "right"] {
        let req = request(
            Verb::Source,
            &format!("urn:host:{side}:select"),
            "query",
            FORMS[0].1,
        );
        let out = block_on(kernel.issue(req, &Capability::scoped([CAP_QUERY]))).unwrap();
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(text.contains(side), "{side}: {text}");
        assert!(!text.contains(if side == "left" { "right" } else { "left" }));
    }
    for (form, _) in FORMS.into_iter().chain([("update", "")]) {
        let req = Request::new(
            Verb::Meta,
            Iri::parse(format!("urn:sparql:{form}")).unwrap(),
        );
        assert!(matches!(
            block_on(kernel.issue(req, &Capability::root())),
            Err(Error::Unresolved(_))
        ));
    }
    // A mounted write reaches only its own store and needs only write authority.
    block_on(kernel.issue(
        request(Verb::Sink, "urn:host:left:update", "update", "CLEAR ALL"),
        &Capability::scoped([CAP_UPDATE]),
    ))
    .unwrap();
    for (side, expected) in [("left", "false"), ("right", "true")] {
        let out = block_on(kernel.issue(
            request(
                Verb::Source,
                &format!("urn:host:{side}:ask"),
                "query",
                FORMS[1].1,
            ),
            &Capability::scoped([CAP_QUERY]),
        ))
        .unwrap();
        assert!(String::from_utf8(out.bytes).unwrap().contains(expected));
    }
}

#[test]
fn mounted_endpoints_preserve_bounds_and_legacy_identities() {
    let store = store_with_value("bounded");
    let bound = AnswerBound::new(1, 1024).unwrap();
    let mounted = space_with_store_at_and_bounds(
        "urn:host:",
        store.clone(),
        Duration::from_millis(1234),
        bound,
    );
    let entries = mounted.entries().unwrap();
    for entry in &entries {
        let req = Request::new(Verb::Meta, Iri::parse(&entry.pattern).unwrap());
        let Resolution::Hit(resolved) = mounted.resolve(&req, &Scope::empty()) else {
            panic!("unbound endpoint")
        };
        let desc = resolved.endpoint.describe();
        let budget = desc.inputs.iter().find(|arg| arg.name == "budget").unwrap();
        assert!(budget.summary.contains("1234 ms"));
    }
    let kernel = Kernel::new(Arc::new(mounted));
    let req = request(
        Verb::Source,
        "urn:host:select",
        "query",
        "SELECT ?s WHERE { VALUES ?s { 1 2 } }",
    );
    assert!(
        block_on(kernel.issue(req, &Capability::scoped([CAP_QUERY])))
            .unwrap_err()
            .to_string()
            .contains("max_rows")
    );
    let req = request(Verb::Source, "urn:host:select", "query", FORMS[0].1)
        .with_arg("graph", ArgRef::Inline(b"urn:external".to_vec()));
    assert!(
        block_on(kernel.issue(req, &Capability::scoped([CAP_QUERY])))
            .unwrap_err()
            .to_string()
            .contains("graph=")
    );
    let legacy = space_with_store(store.clone()).entries().unwrap();
    let explicit = space_with_store_at("urn:sparql:", store).entries().unwrap();
    for (old, new) in legacy.iter().zip(&explicit) {
        assert_eq!(old.pattern, new.pattern);
        assert_eq!(old.endpoint, new.endpoint);
        assert!(old.endpoint.starts_with("sparql-"));
    }
}

#[test]
fn prefix_punctuation_does_not_collapse_catalog_ids() {
    let store = Arc::new(Store::new().unwrap());
    let mut ids = HashSet::new();
    for prefix in ["urn:host:store:", "urn:host:store/", "urn:host:store#"] {
        for entry in space_with_store_at(prefix, store.clone())
            .entries()
            .unwrap()
        {
            assert!(ids.insert(entry.endpoint), "prefix identities collided");
        }
    }
    assert_eq!(ids.len(), 15);
}
