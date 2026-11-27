use super::{read_text, selection_in_transcript, wide, EDITOR};
use crate::correction_ui::{
    append_manual_replacement, manual_replacement, RulePreview, MANUAL_RULE_LABEL, NO_RULES,
    RULES_LABEL,
};
use speaky_core::terms::Replacement;
use std::mem::size_of;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    UI::{
        Controls::*,
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const LABEL: i32 = 110;
const RULES: i32 = 111;
const HINT: i32 = 112;
const MANUAL_LABEL: i32 = 113;
const HEARD_LABEL: i32 = 114;
pub(super) const HEARD: i32 = 115;
const PREFERRED_LABEL: i32 = 116;
pub(super) const PREFERRED: i32 = 117;
const MANUAL_ERROR: i32 = 118;
pub(super) const REFRESH: u32 = WM_APP + 1;
const CHECKED: u32 = 2 << 12;
const UNCHECKED: u32 = 1 << 12;

pub(super) fn register_controls() {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    unsafe { InitCommonControlsEx(&controls) };
}

pub(super) unsafe fn init(dialog: HWND) {
    SetDlgItemTextW(dialog, LABEL, wide(RULES_LABEL).as_ptr());
    SetDlgItemTextW(dialog, HINT, wide(NO_RULES).as_ptr());
    SetDlgItemTextW(dialog, MANUAL_LABEL, wide(MANUAL_RULE_LABEL).as_ptr());
    SetDlgItemTextW(dialog, HEARD_LABEL, wide("识别错词").as_ptr());
    SetDlgItemTextW(dialog, PREFERRED_LABEL, wide("替换为").as_ptr());
    let list = GetDlgItem(dialog, RULES);
    let mut client = RECT::default();
    GetClientRect(list, &mut client);
    let width = (client.right - client.left - GetSystemMetrics(SM_CXVSCROLL)).max(1) / 2;
    SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP) as LPARAM,
    );
    for (index, label) in ["原写法", "替换为"].into_iter().enumerate() {
        let mut text = wide(label);
        let column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: width,
            pszText: text.as_mut_ptr(),
            ..Default::default()
        };
        SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            index,
            &column as *const _ as LPARAM,
        );
    }
    ShowWindow(list, SW_HIDE);
    SetWindowSubclass(
        GetDlgItem(dialog, EDITOR),
        Some(editor_proc),
        1,
        dialog as usize,
    );
}

// The standard EDIT control sends EN_CHANGE but has no selection-change message.
unsafe extern "system" fn editor_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    dialog: usize,
) -> isize {
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        EM_SETSEL | WM_KEYUP | WM_LBUTTONUP | WM_LBUTTONDBLCLK
    ) {
        PostMessageW(dialog as HWND, REFRESH, 0, 0);
    }
    result
}

unsafe fn checked(list: HWND, index: usize) -> bool {
    SendMessageW(list, LVM_GETITEMSTATE, index, LVIS_STATEIMAGEMASK as LPARAM) == CHECKED as isize
}

pub(super) unsafe fn set_manual_error(dialog: HWND, message: &str) {
    SetDlgItemTextW(dialog, MANUAL_ERROR, wide(message).as_ptr());
}

pub(super) unsafe fn selected(
    dialog: HWND,
    preview: &RulePreview,
) -> Result<Vec<Replacement>, &'static str> {
    let heard = String::from_utf16_lossy(&read_text(GetDlgItem(dialog, HEARD)));
    let preferred = String::from_utf16_lossy(&read_text(GetDlgItem(dialog, PREFERRED)));
    let manual = manual_replacement(&heard, &preferred)?;
    let list = GetDlgItem(dialog, RULES);
    let mut rules = preview
        .rules
        .iter()
        .enumerate()
        .filter(|(index, _)| checked(list, *index))
        .map(|(_, rule)| rule.clone())
        .collect();
    append_manual_replacement(&mut rules, manual);
    Ok(rules)
}

pub(super) unsafe fn refresh(dialog: HWND, preview: &mut RulePreview) {
    let list = GetDlgItem(dialog, RULES);
    let editor = GetDlgItem(dialog, EDITOR);
    let raw = read_text(editor);
    let (mut start, mut end) = (0u32, 0u32);
    SendMessageW(
        editor,
        EM_GETSEL,
        &mut start as *mut u32 as WPARAM,
        &mut end as *mut u32 as LPARAM,
    );
    let text = String::from_utf16_lossy(&raw).replace("\r\n", "\n");
    let selection = selection_in_transcript(&raw, start, end);
    if preview.update(text, selection, |index| checked(list, index)) {
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        for (index, rule) in preview.rules.iter().enumerate() {
            let mut original = wide(&rule.original);
            let mut preferred = wide(&rule.preferred);
            let mut item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                pszText: original.as_mut_ptr(),
                ..Default::default()
            };
            if SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as LPARAM) != index as isize
            {
                SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
                break;
            }
            item.iSubItem = 1;
            item.pszText = preferred.as_mut_ptr();
            SendMessageW(list, LVM_SETITEMTEXTW, index, &item as *const _ as LPARAM);
            item.stateMask = LVIS_STATEIMAGEMASK;
            item.state = if preview.included(rule) {
                CHECKED
            } else {
                UNCHECKED
            };
            SendMessageW(list, LVM_SETITEMSTATE, index, &item as *const _ as LPARAM);
        }
    }
    let has_rows = SendMessageW(list, LVM_GETITEMCOUNT, 0, 0) > 0;
    let hint = GetDlgItem(dialog, HINT);
    ShowWindow(list, if has_rows { SW_SHOW } else { SW_HIDE });
    ShowWindow(hint, if has_rows { SW_HIDE } else { SW_SHOW });
}
