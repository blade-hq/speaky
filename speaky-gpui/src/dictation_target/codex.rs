use super::*;
use windows::Win32::Foundation::HWND as AutomationHwnd;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

fn is_codex_window(focus: Focus) -> bool {
    // The packaged Codex desktop app's executable is named ChatGPT.exe.
    const APP_ID: &str = "OpenAI.Codex_2p2nqsd0c76g0!App";
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, focus.process);
        if process.is_null() {
            return false;
        }
        let mut id = [0u16; 130];
        let mut length = id.len() as u32;
        let status = GetApplicationUserModelId(process, &mut length, id.as_mut_ptr());
        CloseHandle(process);
        status == 0
            && id
                .split(|unit| *unit == 0)
                .next()
                .is_some_and(|value| value.iter().copied().eq(APP_ID.encode_utf16()))
    }
}

fn is_unfocused_shell(role: UIA_CONTROLTYPE_ID, class: &str, has_focus: bool) -> bool {
    role == UIA_PaneControlTypeId && class == "Chrome_WidgetWin_1" && !has_focus
}

fn resolve_chromium_focus(
    client: &IUIAutomation,
    field: IUIAutomationElement,
    focus: Focus,
) -> Result<IUIAutomationElement, String> {
    unsafe {
        let role = readable(field.CurrentControlType())?;
        if role != UIA_PaneControlTypeId || !is_codex_window(focus) {
            return Ok(field);
        }
        if readable(field.CurrentProcessId())? as u32 != focus.process
            || !is_unfocused_shell(
                role,
                &readable(field.CurrentClassName())?.to_string(),
                readable(field.CurrentHasKeyboardFocus())?.as_bool(),
            )
        {
            return Ok(field);
        }
        focus.require_current()?;
        let window = readable(client.ElementFromHandle(AutomationHwnd(focus.foreground as HWND)))?;
        // The DOM and the native shell can share a PID across multiple Codex windows.
        // Restrict both the initial shell and the resolved element to this window.
        let window_handle = readable(client.CreatePropertyCondition(
            UIA_NativeWindowHandlePropertyId,
            &(focus.foreground as i32).into(),
        ))?;
        let windows = readable(client.CreateTreeWalker(&window_handle))?;
        let belongs_to_window = |element: &IUIAutomationElement| -> Result<bool, String> {
            let owner = readable(windows.NormalizeElement(element))?;
            Ok(readable(client.CompareElements(&owner, &window))?.as_bool())
        };
        if !belongs_to_window(&field)? {
            return Err(FOCUS_CHANGED.into());
        }

        // A cold Chromium provider sometimes returns only its unfocused native pane.
        // Requesting the document activates its accessibility tree without moving focus.
        let document = readable(client.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &UIA_DocumentControlTypeId.0.into(),
        ))?;
        let deadline = Instant::now() + Duration::from_millis(900);
        let _ = window.FindFirst(TreeScope_Descendants, &document);
        loop {
            focus.require_current()?;
            if let Ok(current) = client.GetFocusedElement() {
                if readable(current.CurrentProcessId())? as u32 != focus.process {
                    return Err(FOCUS_CHANGED.into());
                }
                if readable(current.CurrentControlType())? != UIA_PaneControlTypeId
                    && readable(current.CurrentHasKeyboardFocus())?.as_bool()
                {
                    if !belongs_to_window(&current)? {
                        return Err(FOCUS_CHANGED.into());
                    }
                    focus.require_current()?;
                    return Ok(current);
                }
            }
            if Instant::now() >= deadline {
                return Err(NO_EDITOR.into());
            }
            std::thread::sleep(Duration::from_millis(35));
        }
    }
}

