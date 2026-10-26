use opencadcodec::entities::{AttributeDefinition, AttributeEntity, CellValue, EntityType, Insert};
use opencadcodec::fields::*;
use opencadcodec::objects::{Field, FieldChildValue, ObjectType};
use opencadcodec::sheet_set::SheetSetDatabase;
use opencadcodec::tables::BlockRecord;
use opencadcodec::types::Vector3;
use opencadcodec::{CadDocument, DwgReader, DwgWriter, Handle};
use std::io::Cursor;

struct Poison;
impl FieldContext for Poison {
    fn now_julian(&self) -> f64 {
        2460000.0
    }
    fn sheet_sets(&self, _: &mut dyn FnMut(&SheetSetDatabase) -> Option<String>) -> Option<String> {
        panic!("legacy AcSm evaluation / DST fallback must not be reached")
    }
}
struct Fixture {
    doc: CadDocument,
    insert: Handle,
    source: Handle,
    container: Handle,
}
fn raw(doc: &CadDocument, handle: Handle) -> &Field {
    match &doc.objects[&handle] {
        ObjectType::Field(f) => f,
        _ => panic!(),
    }
}
fn raw_mut(doc: &mut CadDocument, handle: Handle) -> &mut Field {
    match doc.objects.get_mut(&handle).unwrap() {
        ObjectType::Field(f) => f,
        _ => panic!(),
    }
}
fn fixture(code: &str) -> Fixture {
    let mut doc = CadDocument::default();
    let mut block = BlockRecord::new("Label");
    block.handle = doc.allocate_handle();
    block.block_entity_handle = doc.allocate_handle();
    block.block_end_handle = doc.allocate_handle();
    let mut def = AttributeDefinition::new("TAG".into(), "Prompt".into(), "cached".into());
    def.common.owner_handle = block.handle;
    let definition = doc
        .add_entity(EntityType::AttributeDefinition(def))
        .unwrap();
    doc.get_entity_mut(definition)
        .unwrap()
        .common_mut()
        .owner_handle = block.handle;
    block.entity_handles.push(definition);
    doc.block_records.add(block).unwrap();
    let container = doc
        .set_text_field(
            definition,
            "[%<\\_FldIdx 0>%]",
            vec![NewField::new(code, "cached")],
        )
        .unwrap();
    let source = raw(&doc, container).child_fields[0];
    let mut insert = Insert::new("Label", Vector3::ZERO);
    insert
        .attributes
        .push(AttributeEntity::simple("TAG", "untouched"));
    let insert = doc.add_entity(EntityType::Insert(insert)).unwrap();
    Fixture {
        doc,
        insert,
        source,
        container,
    }
}
fn target_text(doc: &CadDocument, insert: Handle) -> &str {
    match doc.get_entity(insert).unwrap() {
        EntityType::Insert(i) => &i.attributes[0].value,
        _ => panic!(),
    }
}
fn attach(f: &mut Fixture) -> AttributeFieldAttachmentReport {
    f.doc
        .attach_attribute_fields_with_policy(f.insert, &Poison, &|_| {
            AttributeFieldEvaluation::Evaluate
        })
}

