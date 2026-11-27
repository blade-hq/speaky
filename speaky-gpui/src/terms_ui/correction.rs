use super::{editor_text, ns_string, string_text, Point, Rect, Size};
use crate::correction_ui::{
    append_manual_replacement, manual_replacement, RulePreview, MANUAL_RULE_LABEL, NO_RULES,
    RULES_LABEL,
};
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    rc::StrongPtr,
    runtime::{Class, Object, Sel, BOOL, NO, YES},
    sel, sel_impl,
};
use speaky_core::terms::Replacement;
use std::{cell::RefCell, ffi::c_void, ops::Range, ptr, sync::OnceLock};

const WIDTH: f64 = 470.;
const PREVIEW_HEIGHT: f64 = 67.;
const ROW_HEIGHT: f64 = 23.;
const MANUAL_HEIGHT: f64 = 100.;
const EDITOR_OFFSET: f64 = MANUAL_HEIGHT + 121.;

pub(super) struct Panel {
    view: StrongPtr,
    target: StrongPtr,
    editor: *mut Object,
    hint: *mut Object,
    headings: [*mut Object; 2],
    list: *mut Object,
    manual_fields: [*mut Object; 2],
    manual_hint: *mut Object,
    save: *mut Object,
    review: RefCell<Review>,
}

struct Review {
    preview: RulePreview,
    rows: Vec<*mut Object>,
}

