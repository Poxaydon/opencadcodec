use opencadcodec::entities::{CellValue, EntityType, MText};
use opencadcodec::fields::*;
use opencadcodec::objects::{Dictionary, DictionaryVariable, Field, FieldChildValue, ObjectType};
use opencadcodec::{CadDocument, DwgReader, DwgWriter, Handle};
use std::io::Cursor;
struct Env;
impl FieldContext for Env {
    fn now_julian(&self) -> f64 {
        2460000.0
    }
}
struct Hook {
    raw: String,
    display: String,
}
impl FieldInterceptor for Hook {
    fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
        let FieldEvaluation::Stored(v) = input else {
            panic!("stored context required")
        };
        assert!(!v.definition().handle.is_null());
        assert_eq!(v.definition().handle, v.stored().handle);
        assert_eq!(v.definition().owner, v.container().handle);
        assert!(matches!(v.layout(), LayoutEvidence::Exact { .. }));
        if v.definition().code.contains("Missing") {
            return FieldDecision::HandledPreserve(FieldDiagnostic::preserve("Missing"));
        }
        if !v.definition().evaluator.starts_with("AcSm") {
            return FieldDecision::Delegate;
        }
        FieldDecision::HandledValue(ResolvedText {
            raw: Some(self.raw.clone()),
            display: self.display.clone(),
        })
    }
}
fn answer(value: &str) -> Hook {
    Hook {
        raw: value.into(),
        display: value.into(),
    }
}
fn raw(doc: &CadDocument, h: Handle) -> &Field {
    match &doc.objects[&h] {
        ObjectType::Field(f) => f,
        _ => panic!(),
    }
}
fn raw_mut(doc: &mut CadDocument, h: Handle) -> &mut Field {
    match doc.objects.get_mut(&h).unwrap() {
        ObjectType::Field(f) => f,
        _ => panic!(),
    }
}
fn add_host(
    doc: &mut CadDocument,
    layout: &str,
    codes: &[(&str, &str)],
) -> (Handle, Handle, Vec<Handle>) {
    let host = doc
        .add_entity_to_layout(EntityType::MText(MText::new()), layout)
        .unwrap();
    let template = (0..codes.len())
        .map(|i| format!("%<{}_FldIdx {i}>%", char::from(92)))
        .collect::<Vec<_>>()
        .join(" / ");
    let children = codes
        .iter()
        .map(|(code, value)| NewField::new(*code, *value))
        .collect();
    let container = doc.set_text_field(host, &template, children).unwrap();
    let children = raw(doc, container).child_fields.clone();
    (host, container, children)
}
fn fixture() -> (CadDocument, Handle, Handle, Handle) {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    doc.source_path = Some("drawing.dwg".into());
    let (host, container, kids) = add_host(&mut doc, "A", &[(r"\AcSm Sheet.Title", "old")]);
    (doc, host, container, kids[0])
}
fn plan(doc: &CadDocument, hook: &dyn FieldInterceptor) -> FieldUpdatePlan {
    doc.plan_field_updates(
        &Env,
        hook,
        FieldUpdateSelection {
            trigger: FieldUpdateTrigger::SheetSetChange,
            hosts: None,
        },
    )
    .unwrap()
}
fn run(doc: &mut CadDocument, hook: &dyn FieldInterceptor) -> FieldUpdateReport {
    let p = plan(doc, hook);
    doc.apply_field_updates(p, CacheAccess::Writable)
}
fn text(doc: &CadDocument, h: Handle) -> &str {
    match doc.get_entity(h).unwrap() {
        EntityType::MText(m) => &m.value,
        _ => panic!(),
    }
}
#[test]
fn resolved_update_is_atomic_idempotent_and_preserves_linkage() {
    let (mut doc, host, container, kid) = fixture();
    let defs = doc.fields.clone();
    let before = raw(&doc, kid).clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(report.changed_field_caches, vec![kid]);
    assert_eq!(report.changed_host_texts, vec![host]);
    assert_eq!(report.changed_containers, vec![container]);
    assert!(report.persisted_mutation);
    assert_eq!(text(&doc, host), "new");
    assert_eq!(doc.fields, defs);
    assert_eq!(raw(&doc, kid).handle, before.handle);
    assert_eq!(raw(&doc, kid).owner, before.owner);
    let after = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert!(!report.persisted_mutation);
    assert_eq!(doc, after);
}
#[test]
fn cache_only_changes_are_reported_without_host_changes() {
    let (mut doc, _, _, kid) = fixture();
    let report = run(
        &mut doc,
        &Hook {
            raw: "new raw".into(),
            display: "old".into(),
        },
    );
    assert_eq!(report.changed_field_caches, vec![kid]);
    assert!(report.changed_host_texts.is_empty());
    assert!(report.persisted_mutation);
}
#[test]
fn host_only_repair_does_not_rebuild_caches() {
    let (mut doc, host, _, kid) = fixture();
    let cache = raw(&doc, kid).clone();
    if let Some(EntityType::MText(m)) = doc.get_entity_mut(host) {
        m.value = "stale host".into();
    }
    let report = run(&mut doc, &answer("old"));
    assert!(report.changed_field_caches.is_empty());
    assert_eq!(report.changed_host_texts, vec![host]);
    assert!(report.persisted_mutation);
    assert_eq!(raw(&doc, kid), &cache);
}
#[test]
fn handled_preserve_blocks_entire_mixed_host() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (_, _, _) = add_host(
        &mut doc,
        "A",
        &[
            (r"\AcSm Sheet.Title", "old"),
            (r"\AcSm Sheet.Missing", "saved"),
            (r"\AcExpr (2+3)", "5"),
        ],
    );
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::BlockedByPreservedChild));
}
#[test]
fn literal_sentinels_and_empty_are_valid_handled_values() {
    for (r, d) in [("####", "####"), ("----", "----"), ("", "----")] {
        let (mut doc, host, _, kid) = fixture();
        let report = run(
            &mut doc,
            &Hook {
                raw: r.into(),
                display: d.into(),
            },
        );
        assert!(report.persisted_mutation);
        assert_eq!(raw(&doc, kid).value.text, r);
        assert_eq!(text(&doc, host), d);
    }
}
#[test]
fn malformed_template_rolls_back_all_child_caches() {
    let (mut doc, _, container, _) = fixture();
    let code = format!("%<{}_FldIdx 99>%", char::from(92));
    doc.fields.get_mut(&container).unwrap().code = code.clone();
    raw_mut(&mut doc, container).code = code;
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
    assert!(report
        .failed
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::InvalidTemplate));
}
#[test]
fn read_only_and_stale_plan_make_no_mutation() {
    let (mut doc, _, _, kid) = fixture();
    let before = doc.clone();
    let p = plan(&doc, &answer("new"));
    let report = doc.apply_field_updates(p, CacheAccess::ReadOnly);
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::ReadOnly));
    let p = plan(&doc, &answer("new"));
    raw_mut(&mut doc, kid).filing_option = 91;
    let before = doc.clone();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
    assert!(report
        .failed
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::StalePlan));
}
#[test]
fn metadata_is_not_replaced_or_normalized() {
    let (mut doc, _, _, kid) = fixture();
    let f = raw_mut(&mut doc, kid);
    f.format = "%tc1".into();
    f.value.format = "%tc1".into();
    f.filing_option = 91;
    f.evaluation_error_message = "vendor error".into();
    f.state = 123;
    f.child_values.push(FieldChildValue {
        key: "VendorChild".into(),
        value: CellValue::text("retain"),
    });
    let before = raw(&doc, kid).clone();
    let _ = run(&mut doc, &answer("NEW"));
    let mut expected = before;
    expected.value.text = "NEW".into();
    expected.value.formatted_value = "NEW".into();
    expected.value_string = "NEW".into();
    expected.value_string_length = 3;
    expected.evaluation_status = 2;
    assert_eq!(raw(&doc, kid), &expected);
}
#[test]
fn only_canonical_unknown_cache_can_become_string() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (_, _, kids) = add_host(&mut doc, "A", &[(r"\AcSm Sheet.Title", "####")]);
    let kid = kids[0];
    assert_eq!(raw(&doc, kid).value.raw_type_code, 0);
    let state = raw(&doc, kid).state;
    let error = raw(&doc, kid).evaluation_error_code;
    let report = run(&mut doc, &answer("Resolved"));
    assert!(report.persisted_mutation);
    assert_eq!(raw(&doc, kid).value.raw_type_code, 4);
    assert_eq!(
        (raw(&doc, kid).state, raw(&doc, kid).evaluation_error_code),
        (state, error)
    );
    raw_mut(&mut doc, kid).value.value_type = opencadcodec::entities::table::CellValueType::Unknown;
    raw_mut(&mut doc, kid).value.raw_type_code = 0x7FFF;
    let before = doc.clone();
    let report = run(&mut doc, &answer("Other"));
    assert_eq!(doc, before);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnsupportedCache));
}
#[test]
fn missing_checksum_is_not_created_and_bad_checksum_blocks_host() {
    let (mut doc, _, container, _) = fixture();
    raw_mut(&mut doc, container).child_values.clear();
    let _ = run(&mut doc, &answer("new"));
    assert!(raw(&doc, container).child_values.is_empty());
    for duplicate in [false, true] {
        let (mut doc, _, container, _) = fixture();
        if duplicate {
            let item = raw(&doc, container).child_values[0].clone();
            raw_mut(&mut doc, container).child_values.push(item);
        } else {
            raw_mut(&mut doc, container).child_values[0]
                .value
                .raw_type_code = 999;
        }
        let before = doc.clone();
        let report = run(&mut doc, &answer("new"));
        assert_eq!(doc, before);
        assert!(report
            .preserved
            .iter()
            .any(|d| d.code == FieldDiagnosticCode::UnsupportedChecksum));
    }
}
#[test]
fn independent_hosts_can_succeed_and_preserve_separately() {
    let (mut doc, host, _, _) = fixture();
    let (other, _, kids) = add_host(&mut doc, "A", &[(r"\AcSm Sheet.Missing", "saved")]);
    let cache = raw(&doc, kids[0]).clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(text(&doc, host), "new");
    assert_eq!(text(&doc, other), "saved");
    assert_eq!(raw(&doc, kids[0]), &cache);
    assert_eq!(report.changed_host_texts, vec![host]);
    assert!(report.persisted_mutation);
    assert!(!report.preserved.is_empty());
}
#[test]
fn broken_ownership_never_reaches_verified_callback() {
    let (mut doc, _, _, kid) = fixture();
    raw_mut(&mut doc, kid).owner = Handle::NULL;
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(report
        .failed
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::InvalidIdentity));
}
#[test]
fn event_and_fieldeval_masks_and_updatefield_override() {
    let (mut doc, host, _, kid) = fixture();
    let mut var = DictionaryVariable::new("FIELDEVAL", "0");
    var.handle = Handle::new(0xFF01);
    let mut dictionary = Dictionary::new();
    dictionary.handle = Handle::new(0xFF02);
    dictionary.add_entry("FIELDEVAL", var.handle);
    doc.objects
        .insert(var.handle, ObjectType::DictionaryVariable(var));
    doc.objects
        .insert(dictionary.handle, ObjectType::Dictionary(dictionary));
    for event in [1, 2, 4, 16] {
        let p = doc
            .plan_field_updates(
                &Env,
                &answer("new"),
                FieldUpdateSelection {
                    trigger: FieldUpdateTrigger::Event(event),
                    hosts: None,
                },
            )
            .unwrap();
        let before = doc.clone();
        let report = doc.apply_field_updates(p, CacheAccess::Writable);
        assert_eq!(doc, before);
        assert!(!report.persisted_mutation);
    }
    raw_mut(&mut doc, kid).evaluation_option = 32;
    let p = doc
        .plan_field_updates(
            &Env,
            &answer("new"),
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(32),
                hosts: None,
            },
        )
        .unwrap();
    assert!(
        doc.apply_field_updates(p, CacheAccess::Writable)
            .persisted_mutation
    );
    assert_eq!(text(&doc, host), "new");
}
#[test]
fn mixed_delegate_and_handled_values_keep_existing_evaluators() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, _, _) = add_host(
        &mut doc,
        "A",
        &[(r"\AcSm Sheet.Title", "old"), (r"\AcExpr (2+3)", "0")],
    );
    let p = doc
        .plan_field_updates(
            &Env,
            &answer("new"),
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(32),
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(report.persisted_mutation);
    assert_eq!(text(&doc, host), "new / 5");
}
#[test]
fn legacy_api_still_operates_without_checked_interception() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, _, _) = add_host(&mut doc, "A", &[(r"\AcExpr (2+3)", "0")]);
    assert_eq!(
        evaluate_code(&doc, r"\AcExpr (2+3)", &[], None, &Env),
        Some("5".into())
    );
    assert!(!doc.update_fields(&Env, 32, None).0.is_empty());
    assert_eq!(text(&doc, host), "5");
}
#[test]
fn checked_cache_survives_explicit_in_memory_dwg_roundtrip() {
    let (mut doc, _, _, _) = fixture();
    assert!(run(&mut doc, &answer("Native title")).persisted_mutation);
    let identities: Vec<_> = doc
        .fields
        .values()
        .filter(|f| f.evaluator.starts_with("AcSm"))
        .map(|f| {
            (
                f.handle,
                f.owner,
                f.evaluator.clone(),
                f.code.clone(),
                f.objects.clone(),
            )
        })
        .collect();
    let bytes = DwgWriter::write_to_vec(&doc).unwrap();
    let mut reader = DwgReader::from_stream(Cursor::new(bytes));
    let reopened = reader.read().unwrap();
    for (handle, owner, evaluator, code, objects) in identities {
        let f = reopened
            .fields
            .get(&handle)
            .expect("Field handle preserved");
        assert_eq!(
            (f.owner, &f.evaluator, &f.code, &f.objects),
            (owner, &evaluator, &code, &objects)
        );
    }
    assert!(reopened.objects.values().any(|o|matches!(o,ObjectType::Field(f) if f.evaluator_id.starts_with("AcSm") && f.value_string=="Native title")));
}

