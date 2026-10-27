use opencadcodec::entities::{AttributeDefinition, AttributeEntity, EntityType, Insert, MText};
use opencadcodec::fields::*;
use opencadcodec::objects::{Dictionary, DictionaryVariable, Field, FieldChildValue, ObjectType};
use opencadcodec::sheet_set::SheetSetDatabase;
use opencadcodec::tables::BlockRecord;
use opencadcodec::types::Vector3;
use opencadcodec::{CadDocument, Handle};
use std::cell::Cell;

struct Poison;
impl FieldContext for Poison {
    fn now_julian(&self) -> f64 {
        2460000.0
    }
    fn sheet_sets(&self, _: &mut dyn FnMut(&SheetSetDatabase) -> Option<String>) -> Option<String> {
        panic!("legacy AcSm / DST access is forbidden");
    }
}
struct Hook<'a> {
    value: &'a str,
    calls: Cell<usize>,
}
impl<'a> Hook<'a> {
    fn new(value: &'a str) -> Self {
        Self {
            value,
            calls: Cell::new(0),
        }
    }
}
impl FieldInterceptor for Hook<'_> {
    fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
        self.calls.set(self.calls.get() + 1);
        let (code, layout) = match input {
            FieldEvaluation::Stored(v) => {
                assert_eq!(v.definition().handle, v.stored().handle);
                assert_eq!(v.stored().owner, v.container().handle);
                assert_eq!(v.source_path(), Some("drawing.dwg"));
                (v.definition().code.as_str(), v.layout())
            }
            FieldEvaluation::Preview(v) => (v.code(), v.layout()),
        };
        if !matches!(layout, LayoutEvidence::Exact { .. }) {
            // ATTDEF source Fields stay in their block definition; never invent a Layout.
            return FieldDecision::HandledPreserve(FieldDiagnostic::preserve("Unsupported Layout"));
        }
        assert!(matches!(layout, LayoutEvidence::Exact { name: "A", .. }));
        if !code.trim_start_matches('\\').starts_with("AcSm") {
            return FieldDecision::Delegate;
        }
        if code.contains("Missing") {
            return FieldDecision::HandledPreserve(FieldDiagnostic::preserve("Missing"));
        }
        FieldDecision::HandledValue(ResolvedText {
            raw: Some(self.value.into()),
            display: self.value.into(),
        })
    }
}
#[derive(Clone)]
struct Target {
    parent: Handle,
    host: Handle,
    container: Handle,
    children: Vec<Handle>,
}
fn raw(doc: &CadDocument, h: Handle) -> &Field {
    let ObjectType::Field(f) = &doc.objects[&h] else {
        panic!()
    };
    f
}
fn raw_mut(doc: &mut CadDocument, h: Handle) -> &mut Field {
    let ObjectType::Field(f) = doc.objects.get_mut(&h).unwrap() else {
        panic!()
    };
    f
}
fn attr_mut<'a>(doc: &'a mut CadDocument, t: &Target) -> &'a mut AttributeEntity {
    let EntityType::Insert(i) = doc.get_entity_mut(t.parent).unwrap() else {
        panic!()
    };
    i.attributes
        .iter_mut()
        .find(|a| a.common.handle == t.host)
        .unwrap()
}
fn text<'a>(doc: &'a CadDocument, t: &Target) -> &'a str {
    let EntityType::Insert(i) = doc.get_entity(t.parent).unwrap() else {
        panic!()
    };
    &i.attributes
        .iter()
        .find(|a| a.common.handle == t.host)
        .unwrap()
        .value
}
fn document() -> CadDocument {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    doc.add_layout("B").unwrap();
    doc.source_path = Some("drawing.dwg".into());
    let mut block = BlockRecord::new("Label");
    block.handle = doc.allocate_handle();
    block.block_entity_handle = doc.allocate_handle();
    block.block_end_handle = doc.allocate_handle();
    doc.block_records.add(block).unwrap();
    doc
}
fn insert(doc: &mut CadDocument, codes: &[&[(&str, &str)]]) -> Vec<Target> {
    insert_in_layout(doc, "A", codes)
}
fn insert_in_layout(doc: &mut CadDocument, layout: &str, codes: &[&[(&str, &str)]]) -> Vec<Target> {
    let mut entity = Insert::new("Label", Vector3::new(12.0, 34.0, 56.0));
    let hosts: Vec<_> = codes
        .iter()
        .enumerate()
        .map(|(n, _)| {
            let mut attr = AttributeEntity::simple(format!("TAG{n}"), "old");
            attr.common.handle = doc.allocate_handle();
            let h = attr.common.handle;
            entity.attributes.push(attr);
            h
        })
        .collect();
    let parent = doc
        .add_entity_to_layout(EntityType::Insert(entity), layout)
        .unwrap();
    if let Some(EntityType::Insert(i)) = doc.get_entity_mut(parent) {
        for a in &mut i.attributes {
            a.common.owner_handle = parent;
        }
    }
    codes
        .iter()
        .zip(hosts)
        .map(|(children, host)| {
            let template = (0..children.len())
                .map(|i| format!(r"%<\_FldIdx {i}>%"))
                .collect::<Vec<_>>()
                .join(" / ");
            let container = doc
                .set_text_field(
                    host,
                    &template,
                    children
                        .iter()
                        .map(|(c, v)| NewField::new(*c, *v))
                        .collect(),
                )
                .unwrap();
            Target {
                parent,
                host,
                container,
                children: raw(doc, container).child_fields.clone(),
            }
        })
        .collect()
}
fn fixture() -> (CadDocument, Target) {
    let mut doc = document();
    let target = insert(&mut doc, &[&[(r"\AcSm Sheet.Title", "old")]]).remove(0);
    (doc, target)
}
fn plan(doc: &CadDocument, hook: &dyn FieldInterceptor, event: i32) -> FieldUpdatePlan {
    doc.plan_field_updates(
        &Poison,
        hook,
        FieldUpdateSelection {
            trigger: FieldUpdateTrigger::Event(event),
            hosts: None,
        },
    )
    .unwrap()
}
fn update(doc: &mut CadDocument, hook: &dyn FieldInterceptor) -> FieldUpdateReport {
    let p = plan(doc, hook, 16);
    doc.apply_field_updates(p, CacheAccess::Writable)
}
fn linkage(doc: &CadDocument, t: &Target) {
    assert!(
        doc.get_entity(t.host).is_none(),
        "must remain nested, not synthetic top-level entity"
    );
    let EntityType::Insert(i) = doc.get_entity(t.parent).unwrap() else {
        panic!()
    };
    let a = i
        .attributes
        .iter()
        .find(|a| a.common.handle == t.host)
        .unwrap();
    assert_eq!(a.common.owner_handle, t.parent);
    let xh = a.common.xdictionary_handle.unwrap();
    let ObjectType::Dictionary(x) = &doc.objects[&xh] else {
        panic!()
    };
    assert_eq!(x.owner, t.host);
    let dh = x.get("ACAD_FIELD").unwrap();
    let ObjectType::Dictionary(d) = &doc.objects[&dh] else {
        panic!()
    };
    assert_eq!(d.owner, xh);
    assert_eq!(d.get("TEXT"), Some(t.container));
    assert_eq!(raw(doc, t.container).owner, dh);
    assert_eq!(raw(doc, t.container).child_fields, t.children);
    for h in &t.children {
        assert_eq!(raw(doc, *h).owner, t.container);
    }
}