impl Panel {
    pub(super) unsafe fn new(
        editor: *mut Object,
        scroll: *mut Object,
        original: &str,
        save: *mut Object,
    ) -> Box<Self> {
        let view = StrongPtr::new(make_view(
            class!(NSView),
            rect(0., 0., WIDTH, 220. + EDITOR_OFFSET),
        ));
        let _: () = msg_send![scroll, setFrame: rect(0., EDITOR_OFFSET, WIDTH, 220.)];
        let _: () = msg_send![*view, addSubview: scroll];
        label(
            *view,
            rect(0., MANUAL_HEIGHT + 94., WIDTH, 21.),
            RULES_LABEL,
        );
        let hint: *mut Object =
            msg_send![class!(NSTextField), wrappingLabelWithString: ns_string(NO_RULES)];
        let _: () = msg_send![hint, setFrame: rect(1., MANUAL_HEIGHT + 48., WIDTH - 2., 39.)];
        let _: () = msg_send![*view, addSubview: hint];
        let headings = [
            label(*view, rect(24., MANUAL_HEIGHT + 71., 205., 17.), "原写法"),
            label(*view, rect(239., MANUAL_HEIGHT + 71., 205., 17.), "替换为"),
        ];
        let list = StrongPtr::new(make_view(
            class!(NSScrollView),
            rect(0., MANUAL_HEIGHT, WIDTH, PREVIEW_HEIGHT),
        ));
        let _: () = msg_send![*list, setHasVerticalScroller: YES];
        let _: () = msg_send![*list, setBorderType: 2usize];
        let _: () = msg_send![*view, addSubview: *list];
        label(*view, rect(0., 77., WIDTH, 18.), MANUAL_RULE_LABEL);
        let manual_fields = [
            manual_field(*view, 0., "识别错词"),
            manual_field(*view, 242., "替换为"),
        ];
        let manual_hint: *mut Object =
            msg_send![class!(NSTextField), wrappingLabelWithString: ns_string("")];
        let _: () = msg_send![manual_hint, setFrame: rect(0., 0., WIDTH, 31.)];
        let _: () = msg_send![*view, addSubview: manual_hint];
        let target: *mut Object = msg_send![review_class(), new];
        let target = StrongPtr::new(target);
        let panel = Box::new(Self {
            view,
            target,
            editor,
            hint,
            headings,
            list: *list,
            manual_fields,
            manual_hint,
            save,
            review: RefCell::new(Review {
                preview: RulePreview::new(original),
                rows: Vec::new(),
            }),
        });
        let object = *panel.target;
        (*object).set_ivar(
            "panel",
            (&*panel as *const Self).cast_mut().cast::<c_void>(),
        );
        let selector = sel!(refreshSpeakyCorrection:);
        let center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];
        for name in [
            "NSTextDidChangeNotification",
            "NSTextViewDidChangeSelectionNotification",
        ] {
            let _: () = msg_send![center, addObserver: object selector: selector name: ns_string(name) object: editor];
        }
        let selector = sel!(validateSpeakyManualRule:);
        for field in manual_fields {
            for name in [
                "NSControlTextDidChangeNotification",
                "NSControlTextDidEndEditingNotification",
            ] {
                let _: () = msg_send![center, addObserver: object selector: selector name: ns_string(name) object: field];
            }
        }
        panel.refresh();
        panel.validate_manual();
        panel
    }

    pub(super) fn view(&self) -> *mut Object {
        *self.view
    }

    pub(super) unsafe fn selected(&self, text: &str) -> Result<Vec<Replacement>, String> {
        let manual = self.read_manual()?;
        self.refresh();
        let review = self.review.borrow();
        let mut rules = review
            .preview
            .rules
            .iter()
            .zip(&review.rows)
            .filter(|(_, button)| review.preview.text == text && checked(**button))
            .map(|(rule, _)| rule.clone())
            .collect();
        append_manual_replacement(&mut rules, manual);
        Ok(rules)
    }

    unsafe fn read_manual(&self) -> Result<Option<Replacement>, String> {
        let [original, preferred] = self.manual_fields;
        manual_replacement(&field_text(original)?, &field_text(preferred)?).map_err(str::to_owned)
    }

    unsafe fn validate_manual(&self) {
        let validation = self.read_manual();
        let _: () = msg_send![self.save, setEnabled: cocoa_bool(validation.is_ok())];
        let message = validation.as_ref().err().map_or("", String::as_str);
        let _: () = msg_send![self.manual_hint, setStringValue: ns_string(message)];
    }

    unsafe fn refresh(&self) {
        let Ok(mut review) = self.review.try_borrow_mut() else {
            return;
        };
        if let Ok(text) = editor_text(self.editor) {
            let range: NSRange = msg_send![self.editor, selectedRange];
            let range = selection_in_text(&text, range);
            let Review { preview, rows } = &mut *review;
            if preview.update(text, range, |index| {
                rows.get(index).is_some_and(|button| checked(*button))
            }) {
                *rows = self.populate(preview);
            }
        }
        let has_rows = !review.rows.is_empty();
        for view in [self.headings[0], self.headings[1], self.list] {
            let _: () = msg_send![view, setHidden: cocoa_bool(!has_rows)];
        }
        let _: () = msg_send![self.hint, setHidden: cocoa_bool(has_rows)];
    }

    unsafe fn populate(&self, preview: &RulePreview) -> Vec<*mut Object> {
        let rules = &preview.rules;
        let height = ((rules.len() as f64) * ROW_HEIGHT + 4.).max(PREVIEW_HEIGHT - 4.);
        let content = StrongPtr::new(make_view(class!(NSView), rect(0., 0., WIDTH - 18., height)));
        let mut rows = Vec::with_capacity(rules.len());
        for (index, rule) in rules.iter().enumerate() {
            let y = height - 2. - ((index + 1) as f64) * ROW_HEIGHT;
            let row = checkbox(*content, rect(3., y, 225., ROW_HEIGHT), &rule.original);
            let _: () = msg_send![row, setState: isize::from(preview.included(rule))];
            let _: () = msg_send![row, setToolTip: ns_string(&rule.original)];
            let preferred = label(
                *content,
                rect(238., y + 3., 202., ROW_HEIGHT - 4.),
                &rule.preferred,
            );
            let _: () = msg_send![preferred, setToolTip: ns_string(&rule.preferred)];
            rows.push(row);
        }
        let _: () = msg_send![self.list, setDocumentView: *content];
        let clip: *mut Object = msg_send![self.list, contentView];
        let _: () = msg_send![clip, scrollToPoint: Point { x: 0., y: (height - PREVIEW_HEIGHT + 4.).max(0.) }];
        let _: () = msg_send![self.list, reflectScrolledClipView: clip];
        rows
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];
            let _: () = msg_send![center, removeObserver: *self.target];
            (**self.target).set_ivar("panel", ptr::null_mut::<c_void>());
        }
    }
}