#[test]
fn nonoverlapping_combined_event_mask_does_not_evaluate() {
    let (mut doc, _, _, kid) = fixture();
    let mut var = DictionaryVariable::new("FIELDEVAL", "1");
    var.handle = Handle::new(0xFF01);
    let mut dictionary = Dictionary::new();
    dictionary.handle = Handle::new(0xFF02);
    dictionary.add_entry("fieldeval", var.handle);
    doc.objects
        .insert(var.handle, ObjectType::DictionaryVariable(var));
    doc.objects
        .insert(dictionary.handle, ObjectType::Dictionary(dictionary));
    raw_mut(&mut doc, kid).evaluation_option = 2;
    let before = doc.clone();
    let p = doc
        .plan_field_updates(
            &Env,
            &answer("new"),
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(3),
                hosts: None,
            },
        )
        .unwrap();
    assert!(
        !doc.apply_field_updates(p, CacheAccess::Writable)
            .persisted_mutation
    );
    assert_eq!(doc, before);
}
#[test]
fn event_options_host_filter_and_two_layouts_are_respected() {
    let (mut doc, a, _, kid) = fixture();
    doc.add_layout("B").unwrap();
    let (b, _, _) = add_host(&mut doc, "B", &[(r"\AcSm Sheet.Title", "other")]);
    struct LayoutName;
    impl FieldInterceptor for LayoutName {
        fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
            let FieldEvaluation::Stored(v) = input else {
                unreachable!()
            };
            let LayoutEvidence::Exact { name, .. } = v.layout() else {
                panic!("exact layout required")
            };
            assert_eq!(v.trigger(), FieldUpdateTrigger::Event(16));
            FieldDecision::HandledValue(ResolvedText {
                raw: Some(name.into()),
                display: name.into(),
            })
        }
    }
    raw_mut(&mut doc, kid).evaluation_option = 1;
    let p = doc
        .plan_field_updates(
            &Env,
            &LayoutName,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(16),
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(text(&doc, a), "old");
    assert_eq!(text(&doc, b), "B");
    assert_eq!(report.changed_host_texts, vec![b]);
    let p = doc
        .plan_field_updates(
            &Env,
            &answer("filtered"),
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::SheetSetChange,
                hosts: Some(&[a]),
            },
        )
        .unwrap();
    let _ = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(text(&doc, a), "filtered");
    assert_eq!(text(&doc, b), "B");
}
#[test]
fn delegated_plotdate_uses_plotting_environment() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, _, kids) = add_host(&mut doc, "A", &[(r"\AcVar PlotDate", "----")]);
    struct Delegate;
    impl FieldInterceptor for Delegate {}
    // Its baseline cache is text; a typed Date conversion is deliberately not coerced.
    let before = doc.clone();
    let p = doc
        .plan_field_updates(
            &Env,
            &Delegate,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(4),
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(doc, before);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnsupportedCache));
    assert_eq!(text(&doc, host), "----");
    assert_eq!(raw(&doc, kids[0]).code, r"\AcVar PlotDate");
}
#[test]
fn vendor_string_cache_flags_and_payload_are_never_discarded() {
    let (mut doc, _, _, kid) = fixture();
    raw_mut(&mut doc, kid).value.flags = 0x7FFF;
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnsupportedCache));
}
#[test]
fn xdata_children_format_and_unicode_checksum_survive_update() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, container, kids) = add_host(
        &mut doc,
        "A",
        &[(
            r#"\AcSm Sheet.Title \href "https://example.invalid/##1""#,
            "old",
        )],
    );
    let kid = kids[0];
    let f = raw_mut(&mut doc, kid);
    assert!(!f.xdata.records().is_empty());
    f.child_values.push(FieldChildValue {
        key: "Vendor".into(),
        value: CellValue::text("keep"),
    });
    f.value.format = "%tc1".into();
    let before = raw(&doc, kid).clone();
    let _ = run(&mut doc, &answer("?? ?N ??"));
    let after = raw(&doc, kid);
    assert_eq!(after.xdata, before.xdata);
    assert_eq!(after.child_values, before.child_values);
    assert_eq!(after.value.format, before.value.format);
    assert_eq!(
        after.value_string_length,
        "?? ?N ??".encode_utf16().count() as i32
    );
    let checksum: f64 = text(&doc, host)
        .encode_utf16()
        .enumerate()
        .map(|(i, u)| (i as f64 + 1.0) * u as f64)
        .sum();
    assert_eq!(
        raw(&doc, container)
            .child_values
            .iter()
            .find(|v| v.key == "ACFD_FIELDTEXT_CHECKSUM")
            .unwrap()
            .value
            .numeric_value,
        checksum
    );
}
#[test]
fn real_dwg_fixture_is_read_only_while_in_memory_fields_are_updated() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dynamic-blocks/xy.dwg");
    let bytes = std::fs::read(&path).unwrap();
    let mut reader = DwgReader::from_stream(Cursor::new(bytes.clone()));
    let mut doc = reader.read().unwrap();
    doc.add_layout("CheckedApiTest").unwrap();
    let (host, _, _) = add_host(&mut doc, "CheckedApiTest", &[(r"\AcSm Sheet.Title", "old")]);
    let report = run(&mut doc, &answer("Fixture memory only"));
    assert!(report.persisted_mutation);
    assert_eq!(text(&doc, host), "Fixture memory only");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn established_date_cache_keeps_legacy_plot_semantics_and_format() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, _, kids) = add_host(&mut doc, "A", &[(r"\AcVar PlotDate", "----")]);
    let kid = kids[0];
    doc.stamp_plot_fields(&Env);
    let format = raw(&doc, kid).value.format.clone();
    struct Later;
    impl FieldContext for Later {
        fn now_julian(&self) -> f64 {
            2460001.0
        }
    }
    struct Delegate;
    impl FieldInterceptor for Delegate {}
    let expected = evaluate_code(&doc, r"\AcVar PlotDate", &[], Some(host), &Later);
    // The pure ordinary context preserves PlotDate outside plotting; the checked event supplies plotting=true.
    assert!(expected.is_some());
    let p = doc
        .plan_field_updates(
            &Later,
            &Delegate,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(4),
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert!(report.persisted_mutation);
    assert_eq!(raw(&doc, kid).value.format, format);
    assert_eq!(raw(&doc, kid).value.raw_type_code, 8);
    assert_eq!(raw(&doc, kid).value.binary_value.len(), 16);
}
#[test]
fn unselected_or_unavailable_evaluators_do_not_repair_host_text() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (host, _, _) = add_host(&mut doc, "A", &[(r"\Unknown value", "saved")]);
    if let Some(EntityType::MText(m)) = doc.get_entity_mut(host) {
        m.value = "retain host".into();
    }
    struct Delegate;
    impl FieldInterceptor for Delegate {}
    let before = doc.clone();
    let p = doc
        .plan_field_updates(
            &Env,
            &Delegate,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::Event(32),
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(doc, before);
    assert!(!report.persisted_mutation);
}