#[test]
fn nested_strict_acsm_updates_cache_and_host_without_legacy_and_is_idempotent() {
    let (mut doc, t) = fixture();
    let before = doc.clone();
    let hook = Hook::new("####");
    let report = update(&mut doc, &hook);
    assert!(report.persisted_mutation, "{report:?}");
    assert_eq!(hook.calls.get(), 1);
    assert_eq!(report.changed_host_texts, vec![t.host]);
    assert_eq!(report.changed_field_caches, t.children);
    assert_eq!(text(&doc, &t), "####");
    assert_eq!(raw(&doc, t.children[0]).value.display(), "####");
    assert_eq!(raw(&doc, t.children[0]).value_string, "####");
    assert_eq!(raw(&doc, t.children[0]).value_string_length, 4);
    assert_eq!(doc.fields, before.fields);
    let mut expected = raw(&before, t.children[0]).clone();
    expected.value = raw(&doc, t.children[0]).value.clone();
    expected.value_string = raw(&doc, t.children[0]).value_string.clone();
    expected.value_string_length = 4;
    assert_eq!(raw(&doc, t.children[0]), &expected);
    linkage(&doc, &t);
    let stable = doc.clone();
    assert!(!update(&mut doc, &hook).persisted_mutation);
    assert_eq!(doc, stable);
}
#[test]
fn nested_multiple_attributes_and_inserts_have_distinct_verified_hosts() {
    let mut doc = document();
    let mut targets = insert(
        &mut doc,
        &[
            &[(r"\AcSm Sheet.Title", "old")],
            &[(r"\AcSm Sheet.Number", "old")],
        ],
    );
    targets.extend(insert(&mut doc, &[&[(r"\AcSm Sheet.Title", "old")]]));
    let hook = Hook::new("new");
    let report = update(&mut doc, &hook);
    assert_eq!(hook.calls.get(), 3);
    assert_eq!(report.changed_host_texts.len(), 3);
    for t in targets {
        assert_eq!(text(&doc, &t), "new");
        linkage(&doc, &t);
    }
}
#[test]
fn nested_non_acsm_acexpr_and_acobjprop_match_legacy() {
    let mut doc = document();
    let targets = insert(
        &mut doc,
        &[
            &[(r"\AcExpr (2+3)", "old")],
            &[(
                r"\AcObjProp.16.2 Object(?BlockRefId,1).InsertionPoint",
                "old",
            )],
        ],
    );
    let mut legacy = doc.clone();
    legacy.update_fields(&Poison, 16, None); // non-AcSm oracle only
    let report = update(&mut doc, &Hook::new("unused"));
    assert!(report.persisted_mutation);
    for t in &targets {
        assert_eq!(text(&doc, t), text(&legacy, t));
        assert_ne!(text(&doc, t), "old");
        assert_eq!(
            raw(&doc, t.children[0]).value.display(),
            raw(&legacy, t.children[0]).value.display()
        );
        linkage(&doc, t);
    }
    assert_eq!(text(&doc, &targets[0]), "5");
}
#[test]
fn nested_mixed_fields_and_top_level_hosts_keep_interception_semantics() {
    let mut doc = document();
    let targets = insert(
        &mut doc,
        &[
            &[(r"\AcSm Sheet.Title", "old"), (r"\AcExpr (2+3)", "old")],
            &[(r"\AcSm Missing", "old"), (r"\AcExpr (3+4)", "old")],
        ],
    );
    let top = doc
        .add_entity_to_layout(EntityType::MText(MText::new()), "A")
        .unwrap();
    doc.set_text_field(
        top,
        r"%<\_FldIdx 0>%",
        vec![NewField::new(r"\AcExpr (1+1)", "old")],
    )
    .unwrap();
    let preserved = raw(&doc, targets[1].children[1]).clone();
    let report = update(&mut doc, &Hook::new("----"));
    assert!(report.persisted_mutation);
    assert!(!report.preserved.is_empty());
    assert_eq!(text(&doc, &targets[0]), "---- / 5");
    assert_eq!(text(&doc, &targets[1]), "old / old");
    assert_eq!(raw(&doc, targets[1].children[1]), &preserved);
    let EntityType::MText(m) = doc.get_entity(top).unwrap() else {
        panic!()
    };
    assert_eq!(m.value, "2");
}
#[test]
fn nested_duplicate_handles_within_or_across_inserts_are_rejected() {
    for across in [false, true] {
        let mut doc = document();
        let first = insert(&mut doc, &[&[(r"\AcSm Sheet.Title", "old")]]).remove(0);
        if across {
            let second = insert(&mut doc, &[&[(r"\AcSm Sheet.Title", "old")]]).remove(0);
            attr_mut(&mut doc, &second).common.handle = first.host;
        } else {
            let EntityType::Insert(i) = doc.get_entity_mut(first.parent).unwrap() else {
                panic!()
            };
            i.attributes.push(i.attributes[0].clone());
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let report = update(&mut doc, &hook);
        assert!(!report.persisted_mutation);
        assert_eq!(hook.calls.get(), 0);
        assert!(!report.failed.is_empty());
        assert_eq!(doc, before);
    }
}
#[test]
fn nested_missing_parent_inconsistent_owner_and_null_identity_are_rejected() {
    for case in 0..4 {
        let (mut doc, t) = fixture();
        match case {
            0 => {
                doc.remove_entity(t.parent);
            }
            1 => attr_mut(&mut doc, &t).common.owner_handle = Handle::new(0xBAD),
            2 => attr_mut(&mut doc, &t).common.handle = Handle::NULL,
            _ => doc.get_entity_mut(t.parent).unwrap().common_mut().handle = Handle::new(0xBAD),
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let report = update(&mut doc, &hook);
        assert!(!report.persisted_mutation);
        assert_eq!(hook.calls.get(), 0);
        assert!(!report.failed.is_empty());
        assert_eq!(doc, before);
    }
}
#[test]
fn nested_collision_with_top_level_or_object_handle_is_rejected() {
    for object in [false, true] {
        let (mut doc, t) = fixture();
        if object {
            let mut d = Dictionary::new();
            d.handle = t.host;
            doc.objects.insert(t.host, ObjectType::Dictionary(d));
        } else {
            let mut m = MText::new();
            m.common.handle = t.host;
            doc.add_entity_to_layout(EntityType::MText(m), "A").unwrap();
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let report = update(&mut doc, &hook);
        assert!(!report.persisted_mutation);
        assert_eq!(hook.calls.get(), 0);
        assert_eq!(doc, before);
    }
}
#[test]
fn nested_missing_invalid_or_ambiguous_layout_is_not_guessed() {
    for case in 0..4 {
        let (mut doc, t) = fixture();
        let owner = doc.get_entity(t.parent).unwrap().common().owner_handle;
        let layout = doc
            .objects
            .values()
            .find_map(|o| match o {
                ObjectType::Layout(l) if l.block_record == owner => Some(l.handle),
                _ => None,
            })
            .unwrap();
        match case {
            0 => {
                doc.objects.remove(&layout);
            }
            1 => {
                doc.get_entity_mut(t.parent)
                    .unwrap()
                    .common_mut()
                    .owner_handle = Handle::new(0xBAD)
            }
            2 => {
                let ObjectType::Layout(l) = doc.objects.get_mut(&layout).unwrap() else {
                    panic!()
                };
                l.handle = Handle::new(0xBAD);
            }
            _ => {
                let mut l = doc.objects[&layout].clone();
                let h = doc.allocate_handle();
                let ObjectType::Layout(v) = &mut l else {
                    panic!()
                };
                v.handle = h;
                doc.objects.insert(h, l);
            }
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let report = update(&mut doc, &hook);
        assert!(!report.persisted_mutation);
        assert_eq!(hook.calls.get(), 0);
        assert_eq!(doc, before);
    }
}
#[test]
fn nested_broken_dictionary_linkage_never_reaches_callback() {
    for case in 0..3 {
        let (mut doc, t) = fixture();
        let xh = attr_mut(&mut doc, &t).common.xdictionary_handle.unwrap();
        let ObjectType::Dictionary(x) = &doc.objects[&xh] else {
            panic!()
        };
        let dh = x.get("ACAD_FIELD").unwrap();
        match case {
            0 => attr_mut(&mut doc, &t).common.xdictionary_handle = None,
            1 => {
                let ObjectType::Dictionary(d) = doc.objects.get_mut(&dh).unwrap() else {
                    panic!()
                };
                d.owner = t.parent;
            }
            _ => {
                let ObjectType::Dictionary(x) = doc.objects.get_mut(&xh).unwrap() else {
                    panic!()
                };
                x.entries.push(("ACAD_FIELD".into(), dh));
            }
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let report = update(&mut doc, &hook);
        assert!(!report.persisted_mutation);
        assert_eq!(hook.calls.get(), 0);
        assert_eq!(doc, before);
    }
}
#[test]
fn nested_invalid_host_does_not_block_valid_host_in_same_batch() {
    let mut doc = document();
    let targets = insert(
        &mut doc,
        &[
            &[(r"\AcSm Sheet.Title", "old")],
            &[(r"\AcSm Sheet.Title", "old")],
        ],
    );
    attr_mut(&mut doc, &targets[0]).common.owner_handle = Handle::new(0xBAD);
    let report = update(&mut doc, &Hook::new("new"));
    assert!(report.persisted_mutation);
    assert!(!report.failed.is_empty());
    assert_eq!(text(&doc, &targets[0]), "old");
    assert_eq!(text(&doc, &targets[1]), "new");
}
#[test]
fn nested_fieldeval_per_field_and_host_selection_are_respected() {
    let (mut doc, t) = fixture();
    let mut var = DictionaryVariable::new("FIELDEVAL", "0");
    var.handle = doc.allocate_handle();
    let vh = var.handle;
    let mut d = Dictionary::new();
    d.handle = doc.allocate_handle();
    d.add_entry("FIELDEVAL", vh);
    doc.objects.insert(d.handle, ObjectType::Dictionary(d));
    doc.objects.insert(vh, ObjectType::DictionaryVariable(var));
    let hook = Hook::new("new");
    let before = doc.clone();
    let p = plan(&doc, &hook, 16);
    let r = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(!r.persisted_mutation);
    assert_eq!(hook.calls.get(), 0);
    assert_eq!(doc, before);
    raw_mut(&mut doc, t.children[0]).evaluation_option = 2;
    let p = plan(&doc, &hook, 32);
    assert!(
        !doc.apply_field_updates(p, CacheAccess::Writable)
            .persisted_mutation
    );
    raw_mut(&mut doc, t.children[0]).evaluation_option = 32;
    let p = doc
        .plan_field_updates(
            &Poison,
            &hook,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(32),
                hosts: Some(&[]),
            },
        )
        .unwrap();
    assert!(
        !doc.apply_field_updates(p, CacheAccess::Writable)
            .persisted_mutation
    );
    let p = plan(&doc, &hook, 32);
    assert!(
        doc.apply_field_updates(p, CacheAccess::Writable)
            .persisted_mutation
    );
    assert_eq!(text(&doc, &t), "new");
}
#[test]
fn nested_cache_only_host_only_and_checksum_only_are_persisted() {
    for case in 0..3 {
        let (mut doc, t) = fixture();
        update(&mut doc, &Hook::new("new"));
        match case {
            0 => {
                let f = raw_mut(&mut doc, t.children[0]);
                f.value = opencadcodec::entities::CellValue::text("old");
                f.value_string = "old".into();
            }
            1 => attr_mut(&mut doc, &t).value = "wrong host".into(),
            _ => {
                raw_mut(&mut doc, t.container)
                    .child_values
                    .iter_mut()
                    .find(|v| v.key == "ACFD_FIELDTEXT_CHECKSUM")
                    .unwrap()
                    .value
                    .numeric_value = -1.0
            }
        }
        let report = update(&mut doc, &Hook::new("new"));
        assert!(report.persisted_mutation, "{report:?}");
        assert_eq!(report.changed_field_caches.is_empty(), case != 0);
        assert_eq!(report.changed_host_texts.is_empty(), case != 1);
        assert_eq!(report.changed_containers.is_empty(), case != 2);
        linkage(&doc, &t);
    }
}
#[test]
fn nested_readonly_preserves_entire_document() {
    let (mut doc, _) = fixture();
    let before = doc.clone();
    let p = plan(&doc, &Hook::new("new"), 16);
    let r = doc.apply_field_updates(p, CacheAccess::ReadOnly);
    assert!(!r.persisted_mutation);
    assert!(r
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::ReadOnly));
    assert_eq!(doc, before);
}
#[test]
fn nested_stale_plan_detects_attribute_parent_linkage_cache_layout_and_source_changes() {
    for case in 0..8 {
        let (mut doc, t) = fixture();
        let p = plan(&doc, &Hook::new("new"), 16);
        match case {
            0 => attr_mut(&mut doc, &t).common.handle = Handle::new(0xBAD),
            1 => attr_mut(&mut doc, &t).value = "newer text".into(),
            2 => {
                let mut i = doc.get_entity(t.parent).unwrap().clone();
                let EntityType::Insert(v) = &mut i else {
                    panic!()
                };
                v.block_name = "new parent".into();
                doc.replace_entity_arc(t.parent, std::sync::Arc::new(i));
            }
            3 => raw_mut(&mut doc, t.children[0]).owner = t.parent,
            4 => raw_mut(&mut doc, t.children[0]).value_string = "newer cache".into(),
            5 => {
                let b = doc
                    .objects
                    .values()
                    .find_map(|o| match o {
                        ObjectType::Layout(l) if l.name == "B" => Some(l.block_record),
                        _ => None,
                    })
                    .unwrap();
                doc.get_entity_mut(t.parent)
                    .unwrap()
                    .common_mut()
                    .owner_handle = b;
            }
            6 => doc.source_path = Some("other.dwg".into()),
            _ => {
                let other = doc.allocate_handle();
                attr_mut(&mut doc, &t).common.owner_handle = other;
            }
        }
        let changed = doc.clone();
        let report = doc.apply_field_updates(p, CacheAccess::Writable);
        assert!(!report.persisted_mutation);
        assert!(report
            .failed
            .iter()
            .any(|d| d.code == FieldDiagnosticCode::StalePlan));
        assert_eq!(doc, changed);
    }
}
#[test]
fn nested_vendor_metadata_and_field_identity_survive_cache_update() {
    let (mut doc, t) = fixture();
    let f = raw_mut(&mut doc, t.children[0]);
    f.filing_option = 77;
    f.evaluation_error_message = "vendor".into();
    f.child_values.push(FieldChildValue {
        key: "Vendor".into(),
        value: opencadcodec::entities::CellValue::text("opaque"),
    });
    let before = raw(&doc, t.children[0]).clone();
    let defs = doc.fields.clone();
    let r = update(&mut doc, &Hook::new("new"));
    assert!(r.persisted_mutation);
    let mut expected = before;
    expected.value = raw(&doc, t.children[0]).value.clone();
    expected.value_string = raw(&doc, t.children[0]).value_string.clone();
    assert_eq!(raw(&doc, t.children[0]), &expected);
    assert_eq!(doc.fields, defs);
    linkage(&doc, &t);
}
#[test]
fn nested_attachment_mapping_survives_repeated_checked_updates() {
    let mut doc = document();
    let owner = doc.block_records.get("Label").unwrap().handle;
    let def = doc
        .add_entity(EntityType::AttributeDefinition(AttributeDefinition::new(
            "TAG".into(),
            "Prompt".into(),
            "old".into(),
        )))
        .unwrap();
    doc.get_entity_mut(def).unwrap().common_mut().owner_handle = owner;
    doc.block_records
        .get_mut("Label")
        .unwrap()
        .entity_handles
        .push(def);
    let source_container = doc
        .set_text_field(
            def,
            r"%<\_FldIdx 0>%",
            vec![NewField::new(
                r#"\AcSm Sheet.Title \href "https://example.invalid/#1""#,
                "old",
            )],
        )
        .unwrap();
    let source = raw(&doc, source_container).child_fields[0];
    let before = raw(&doc, source).clone();
    assert!(!before.xdata.records().is_empty());
    let mut i = Insert::new("Label", Vector3::ZERO);
    i.attributes.push(AttributeEntity::simple("TAG", "old"));
    let parent = doc
        .add_entity_to_layout(EntityType::Insert(i), "A")
        .unwrap();
    let report = doc.attach_attribute_fields_with_policy(parent, &Poison, &|_| {
        AttributeFieldEvaluation::Evaluate
    });
    assert_eq!(report.fields.len(), 1);
    let mapping = &report.fields[0];
    assert!(mapping.deferred);
    assert_eq!(mapping.source, source);
    assert_ne!(mapping.target, source);
    let t = Target {
        parent,
        host: mapping.host,
        container: raw(&doc, mapping.target).owner,
        children: vec![mapping.target],
    };
    let xdata = raw(&doc, mapping.target).xdata.clone();
    let r = update(&mut doc, &Hook::new("new"));
    assert!(r.persisted_mutation, "{r:?}");
    assert_eq!(raw(&doc, source), &before);
    assert_eq!(raw(&doc, mapping.target).xdata, xdata);
    linkage(&doc, &t);
    let stable = doc.clone();
    assert!(!update(&mut doc, &Hook::new("new")).persisted_mutation);
    assert_eq!(doc, stable);
    let report =
        doc.attach_attribute_fields_with_policy(parent, &Poison, &|_| panic!("existing target"));
    assert!(report.fields.is_empty());
    assert_eq!(doc, stable);
}
#[test]
fn nested_preview_is_pure_and_exact_layout_is_exposed() {
    let (doc, t) = fixture();
    let before = doc.clone();
    let hook = Hook::new("new");
    let r = evaluate_code_checked(
        &doc,
        PreviewRequest {
            code: r"\AcSm Sheet.Title",
            objects: &[],
            host: Some(t.host),
            layout: None,
        },
        &Poison,
        &hook,
    );
    assert!(matches!(r,CheckedEvaluation::Value(ResolvedText{display,..}) if display=="new"));
    assert_eq!(doc, before);
}
#[test]
fn nested_acsm_delegate_is_preserved_without_legacy_fallback() {
    struct Delegate;
    impl FieldInterceptor for Delegate {
        fn intercept(&self, _: FieldEvaluation<'_>) -> FieldDecision {
            FieldDecision::Delegate
        }
    }
    let (mut doc, _) = fixture();
    let before = doc.clone();
    let r = update(&mut doc, &Delegate);
    assert!(!r.persisted_mutation);
    assert!(r
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnhandledAcSm));
    assert_eq!(doc, before);
}

#[test]
fn nested_unsupported_cache_preserves_whole_host_and_metadata() {
    let (mut doc, t) = fixture();
    let f = raw_mut(&mut doc, t.children[0]);
    f.value.binary_value = vec![1, 2, 3];
    f.value.data_size = 3;
    f.value.flags = 173;
    let before = doc.clone();
    let r = update(&mut doc, &Hook::new("new"));
    assert!(!r.persisted_mutation);
    assert!(!r.preserved.is_empty());
    assert_eq!(doc, before);
    linkage(&doc, &t);
}

fn dictionaries(doc: &CadDocument, t: &Target) -> (Handle, Handle) {
    let EntityType::Insert(i) = doc.get_entity(t.parent).unwrap() else {
        panic!()
    };
    let a = i
        .attributes
        .iter()
        .find(|a| a.common.handle == t.host)
        .unwrap();
    let x = a.common.xdictionary_handle.unwrap();
    let ObjectType::Dictionary(d) = &doc.objects[&x] else {
        panic!()
    };
    (x, d.get("ACAD_FIELD").unwrap())
}
fn dictionary_mut(doc: &mut CadDocument, h: Handle) -> &mut Dictionary {
    let ObjectType::Dictionary(d) = doc.objects.get_mut(&h).unwrap() else {
        panic!()
    };
    d
}
#[test]
fn remediation_unique_case_insensitive_keys_preserve_dictionary_bytes_and_order() {
    for acad in ["ACAD_FIELD", "acad_field", "AcAd_FiElD"] {
        for text_key in ["TEXT", "text", "TeXt"] {
            let (mut doc, t) = fixture();
            let (x, d) = dictionaries(&doc, &t);
            dictionary_mut(&mut doc, x).entries[0].0 = acad.into();
            dictionary_mut(&mut doc, d).entries[0].0 = text_key.into();
            // Opaque siblings and entry ordering must stay untouched.
            dictionary_mut(&mut doc, x)
                .entries
                .insert(0, ("VendorBefore".into(), Handle::new(0xBAD)));
            dictionary_mut(&mut doc, d)
                .entries
                .push(("VendorAfter".into(), Handle::new(0xBAE)));
            let before = doc.clone();
            let r = update(&mut doc, &Hook::new("new"));
            assert!(r.persisted_mutation, "{acad}/{text_key}: {r:?}");
            assert_eq!(text(&doc, &t), "new");
            assert_eq!(doc.objects[&x], before.objects[&x]);
            assert_eq!(doc.objects[&d], before.objects[&d]);
            assert_eq!(doc.fields, before.fields);
            linkage(&doc, &t);
        }
    }
}
fn duplicate_key_cases(acad: bool) {
    let variants: &[(&str, &str)] = if acad {
        &[
            ("ACAD_FIELD", "acad_field"),
            ("ACAD_FIELD", "AcAd_FiElD"),
            ("acad_field", "aCaD_fIeLd"),
        ]
    } else {
        &[("TEXT", "text"), ("TEXT", "TeXt"), ("text", "tExT")]
    };
    for (first, second) in variants {
        for reverse in [false, true] {
            for dangling in [false, true] {
                let (mut doc, t) = fixture();
                let (x, d) = dictionaries(&doc, &t);
                let dictionary = if acad { x } else { d };
                let good = if acad { d } else { t.container };
                let other = if dangling { Handle::new(0xBAD) } else { good };
                let mut entries = vec![(first.to_string(), good), (second.to_string(), other)];
                if reverse {
                    entries.reverse();
                }
                dictionary_mut(&mut doc, dictionary).entries = entries;
                let before = doc.clone();
                let hook = Hook::new("new");
                let r = update(&mut doc, &hook);
                assert!(
                    !r.persisted_mutation,
                    "{first}/{second}, reverse={reverse}, dangling={dangling}: {r:?}"
                );
                assert_eq!(hook.calls.get(), 0);
                assert_eq!(doc, before);
                assert!(r
                    .failed
                    .iter()
                    .any(|v| v.code == FieldDiagnosticCode::InvalidIdentity
                        && v.field == Some(t.container)));
            }
        }
    }
}
#[test]
fn remediation_duplicate_acad_field_variants_reject_both_orders_and_dangling() {
    duplicate_key_cases(true);
}
#[test]
fn remediation_duplicate_text_variants_reject_both_orders_and_dangling() {
    duplicate_key_cases(false);
}

struct MisleadingCtab;
impl FieldContext for MisleadingCtab {
    fn now_julian(&self) -> f64 {
        2460000.0
    }
    fn getvar(&self, name: &str) -> Option<String> {
        name.eq_ignore_ascii_case("ctab").then(|| "A".into())
    }
    fn sheet_sets(&self, _: &mut dyn FnMut(&SheetSetDatabase) -> Option<String>) -> Option<String> {
        panic!("legacy AcSm / DST fallback")
    }
}
struct ExpectedLayout {
    host: Handle,
    parent: Handle,
    layout: Handle,
    block: Handle,
    name: String,
}
fn fixture_layout(doc: &CadDocument, t: &Target, name: &str) -> ExpectedLayout {
    // Independent fixture lookup by requested name, before invoking checked resolution.
    let matches: Vec<_> = doc
        .objects
        .iter()
        .filter_map(|(h, o)| match o {
            ObjectType::Layout(l) if l.name == name => {
                assert_eq!(*h, l.handle);
                Some((*h, l.block_record))
            }
            _ => None,
        })
        .collect();
    assert_eq!(matches.len(), 1);
    ExpectedLayout {
        host: t.host,
        parent: t.parent,
        layout: matches[0].0,
        block: matches[0].1,
        name: name.into(),
    }
}
struct LayoutHook {
    expected: Vec<ExpectedLayout>,
    calls: Cell<usize>,
}
impl FieldInterceptor for LayoutHook {
    fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
        let FieldEvaluation::Stored(v) = input else {
            panic!("stored Field required")
        };
        let e = self
            .expected
            .iter()
            .find(|e| e.host == v.host())
            .expect("unexpected host");
        assert_eq!(
            v.layout(),
            LayoutEvidence::Exact {
                handle: e.layout,
                name: &e.name,
                block_record: e.block
            }
        );
        let EntityType::Insert(parent) = v.document().get_entity(e.parent).unwrap() else {
            panic!()
        };
        assert_eq!(parent.common.owner_handle, e.block);
        assert_eq!(
            parent
                .attributes
                .iter()
                .filter(|a| a.common.handle == e.host && a.common.owner_handle == e.parent)
                .count(),
            1
        );
        assert_eq!(v.stored().owner, v.container().handle);
        self.calls.set(self.calls.get() + 1);
        FieldDecision::HandledValue(ResolvedText {
            raw: Some(e.name.clone()),
            display: e.name.clone(),
        })
    }
}
fn layout_plan(doc: &CadDocument, hook: &LayoutHook) -> FieldUpdatePlan {
    doc.plan_field_updates(
        &MisleadingCtab,
        hook,
        FieldUpdateSelection {
            trigger: FieldUpdateTrigger::Event(16),
            hosts: None,
        },
    )
    .unwrap()
}
#[test]
fn remediation_model_paper_and_multiple_layouts_use_exact_parent_identities() {
    let mut doc = document();
    let mut targets = Vec::new();
    let mut expected = Vec::new();
    for name in ["Model", "A", "B"] {
        let t = insert_in_layout(&mut doc, name, &[&[(r"\AcSm Sheet.Title", "old")]]).remove(0);
        expected.push(fixture_layout(&doc, &t, name));
        targets.push(t);
    }
    assert_ne!(expected[0].block, expected[1].block);
    assert_ne!(expected[1].block, expected[2].block);
    let b_layout = expected[2].layout;
    let hook = LayoutHook {
        expected,
        calls: Cell::new(0),
    };
    let p = layout_plan(&doc, &hook);
    let r = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(r.persisted_mutation);
    assert_eq!(hook.calls.get(), 3);
    for (t, name) in targets.iter().zip(["Model", "A", "B"]) {
        assert_eq!(text(&doc, t), name);
        linkage(&doc, t);
    }
    // CTAB still names A and Model still exists; neither may mask a missing B Layout.
    doc.objects.remove(&b_layout);
    let before = doc.clone();
    hook.calls.set(0);
    let p = layout_plan(&doc, &hook);
    let r = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(!r.persisted_mutation);
    assert_eq!(hook.calls.get(), 2);
    assert_eq!(doc, before);
    assert!(r
        .failed
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::InvalidHost && d.host == Some(targets[2].host)));
}
#[test]
fn remediation_valid_reparent_rejects_old_plan_and_fresh_plan_resolves_new_insert() {
    let mut doc = document();
    let mut t = insert(&mut doc, &[&[(r"\AcSm Sheet.Title", "old")]]).remove(0);
    let old_parent = t.parent;
    let old_hook = LayoutHook {
        expected: vec![fixture_layout(&doc, &t, "A")],
        calls: Cell::new(0),
    };
    let old_plan = layout_plan(&doc, &old_hook);
    // Real destination INSERT is registered in B, with no duplicate ATTRIB handles.
    let parent_b = doc
        .add_entity_to_layout(
            EntityType::Insert(Insert::new("Label", Vector3::new(7.0, 8.0, 9.0))),
            "B",
        )
        .unwrap();
    let EntityType::Insert(a) = doc.get_entity_mut(old_parent).unwrap() else {
        panic!()
    };
    let index = a
        .attributes
        .iter()
        .position(|a| a.common.handle == t.host)
        .unwrap();
    let mut moved = a.attributes.remove(index);
    moved.common.owner_handle = parent_b;
    let EntityType::Insert(b) = doc.get_entity_mut(parent_b).unwrap() else {
        panic!()
    };
    b.attributes.push(moved);
    t.parent = parent_b;
    linkage(&doc, &t);
    let EntityType::Insert(a) = doc.get_entity(old_parent).unwrap() else {
        panic!()
    };
    assert!(a.attributes.is_empty());
    let reparented = doc.clone();
    let r = doc.apply_field_updates(old_plan, CacheAccess::Writable);
    assert!(!r.persisted_mutation);
    assert!(r
        .failed
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::StalePlan));
    assert_eq!(doc, reparented);
    let fresh_hook = LayoutHook {
        expected: vec![fixture_layout(&doc, &t, "B")],
        calls: Cell::new(0),
    };
    let p = layout_plan(&doc, &fresh_hook);
    let r = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(r.persisted_mutation);
    assert_eq!(fresh_hook.calls.get(), 1);
    assert_eq!(text(&doc, &t), "B");
    assert_eq!(doc.fields, reparented.fields);
    linkage(&doc, &t);
}
#[test]
fn remediation_preplanning_linkage_failures_are_deterministic_and_mutation_free() {
    for case in 0..7 {
        let (mut doc, t) = fixture();
        let (x, d) = dictionaries(&doc, &t);
        match case {
            0 => {
                doc.objects.remove(&t.container);
            }
            1 => dictionary_mut(&mut doc, x).entries[0].1 = Handle::new(0xBAD),
            2 => raw_mut(&mut doc, t.children[0]).owner = t.parent,
            3 => dictionary_mut(&mut doc, d).entries.clear(),
            4 => dictionary_mut(&mut doc, d).entries[0].1 = Handle::new(0xBAD),
            5 => {
                raw_mut(&mut doc, t.children[0]).owner = t.parent;
                doc.fields.get_mut(&t.children[0]).unwrap().owner = t.parent;
            }
            _ => dictionary_mut(&mut doc, x).entries.clear(),
        }
        let before = doc.clone();
        let hook = Hook::new("new");
        let r = update(&mut doc, &hook);
        assert!(!r.persisted_mutation, "case {case}: {r:?}");
        assert_eq!(hook.calls.get(), 0);
        assert_eq!(doc, before);
        let code = if case == 0 {
            FieldDiagnosticCode::InvalidHost
        } else {
            FieldDiagnosticCode::InvalidIdentity
        };
        let field = if case == 2 {
            t.children[0]
        } else {
            t.container
        };
        assert!(
            r.failed
                .iter()
                .any(|d| d.code == code && d.field == Some(field)),
            "case {case}: {r:?}"
        );
        let again = update(&mut doc, &hook);
        assert_eq!(r.failed, again.failed);
        assert_eq!(doc, before);
        assert!(r.changed_field_caches.is_empty());
        assert!(r.changed_host_texts.is_empty());
    }
}
#[test]
fn remediation_malformed_nested_acsm_delegate_and_preserve_never_reach_legacy() {
    struct Conservative {
        preserve: bool,
        calls: Cell<usize>,
    }
    impl FieldInterceptor for Conservative {
        fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
            let FieldEvaluation::Stored(v) = input else {
                panic!()
            };
            assert!(matches!(
                v.layout(),
                LayoutEvidence::Exact { name: "A", .. }
            ));
            self.calls.set(self.calls.get() + 1);
            if self.preserve {
                FieldDecision::HandledPreserve(FieldDiagnostic::preserve("Malformed AcSm retained"))
            } else {
                FieldDecision::Delegate
            }
        }
    }
    for code in [
        r"\AcSm",
        r"\AcSm.99 broken(",
        r"\aCsM Broken",
        r"%<\AcSm.16.2 broken>%",
    ] {
        for preserve in [false, true] {
            let mut doc = document();
            let t = insert(&mut doc, &[&[(code, "old")]]).remove(0);
            let before = doc.clone();
            let hook = Conservative {
                preserve,
                calls: Cell::new(0),
            };
            let r = update(&mut doc, &hook);
            assert_eq!(hook.calls.get(), 1, "{code}: {r:?}");
            assert!(!r.persisted_mutation);
            assert_eq!(doc, before);
            linkage(&doc, &t);
            let expected = if preserve {
                FieldDiagnosticCode::AdapterPreserve
            } else {
                FieldDiagnosticCode::UnhandledAcSm
            };
            assert!(r.preserved.iter().any(|d| d.code == expected));
        }
    }
}
#[test]
fn remediation_non_acsm_full_raw_cache_and_metadata_match_safe_legacy_oracle() {
    let mut doc = document();
    let targets = insert(
        &mut doc,
        &[
            &[(r"\AcExpr (2+3)", "old")],
            &[(
                r"\AcObjProp.16.2 Object(?BlockRefId,1).InsertionPoint",
                "old",
            )],
        ],
    );
    for t in &targets {
        let f = raw_mut(&mut doc, t.children[0]);
        f.filing_option = 77;
        f.evaluation_error_message = "vendor diagnostic".into();
        f.child_values.push(FieldChildValue {
            key: "Vendor".into(),
            value: opencadcodec::entities::CellValue::text("opaque"),
        });
    }
    let before = doc.clone();
    let mut legacy = doc.clone();
    legacy.update_fields(&Poison, 16, None);
    let r = update(&mut doc, &Hook::new("unused"));
    assert!(r.persisted_mutation);
    for t in &targets {
        let actual = raw(&doc, t.children[0]);
        let oracle = raw(&legacy, t.children[0]);
        assert_eq!(
            actual, oracle,
            "full Field/cache parity without handle or metadata normalization"
        );
        let mut expected = raw(&before, t.children[0]).clone();
        expected.value = oracle.value.clone();
        expected.value_string = oracle.value_string.clone();
        expected.value_string_length = oracle.value_string_length;
        expected.evaluation_status = 2;
        assert_eq!(actual, &expected);
        assert_eq!(text(&doc, t), text(&legacy, t));
        linkage(&doc, t);
    }
    assert_eq!(doc.fields, before.fields);
}