fn review_class() -> &'static Class {
    static CLASS: OnceLock<&Class> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut class = ClassDecl::new("SpeakyCorrectionReview", class!(NSObject)).unwrap();
        class.add_ivar::<*mut c_void>("panel");
        unsafe {
            class.add_method(
                sel!(refreshSpeakyCorrection:),
                refresh as extern "C" fn(&Object, Sel, *mut Object),
            );
            class.add_method(
                sel!(validateSpeakyManualRule:),
                validate_manual as extern "C" fn(&Object, Sel, *mut Object),
            );
        }
        class.register()
    })
}

extern "C" fn validate_manual(object: &Object, _: Sel, _: *mut Object) {
    unsafe {
        let panel = *object.get_ivar::<*mut c_void>("panel");
        if let Some(panel) = panel.cast::<Panel>().as_ref() {
            panel.validate_manual();
        }
    }
}

extern "C" fn refresh(object: &Object, _: Sel, _: *mut Object) {
    unsafe {
        let panel = *object.get_ivar::<*mut c_void>("panel");
        if let Some(panel) = panel.cast::<Panel>().as_ref() {
            panel.refresh();
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        origin: Point { x, y },
        size: Size { width, height },
    }
}

fn cocoa_bool(value: bool) -> BOOL {
    if value {
        YES
    } else {
        NO
    }
}

unsafe fn make_view(class: &Class, frame: Rect) -> *mut Object {
    let view: *mut Object = msg_send![class, alloc];
    msg_send![view, initWithFrame: frame]
}

unsafe fn checkbox(parent: *mut Object, frame: Rect, title: &str) -> *mut Object {
    let button = StrongPtr::new(make_view(class!(NSButton), frame));
    let _: () = msg_send![*button, setButtonType: 3usize]; // NSButtonTypeSwitch
    let _: () = msg_send![*button, setTitle: ns_string(title)];
    let _: () = msg_send![*button, setState: 0isize];
    let _: () = msg_send![parent, addSubview: *button];
    *button
}

unsafe fn label(parent: *mut Object, frame: Rect, text: &str) -> *mut Object {
    let label: *mut Object = msg_send![class!(NSTextField), labelWithString: ns_string(text)];
    let _: () = msg_send![label, setFrame: frame];
    let _: () = msg_send![label, setLineBreakMode: 4usize]; // NSLineBreakByTruncatingTail
    let _: () = msg_send![parent, addSubview: label];
    label
}

unsafe fn manual_field(parent: *mut Object, x: f64, caption: &str) -> *mut Object {
    label(parent, rect(x, 57., 228., 17.), caption);
    let field = StrongPtr::new(make_view(class!(NSTextField), rect(x, 33., 228., 22.)));
    let _: () = msg_send![*field, setAccessibilityLabel: ns_string(caption)];
    let _: () = msg_send![parent, addSubview: *field];
    *field
}

unsafe fn field_text(field: *mut Object) -> Result<String, String> {
    // The field editor contains the live value until the user leaves the field.
    let editor: *mut Object = msg_send![field, currentEditor];
    if !editor.is_null() {
        return editor_text(editor);
    }
    let value: *mut Object = msg_send![field, stringValue];
    string_text(value)
}

unsafe fn checked(button: *mut Object) -> bool {
    let state: isize = msg_send![button, state];
    state == 1
}

#[derive(Clone, Copy)]
#[repr(C)]
struct NSRange {
    location: usize,
    length: usize,
}

unsafe impl objc::Encode for NSRange {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{_NSRange=QQ}") }
    }
}

unsafe impl objc::Encode for Point {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
    }
}

fn selection_in_text(text: &str, range: NSRange) -> Option<Range<usize>> {
    if range.length == 0 {
        return None;
    }
    let utf16: Vec<_> = text.encode_utf16().collect();
    let offset = |index| Some(String::from_utf16(utf16.get(..index)?).ok()?.len());
    Some(offset(range.location)?..offset(range.location.checked_add(range.length)?)?)
}