#[test]
fn unknown_stored_acsm_versions_preserve_without_legacy_callback() {
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let _ = add_host(&mut doc, "A", &[(r"\AcSm.99 Sheet.Title", "saved")]);
    struct Delegate;
    impl FieldInterceptor for Delegate {}
    struct Trap;
    impl FieldContext for Trap {
        fn now_julian(&self) -> f64 {
            0.0
        }
        fn sheet_sets(
            &self,
            _: &mut dyn FnMut(&opencadcodec::sheet_set::SheetSetDatabase) -> Option<String>,
        ) -> Option<String> {
            panic!("legacy callback reached")
        }
    }
    let before = doc.clone();
    let p = doc
        .plan_field_updates(
            &Trap,
            &Delegate,
            FieldUpdateSelection {
                trigger: FieldUpdateTrigger::SheetSetChange,
                hosts: None,
            },
        )
        .unwrap();
    let report = doc.apply_field_updates(p, CacheAccess::Writable);
    assert_eq!(doc, before);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnhandledAcSm));
}
#[test]
fn owner_cycles_and_noncanonical_unknown_payload_never_mutate() {
    let (mut doc, _, container, _) = fixture();
    doc.fields.get_mut(&container).unwrap().owner = container;
    raw_mut(&mut doc, container).owner = container;
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(!report.failed.is_empty());
    let mut doc = CadDocument::default();
    doc.add_layout("A").unwrap();
    let (_, _, kids) = add_host(&mut doc, "A", &[(r"\AcSm Sheet.Title", "####")]);
    raw_mut(&mut doc, kids[0]).value.binary_value.push(91);
    let before = doc.clone();
    let report = run(&mut doc, &answer("new"));
    assert_eq!(doc, before);
    assert!(report
        .preserved
        .iter()
        .any(|d| d.code == FieldDiagnosticCode::UnsupportedCache));
}