pub(super) fn focused_or_composer(
    client: &IUIAutomation,
    field: IUIAutomationElement,
    focus: Focus,
) -> Result<IUIAutomationElement, String> {
    unsafe {
        let field = resolve_chromium_focus(client, field, focus)?;
        let role = readable(field.CurrentControlType())?;
        if !(role == UIA_DocumentControlTypeId || role == UIA_ButtonControlTypeId)
            || !is_codex_window(focus)
            || readable(field.CurrentProcessId())? as u32 != focus.process
            || readable(field.CurrentIsPassword())?.as_bool()
            || !readable(field.CurrentFrameworkId())?
                .to_string()
                .eq_ignore_ascii_case("Chrome")
        {
            return Ok(field);
        }
        // Codex can leave focus on the read-only page or the selected chat in the sidebar.
        // A deliberately focused search box, prompt, or other control keeps its focus.
        if role == UIA_ButtonControlTypeId {
            let class = readable(field.CurrentClassName())?.to_string();
            if !class
                .split_ascii_whitespace()
                .any(|token| token == "sidebar-item")
            {
                return Ok(field);
            }
        } else {
            let value = readable(
                field.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId),
            )?;
            if !readable(value.CurrentIsReadOnly())?.as_bool() {
                return Ok(field);
            }
        }

        let hwnd = focus.foreground as HWND;
        if IsWindowEnabled(hwnd) == 0 {
            return Ok(field);
        }
        let window = readable(client.ElementFromHandle(AutomationHwnd(hwnd)))?;
        let document_type = readable(client.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &UIA_DocumentControlTypeId.0.into(),
        ))?;
        let page = readable(window.FindFirst(TreeScope_Descendants, &document_type))?;
        let documents = readable(client.CreateTreeWalker(&document_type))?;
        let current_page = readable(documents.NormalizeElement(&field))?;
        if !readable(client.CompareElements(&page, &current_page))?.as_bool() {
            return Ok(field);
        }

        let edit =
            readable(client.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &UIA_EditControlTypeId.0.into(),
            ))?;
        let mut filter = edit;
        for (property, value) in [
            (UIA_IsDialogPropertyId, true.into()),
            (UIA_WindowIsModalPropertyId, true.into()),
            (UIA_AriaRolePropertyId, "dialog".into()),
            (UIA_AriaRolePropertyId, "alertdialog".into()),
        ] {
            let dialog = readable(client.CreatePropertyCondition(property, &value))?;
            filter = readable(client.CreateOrCondition(&filter, &dialog))?;
        }
        let matches = readable(window.FindAll(TreeScope_Descendants, &filter))?;
        let mut candidate = None;
        for index in 0..readable(matches.Length())? {
            let element = readable(matches.GetElement(index))?;
            if readable(element.CurrentIsOffscreen())?.as_bool() {
                continue;
            }
            // Any visible dialog prevents taking focus from the task displayed in it.
            if readable(element.CurrentControlType())? != UIA_EditControlTypeId {
                return Ok(field);
            }
            let class = readable(element.CurrentClassName())?.to_string();
            if !class
                .split_ascii_whitespace()
                .any(|token| token == "ProseMirror")
            {
                continue;
            }
            let rect = readable(element.CurrentBoundingRectangle())?;
            if rect.left >= rect.right || rect.top >= rect.bottom {
                continue;
            }
            let enclosing_page = readable(documents.NormalizeElement(&element))?;
            if !readable(client.CompareElements(&page, &enclosing_page))?.as_bool() {
                continue;
            }
            // Additional editors can appear in a question card. In that case the
            // user must pick the intended input explicitly.
            if candidate.is_some()
                || readable(element.CurrentProcessId())? as u32 != focus.process
                || !readable(element.CurrentIsKeyboardFocusable())?.as_bool()
            {
                return Ok(field);
            }
            let editor = AutomationEditor {
                client: client.clone(),
                field: element,
            };
            editor.require_writable()?;
            candidate = Some(editor);
        }
        let Some(editor) = candidate else {
            return Ok(field);
        };
        // Codex also embeds rich-text editors in other screens. Only the chat
        // composer has this CSS module as the immediate raw accessibility parent.
        let raw = readable(client.RawViewWalker())?;
        let parent = readable(raw.GetParentElement(&editor.field))?;
        let class = readable(parent.CurrentClassName())?.to_string();
        if !class
            .split_ascii_whitespace()
            .any(|token| token.starts_with("ComposerLayoutBody-"))
        {
            return Ok(field);
        }
        let original = AutomationEditor {
            client: client.clone(),
            field,
        };
        original.require_focus(focus)?;
        readable(editor.field.SetFocus())?;
        editor.require_focus(focus)?;
        Ok(editor.field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_unfocused_native_chromium_pane_needs_a_document_query() {
        assert!(is_unfocused_shell(
            UIA_PaneControlTypeId,
            "Chrome_WidgetWin_1",
            false
        ));
        assert!(!is_unfocused_shell(
            UIA_PaneControlTypeId,
            "Chrome_WidgetWin_1",
            true
        ));
        assert!(!is_unfocused_shell(
            UIA_PaneControlTypeId,
            "Chrome_RenderWidgetHostHWND",
            false
        ));
        assert!(!is_unfocused_shell(
            UIA_EditControlTypeId,
            "Chrome_WidgetWin_1",
            false
        ));
    }
}
