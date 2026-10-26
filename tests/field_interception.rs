use opencadcodec::entities::{EntityType, MText};
use opencadcodec::fields::*;
use opencadcodec::objects::ObjectType;
use opencadcodec::{CadDocument, Handle};
use std::cell::Cell;
struct Env;
impl FieldContext for Env {
    fn now_julian(&self) -> f64 {
        2460000.0
    }
    fn sheet_sets(
        &self,
        _: &mut dyn FnMut(&opencadcodec::sheet_set::SheetSetDatabase) -> Option<String>,
    ) -> Option<String> {
        panic!("strict AcSm must never reach legacy database callback")
    }
}
struct Hook(FieldDecision);
impl FieldInterceptor for Hook {
    fn intercept(&self, _: FieldEvaluation<'_>) -> FieldDecision {
        self.0.clone()
    }
}
struct Delegate;
impl FieldInterceptor for Delegate {}
fn request(code: &str) -> PreviewRequest<'_> {
    PreviewRequest {
        code,
        objects: &[],
        host: None,
        layout: None,
    }
}
#[test]
fn delegate_preserves_existing_non_acsm_results() {
    let doc = CadDocument::default();
    for code in [r"\AcVar Filename", r"\AcExpr (2+3)", r"\AcDiesel $(+,2,3)"] {
        let expected = evaluate_code(&doc, code, &[], None, &Env);
        let actual = evaluate_code_checked(&doc, request(code), &Env, &Delegate);
        match expected {
            Some(value) => {
                assert!(matches!(actual, CheckedEvaluation::Value(v) if v.display == value))
            }
            None => assert!(matches!(actual, CheckedEvaluation::Preserved(_))),
        }
    }
}
#[test]
fn handled_literals_missing_and_empty_are_not_sentinels() {
    let doc = CadDocument::default();
    for (raw, display) in [("####", "####"), ("----", "----"), ("", "----")] {
        let value = ResolvedText {
            raw: Some(raw.into()),
            display: display.into(),
        };
        let result = evaluate_code_checked(
            &doc,
            request(r"\AcSm Sheet.Title"),
            &Env,
            &Hook(FieldDecision::HandledValue(value.clone())),
        );
        assert_eq!(result, CheckedEvaluation::Value(value));
    }
    let result = evaluate_code_checked(
        &doc,
        request(r"\AcSm Sheet.Title"),
        &Env,
        &Hook(FieldDecision::HandledPreserve(FieldDiagnostic::preserve(
            "Missing",
        ))),
    );
    assert!(matches!(result, CheckedEvaluation::Preserved(d) if d.message == "Missing"));
}
#[test]
fn acsm_family_delegate_is_fail_closed_including_unknown_versions() {
    let doc = CadDocument::default();
    for code in [
        r"\AcSm Sheet.Title",
        r"\AcSm.99 Sheet.Title",
        r"%<\AcSm.99 Sheet.Title>%",
        r"\acsm.99 Sheet.Title",
    ] {
        let result = evaluate_code_checked(&doc, request(code), &Env, &Delegate);
        assert!(
            matches!(result, CheckedEvaluation::Preserved(d) if d.code == FieldDiagnosticCode::UnhandledAcSm)
        );
    }
}
#[test]
fn preview_verifies_layout_source_and_never_fabricates_stored_identity() {
    let mut doc = CadDocument::default();
    doc.source_path = Some(r"C:\Project\drawing.dwg".into());
    let a = doc.add_layout("A").unwrap();
    let b = doc.add_layout("B").unwrap();
    let host = doc
        .add_entity_to_layout(EntityType::MText(MText::new()), "A")
        .unwrap();
    struct Inspect(Cell<usize>);
    impl FieldInterceptor for Inspect {
        fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
            let FieldEvaluation::Preview(v) = input else {
                panic!("not a stored identity")
            };
            assert_eq!(v.source_path(), Some(r"C:\Project\drawing.dwg"));
            assert!(matches!(
                v.layout(),
                LayoutEvidence::Exact { name: "A", .. }
            ));
            assert!(v.host().is_some());
            self.0.set(self.0.get() + 1);
            FieldDecision::HandledValue(ResolvedText {
                raw: None,
                display: "preview".into(),
            })
        }
    }
    let hook = Inspect(Cell::new(0));
    let before = doc.clone();
    let mut req = request(r"\AcSm Sheet.Title");
    req.host = Some(host);
    req.layout = Some(a);
    assert!(matches!(
        evaluate_code_checked(&doc, req, &Env, &hook),
        CheckedEvaluation::Value(_)
    ));
    let mut req = request(r"\AcSm Sheet.Title");
    req.host = Some(host);
    req.layout = Some(b);
    assert!(matches!(
        evaluate_code_checked(&doc, req, &Env, &hook),
        CheckedEvaluation::Preserved(_)
    ));
    assert_eq!(hook.0.get(), 1);
    assert_eq!(doc, before);
}
#[test]
fn missing_and_ambiguous_layout_are_explicit_not_ctab() {
    let mut doc = CadDocument::default();
    let layout = doc.add_layout("A").unwrap();
    let host = doc
        .add_entity_to_layout(EntityType::MText(MText::new()), "A")
        .unwrap();
    let mut duplicate = match doc.objects.get(&layout).unwrap() {
        ObjectType::Layout(l) => l.clone(),
        _ => unreachable!(),
    };
    duplicate.handle = Handle::new(0xFFFF);
    doc.objects
        .insert(duplicate.handle, ObjectType::Layout(duplicate));
    struct Check;
    impl FieldInterceptor for Check {
        fn intercept(&self, input: FieldEvaluation<'_>) -> FieldDecision {
            let FieldEvaluation::Preview(v) = input else {
                unreachable!()
            };
            assert_eq!(v.layout(), LayoutEvidence::Ambiguous);
            FieldDecision::HandledPreserve(FieldDiagnostic::preserve("ambiguous"))
        }
    }
    let mut req = request(r"\AcSm Sheet.Title");
    req.host = Some(host);
    assert!(matches!(
        evaluate_code_checked(&doc, req, &Env, &Check),
        CheckedEvaluation::Preserved(_)
    ));
    let mut req = request(r"\AcSm Sheet.Title");
    req.layout = Some(Handle::new(0xDEAD));
    assert!(matches!(
        evaluate_code_checked(&doc, req, &Env, &Delegate),
        CheckedEvaluation::Preserved(_)
    ));
}