#[test]
fn deferred_acsm_never_calls_legacy_even_if_evaluate_requested() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    let source = raw(&f.doc, f.source).clone();
    let report = attach(&mut f);
    assert_eq!(report.attached_hosts.len(), 1, "{report:?}");
    assert_eq!(report.fields.len(), 1);
    assert!(report.fields[0].deferred);
    assert_eq!(raw(&f.doc, f.source), &source);
    assert_eq!(target_text(&f.doc, f.insert), "[cached]");
}
#[test]
fn deferred_expression_cache_and_unknown_metadata_are_exact_copies() {
    let mut f = fixture(r#"\AcSm.16.2 Sheet.Title \f "%tc1" \href "https://example.invalid""#);
    let source = raw_mut(&mut f.doc, f.source);
    source.value.flags = 173;
    source.value.binary_value = vec![1, 2, 3];
    source.value.data_size = 3;
    source.value.format = "vendor format".into();
    source.value_string = "literal ####".into();
    source.value_string_length = 12;
    source.filing_option = 77;
    source.evaluation_status = 32;
    source.evaluation_error_code = 22;
    source.evaluation_error_message = "keep vendor diagnostic".into();
    source.child_values.push(FieldChildValue {
        key: "Vendor".into(),
        value: CellValue::text("opaque"),
    });
    let before = source.clone();
    let report = attach(&mut f);
    assert_eq!(report.fields.len(), 1, "{report:?}");
    let copy = &report.fields[0];
    assert_ne!(copy.source, copy.target);
    let mut expected = before.clone();
    expected.handle = copy.target;
    expected.owner = raw(&f.doc, copy.target).owner;
    assert_eq!(raw(&f.doc, copy.target), &expected);
    assert_eq!(raw(&f.doc, f.source), &before);
    assert_eq!(f.doc.fields[&copy.target].code, before.code);
    assert_eq!(f.doc.fields[&copy.target].owner, expected.owner);
}
#[test]
fn malformed_and_unknown_acsm_family_is_conservatively_deferred() {
    for code in [
        r"\AcSm",
        r"\AcSm.99 nonsense(",
        r"\aCsM Broken",
        r"\AcSm Sheet.Title ?BlockRefId",
    ] {
        let mut f = fixture(code);
        let before = raw(&f.doc, f.source).clone();
        let report = attach(&mut f);
        assert_eq!(report.fields.len(), 1, "{code}: {report:?}");
        assert!(report.fields[0].deferred);
        assert_eq!(raw(&f.doc, report.fields[0].target).code, code);
        assert_eq!(raw(&f.doc, f.source), &before);
    }
}
#[test]
fn non_acsm_evaluation_matches_legacy_attachment() {
    let mut f = fixture(r"\AcExpr (2+3)");
    let mut legacy = f.doc.clone();
    let legacy_hosts = legacy.attach_attribute_fields(f.insert, &Poison);
    let report = attach(&mut f);
    assert_eq!(report.attached_hosts, legacy_hosts);
    assert!(!report.fields[0].deferred);
    assert_eq!(
        target_text(&f.doc, f.insert),
        target_text(&legacy, f.insert)
    );
    assert_eq!(target_text(&f.doc, f.insert), "[5]");
    assert_eq!(
        raw(&f.doc, report.fields[0].target),
        raw(&legacy, report.fields[0].target)
    );
}
#[test]
fn caller_can_defer_non_acsm_without_cache_normalization() {
    let mut f = fixture(r"\AcExpr (2+3)");
    let source = raw(&f.doc, f.source).clone();
    let report = f
        .doc
        .attach_attribute_fields_with_policy(f.insert, &Poison, &|_| {
            AttributeFieldEvaluation::Defer
        });
    assert_eq!(target_text(&f.doc, f.insert), "[cached]");
    let mut expected = source;
    expected.handle = report.fields[0].target;
    expected.owner = raw(&f.doc, expected.handle).owner;
    assert_eq!(raw(&f.doc, expected.handle), &expected);
}
#[test]
fn target_linkage_and_field_list_have_unique_new_handles() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    let report = attach(&mut f);
    let host = report.attached_hosts[0];
    let target = raw(&f.doc, report.fields[0].target);
    let container = raw(&f.doc, target.owner);
    assert_eq!(container.child_fields, vec![target.handle]);
    assert_eq!(f.doc.fields[&target.handle].owner, container.handle);
    let insert = match f.doc.get_entity(f.insert).unwrap() {
        EntityType::Insert(i) => i,
        _ => panic!(),
    };
    assert_eq!(insert.attributes[0].common.handle, host);
    assert_eq!(insert.attributes[0].common.owner_handle, f.insert);
    let xdict = insert.attributes[0].common.xdictionary_handle.unwrap();
    let dict = match &f.doc.objects[&xdict] {
        ObjectType::Dictionary(d) => d,
        _ => panic!(),
    };
    let field_dict = dict.get("ACAD_FIELD").unwrap();
    assert_eq!(container.owner, field_dict);
    for object in f.doc.objects.values() {
        if let ObjectType::FieldList(list) = object {
            assert_eq!(
                list.fields.iter().filter(|h| **h == target.handle).count(),
                1
            );
            assert_eq!(
                list.fields
                    .iter()
                    .filter(|h| **h == container.handle)
                    .count(),
                1
            );
        }
    }
}
#[test]
fn existing_target_identity_is_never_replaced() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    attach(&mut f);
    let before = f.doc.clone();
    let report = attach(&mut f);
    assert!(report.attached_hosts.is_empty());
    assert!(!report.preserved.is_empty());
    assert_eq!(f.doc, before);
}
#[test]
fn malformed_template_preserves_whole_attribute_without_allocations() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    f.doc.fields.get_mut(&f.container).unwrap().code = "%<\\_FldIdx 7>%".into();
    raw_mut(&mut f.doc, f.container).code = "%<\\_FldIdx 7>%".into();
    let before = f.doc.clone();
    let report = attach(&mut f);
    assert!(report.attached_hosts.is_empty());
    assert!(!report.preserved.is_empty());
    assert_eq!(f.doc, before);
}
#[test]
fn source_identity_mismatch_preserves_without_evaluation() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    raw_mut(&mut f.doc, f.source).code = "different".into();
    let before = f.doc.clone();
    let report = attach(&mut f);
    assert!(report.attached_hosts.is_empty());
    assert_eq!(f.doc, before);
}
#[test]
fn mixed_children_only_evaluate_non_acsm() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    let definition = match f.doc.get_entity(f.insert).unwrap() {
        EntityType::Insert(i) => {
            f.doc
                .entities_in_block(&i.block_name)
                .next()
                .unwrap()
                .common()
                .handle
        }
        _ => panic!(),
    };
    f.doc
        .set_text_field(
            definition,
            "%<\\_FldIdx 0>% / %<\\_FldIdx 1>%",
            vec![
                NewField::new(r"\AcSm Sheet.Title", "native cache"),
                NewField::new(r"\AcExpr (2+3)", "old"),
            ],
        )
        .unwrap();
    let report = attach(&mut f);
    assert_eq!(report.fields.len(), 2, "{report:?}");
    assert!(report.fields[0].deferred);
    assert!(!report.fields[1].deferred);
    assert_eq!(target_text(&f.doc, f.insert), "native cache / 5");
}
#[test]
fn deferred_cache_and_identity_survive_dwg_roundtrip() {
    let mut f = fixture(r"\AcSm Sheet.Title");
    let report = attach(&mut f);
    let copy = &report.fields[0];
    let before = raw(&f.doc, copy.target).clone();
    let bytes = DwgWriter::write_to_vec(&f.doc).unwrap();
    let reopened = DwgReader::from_stream(Cursor::new(bytes)).read().unwrap();
    let after = raw(&reopened, copy.target);
    assert_eq!(after.handle, before.handle);
    assert_eq!(after.owner, before.owner);
    assert_eq!(after.code, before.code);
    assert_eq!(after.evaluator_id, before.evaluator_id);
    assert_eq!(after.value, before.value);
    assert_eq!(target_text(&reopened, f.insert), "[cached]");
}
#[test]
fn poison_dst_is_not_opened_or_imported() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            assert!(self.0.is_absolute() && self.0.starts_with(std::env::temp_dir()));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "codec-attach-{}",
        opencadcodec::sheet_set::new_id()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let path = root.0.join("poison.dst");
    std::fs::write(&path, b"POISON: not a valid DST").unwrap();
    #[cfg(windows)]
    let _exclusive = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap()
    };
    let code = format!(
        "\\AcSm Database(\"{}\").SheetSet(\"id\").Name",
        path.display()
    );
    let mut f = fixture(&code);
    let report = attach(&mut f);
    assert_eq!(report.fields.len(), 1, "{report:?}");
    assert_eq!(raw(&f.doc, report.fields[0].target).code, code);
    #[cfg(windows)]
    drop(_exclusive);
    assert_eq!(std::fs::read(&path).unwrap(), b"POISON: not a valid DST");
    assert!(!root.0.join("poison.ocss.json").exists());
}

#[test]
#[should_panic(expected = "legacy AcSm evaluation / DST fallback must not be reached")]
fn poison_oracle_detects_the_unchanged_legacy_path() {
    let mut f = fixture(r#"\AcSm Database("poison.dst").SheetSet("id").Name"#);
    f.doc.attach_attribute_fields(f.insert, &Poison);
}

#[test]
fn deferred_literal_and_empty_caches_remain_data() {
    for value in ["", "####", "----", "Ti\u{1ebf}ng Vi\u{1ec7}t \u{1f680}"] {
        let mut f = fixture(r"\AcSm Sheet.Title");
        let source = raw_mut(&mut f.doc, f.source);
        source.value = CellValue::text(value);
        source.value.flags = 4;
        source.value_string = value.into();
        source.value_string_length = value.encode_utf16().count() as i32;
        let before = source.clone();
        let report = attach(&mut f);
        assert_eq!(report.fields.len(), 1, "{report:?}");
        assert_eq!(raw(&f.doc, report.fields[0].target).value, before.value);
        assert_eq!(
            raw(&f.doc, report.fields[0].target).value_string,
            before.value_string
        );
        assert_eq!(target_text(&f.doc, f.insert), format!("[{value}]"));
    }
}
