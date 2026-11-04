//! Adversarial capability non-disclosure court.
//!
//! Two stores expose identical *authorized* views to a non-root principal but
//! hold deliberately different *unauthorized* contents (2 vs 7 hidden files,
//! plus hidden relation edges). Resolution results, work accounting, budget
//! failures, and error text must be identical, so nothing about the hidden
//! population can be inferred from the resolver's behaviour.

use std::collections::BTreeMap;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::context::{Budget, ContextRoot};
use weftcodec::selector;
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::context::ContextResolution;
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{Options, Store};

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 16,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
    }
}

fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::Str((*value).to_owned())))
        .collect()
}

fn tag_value(name: &str) -> RelationTarget {
    RelationTarget::Value(Value::Str(name.to_owned()))
}

/// Deterministic fixture: identical visible objects (same seeds => same
/// ObjectIds/VersionIds in both stores) with a variable hidden population.
struct Fixture {
    root: Hash32,
    a: Hash32,
    b: Hash32,
    hidden: Vec<Hash32>,
    hidden_names: Vec<String>,
    snapshot: Hash32,
}

fn build(dir: &Path, hidden_count: usize, tag: &str) -> (Store, Fixture) {
    let mut store = Store::init(dir, opts()).unwrap();
    let root = store.resolve_path("/").unwrap();

    // Visible, byte-identical and seed-identical content in both stores.
    let a = store
        .create_file_seeded(
            root,
            b"a.txt",
            b"alpha-shared-payload",
            0o644,
            1000,
            1000,
            attrs(&[("project", "demo")]),
            b"nondisclosure-visible-a",
        )
        .unwrap();
    let b = store
        .create_file_seeded(
            root,
            b"b.txt",
            b"beta-shared-payload",
            0o644,
            1000,
            1000,
            attrs(&[("project", "demo")]),
            b"nondisclosure-visible-b",
        )
        .unwrap();

    // Visible relation edges plus visible value-target tag edges.
    store
        .relation_add(
            root,
            "derived_from",
            RelationTarget::Object(a),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .relation_add(
            a,
            "derived_from",
            RelationTarget::Object(b),
            None,
            BTreeMap::new(),
        )
        .unwrap();
    for object in [a, b] {
        store
            .relation_add(
                object,
                "tagged_as",
                tag_value("demo"),
                None,
                BTreeMap::new(),
            )
            .unwrap();
    }

    // Unauthorized population: mode 0600, uid/gid 0, unreadable for 1000.
    let mut hidden = Vec::new();
    let mut hidden_names = Vec::new();
    for i in 0..hidden_count {
        let name = format!("hidden_{tag}_{i}");
        let content = format!("unauthorized-{tag}-{i}");
        let seed = format!("nondisclosure-hidden-{tag}-{i}");
        let id = store
            .create_file_seeded(
                root,
                name.as_bytes(),
                content.as_bytes(),
                0o600,
                0,
                0,
                attrs(&[("project", "secret")]),
                seed.as_bytes(),
            )
            .unwrap();
        // Hidden subject -> visible target: must not be counted or traversed.
        store
            .relation_add(
                id,
                "derived_from",
                RelationTarget::Object(a),
                None,
                BTreeMap::new(),
            )
            .unwrap();
        // Hidden subject -> visible value target: still not a visible edge.
        store
            .relation_add(id, "tagged_as", tag_value("demo"), None, BTreeMap::new())
            .unwrap();
        hidden.push(id);
        hidden_names.push(name);
    }
    // Visible subject -> hidden target: invisible because the target is not
    // readable, and must not make the selector fail or disclose the target.
    store
        .relation_add(
            a,
            "derived_from",
            RelationTarget::Object(hidden[0]),
            None,
            BTreeMap::new(),
        )
        .unwrap();

    let snapshot = store.snapshot_create("s1").unwrap();
    let fixture = Fixture {
        root,
        a,
        b,
        hidden,
        hidden_names,
        snapshot,
    };
    (store, fixture)
}

fn save_context(
    store: &mut Store,
    name: &str,
    selector_text: &str,
    snapshot: Hash32,
    budget: Option<Budget>,
) {
    let selector = selector::parse(selector_text).unwrap();
    let root = ContextRoot::new(
        &selector,
        snapshot,
        weftcodec::context::DEFAULT_RESOLVER,
        Vec::new(),
        None,
        budget,
    )
    .unwrap();
    store.save_context_root(name, &root, None, false).unwrap();
}

fn resolve(store: &Store, name: &str, principal: &Principal) -> weftfs::Result<ContextResolution> {
    store.resolve_context(
        name,
        None,
        principal,
        &weftcodec::Limits::default(),
        store.clock_now_ns(),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct View {
    matched_count: u64,
    refs: Vec<(Hash32, Hash32)>,
    work_units: u64,
    truncated: bool,
}

fn view(resolved: &ContextResolution) -> View {
    View {
        matched_count: resolved.resolution.matched_count,
        refs: resolved
            .resolution
            .results
            .iter()
            .map(|item| (item.object_id, item.content_id))
            .collect(),
        work_units: resolved.work_units,
        truncated: resolved.truncated,
    }
}

fn budget(max_work: u64) -> Option<Budget> {
    Some(Budget {
        max_work: Some(max_work),
        max_results: None,
        max_bytes: None,
        max_depth: None,
    })
}

fn selectors(fixture: &Fixture) -> [(&'static str, String); 3] {
    [
        ("plain", "select attrs.project == \"demo\"".to_owned()),
        ("tagged", "select tag(\"demo\")".to_owned()),
        (
            "relation",
            format!("select relation(derived_from, obj:{})", fixture.a.to_hex()),
        ),
    ]
}

#[test]
fn authorized_views_are_identical_despite_different_hidden_content() {
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let (mut store_a, fixture_a) = build(dir_a.path(), 2, "a");
    let (mut store_b, fixture_b) = build(dir_b.path(), 7, "b");
    let principal = Principal::new(1000, 1000, Vec::new());

    // The stores really do differ in their unauthorized population.
    assert!(
        store_b.current_state().objects.len() > store_a.current_state().objects.len(),
        "store B must hold more objects than store A"
    );
    assert!(
        store_b.current_state().relations.len() > store_a.current_state().relations.len(),
        "store B must hold more relations than store A"
    );

    let visible: Vec<Hash32> = vec![fixture_a.root, fixture_a.a, fixture_a.b];
    let hidden_b_contents: Vec<Hash32> = {
        let state = store_b.state_at(fixture_b.snapshot).unwrap();
        fixture_b
            .hidden
            .iter()
            .map(|id| state.objects.get(id).unwrap().content)
            .collect()
    };

    let mut work = BTreeMap::new();
    for (name, selector_text) in selectors(&fixture_a) {
        save_context(&mut store_a, name, &selector_text, fixture_a.snapshot, None);
        save_context(&mut store_b, name, &selector_text, fixture_b.snapshot, None);

        let resolved_a = resolve(&store_a, name, &principal).unwrap();
        let resolved_b = resolve(&store_b, name, &principal).unwrap();
        let view_a = view(&resolved_a);
        let view_b = view(&resolved_b);

        assert_eq!(view_a, view_b, "selector `{name}` diverged across stores");
        assert!(!view_a.truncated, "selector `{name}` must not truncate");
        work.insert(name, view_a.work_units);

        for (object_id, content_id) in &view_a.refs {
            assert!(
                visible.contains(object_id),
                "selector `{name}` returned a non-visible object"
            );
            assert!(
                !fixture_b.hidden.contains(object_id),
                "selector `{name}` leaked a hidden ObjectId"
            );
            assert!(
                !hidden_b_contents.contains(content_id),
                "selector `{name}` leaked a hidden ContentId"
            );
        }
    }

    let plain = resolve(&store_a, "plain", &principal).unwrap();
    let tagged = resolve(&store_a, "tagged", &principal).unwrap();
    let relation = resolve(&store_a, "relation", &principal).unwrap();
    assert_eq!(plain.resolution.matched_count, 2);
    assert_eq!(tagged.resolution.matched_count, 2);
    assert_eq!(relation.resolution.matched_count, 1);
    assert_eq!(relation.resolution.results[0].object_id, fixture_a.root);

    // Visible relation edges are counted, and the count is identical across
    // stores even though B holds strictly more objects and relations.
    assert!(
        work["relation"] > work["plain"],
        "relation traversal must add visible work units"
    );
    assert_eq!(
        work["relation"],
        resolve(&store_b, "relation", &principal)
            .unwrap()
            .work_units
    );
    assert_eq!(
        work["tagged"],
        resolve(&store_b, "tagged", &principal).unwrap().work_units
    );
}

#[test]
fn work_budget_failures_do_not_disclose_unauthorized_state() {
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let (mut store_a, fixture_a) = build(dir_a.path(), 2, "a");
    let (mut store_b, fixture_b) = build(dir_b.path(), 7, "b");
    let principal = Principal::new(1000, 1000, Vec::new());
    assert_ne!(fixture_a.hidden.len(), fixture_b.hidden.len());

    for (name, selector_text) in selectors(&fixture_a) {
        // Establish the shared authorized work count from an unbudgeted run.
        save_context(
            &mut store_a,
            &format!("{name}_base"),
            &selector_text,
            fixture_a.snapshot,
            None,
        );
        save_context(
            &mut store_b,
            &format!("{name}_base"),
            &selector_text,
            fixture_b.snapshot,
            None,
        );
        let base_a = resolve(&store_a, &format!("{name}_base"), &principal).unwrap();
        let base_b = resolve(&store_b, &format!("{name}_base"), &principal).unwrap();
        assert_eq!(base_a.work_units, base_b.work_units);
        let work = base_a.work_units;
        assert!(work > 0);

        // Below the authorized work count: both fail identically.
        for (suffix, max_work) in [("below", work - 1), ("at", work), ("above", work + 1)] {
            let label = format!("{name}_{suffix}");
            save_context(
                &mut store_a,
                &label,
                &selector_text,
                fixture_a.snapshot,
                budget(max_work),
            );
            save_context(
                &mut store_b,
                &label,
                &selector_text,
                fixture_b.snapshot,
                budget(max_work),
            );
            let result_a = resolve(&store_a, &label, &principal);
            let result_b = resolve(&store_b, &label, &principal);
            match suffix {
                "below" => {
                    let error_a = result_a.unwrap_err();
                    let error_b = result_b.unwrap_err();
                    assert_eq!(error_a.class(), Class::LimitExceeded);
                    assert_eq!(error_b.class(), Class::LimitExceeded);
                    assert_eq!(
                        error_a.message(),
                        "resolver work budget exceeded",
                        "non-disclosing message required"
                    );
                    assert_eq!(error_a.message(), error_b.message());
                    assert_eq!(error_a.class(), error_b.class());
                    assert!(!error_a.message().contains("hidden"));
                    assert!(!error_b.message().contains("hidden"));
                    for hidden_name in &fixture_b.hidden_names {
                        assert!(!error_b.message().contains(hidden_name.as_str()));
                    }
                }
                "at" | "above" => {
                    // N == work and N == work + 1 must succeed in both stores,
                    // and both Ns are below B's naive total population, so a
                    // leaky accounting implementation would fail in B.
                    let resolved_a = result_a.expect("budget at/above authorized work must pass");
                    let resolved_b = result_b.expect("budget at/above authorized work must pass");
                    assert_eq!(resolved_a.work_units, work);
                    assert_eq!(view(&resolved_a), view(&resolved_b));
                    assert!(!resolved_a.truncated);
                    assert!(!resolved_b.truncated);
                }
                _ => unreachable!(),
            }
        }
    }
}
