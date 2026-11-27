use crate::{
    app::{self, Event, Updates},
    d2d::{self, Graphics},
    files, floating,
    hotkey_capture::{self, Event as ShortcutEvent, Keys as ShortcutKeys},
    overlay::{Layer, Overlay},
    tray,
};
use speaky_core::{
    audio::{AudioRecorder, VoiceActivity},
    config::{Config, RemoteService},
    drafts, engines, history,
    hotkey::{GlobalHotkey, HotkeyEvents, Shortcut},
    playback::Playback,
    session::{LANGUAGES, SAMPLE_AUDIO},
    terms,
};
use std::{
    cell::RefCell,
    mem::size_of,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Instant,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
    UI::{
        Controls::{
            InitCommonControlsEx, DRAWITEMSTRUCT, EM_GETSEL, EM_SETREADONLY, EM_SETSEL,
            ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, ODS_FOCUS,
            ODS_SELECTED, PBM_SETPOS,
        },
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::*,
    },
};

mod benchmark;
mod diagnostics;
mod theme;

const CLASS: windows_sys::core::PCWSTR = windows_sys::w!("SpeakyWin7App");
const WAKE: u32 = WM_APP + 3;
pub const RECORD: usize = 1;
pub const SETTINGS: usize = 2;
const TERMS: usize = 3;
const LIST: usize = 4;
const COPY: usize = 5;
const PLAY: usize = 6;
const RESUME: usize = 7;
const TRANSCRIBE: usize = 8;
const EDIT: usize = 9;
const SAVE_EDIT: usize = 10;
const STATUS: usize = 11;
const BODY: usize = 12;
const BACK: usize = 13;
const ADDRESS: usize = 20;
const MODEL: usize = 21;
const KEY: usize = 22;
const DEVICE: usize = 23;
const LANGUAGE: usize = 24;
const HOTKEY: usize = 25;
const FLOAT: usize = 26;
const DETECT: usize = 27;
const CHECK: usize = 28;
const PREFERENCES: usize = 29;
const PROVIDER: usize = 30;
const DICTIONARY: usize = 31;
const SAVE_TERMS: usize = 32;
const LEARNED: usize = 33;
const FETCH_MODELS: usize = 34;
const CANCEL_HOTKEY: usize = 35;
const HOTKEY_HINT: usize = 36;
const NAV_HOME: usize = 100;
const NAV_FILES: usize = 101;
const NAV_HISTORY: usize = 102;
const NAV_GUIDE: usize = 103;
const NAV_DIAGNOSTICS: usize = 104;
const NAV_ORGANIZER: usize = 105;
const THEME: usize = 106;
const IMPORT: usize = 107;
const EXPORT: usize = 108;
const HISTORY_SEARCH: usize = 109;
const ORGANIZED: usize = 110;
const LIVE_TEXT: usize = 111;
const INPUT_LEVEL: usize = 112;
const HOME_RECENT: usize = 113;
const HOME_MODELS: usize = 114;
const GUIDE_NEXT: usize = 115;
const GUIDE_PREV: usize = 116;
const FORGET_TERM: usize = 117;
const TERM_TO_FORGET: usize = 118;
const NAV_MODELS: usize = 119;
const NAV_BENCHMARK: usize = 120;
const RAIL_WIDTH: i32 = 152;
const LOADING_MODELS: &str = "正在获取模型列表…";

thread_local! {
    static STATE: RefCell<Option<Window>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Home,
    Files,
    History,
    Settings,
    Terms,
    Guide,
    Organizer,
    Diagnostics,
    Benchmark,
}

struct ShortcutCapture {
    keys: ShortcutKeys,
    previous: Shortcut,
}

struct Window {
    hwnd: HWND,
    scale: f32,
    font: HFONT,
    title_font: HFONT,
    config: Config,
    tray: tray::Tray,
    graphics: Option<Graphics>,
    overlay: Option<Overlay>,
    floating: Option<Layer>,
    hotkey: Option<(GlobalHotkey, HotkeyEvents)>,
    shortcut_capture: Option<ShortcutCapture>,
    updates: Updates,
    events: Receiver<Event>,
    stop: Option<Sender<app::StopRequest>>,
    cancel_after_start: bool,
    escape_down: bool,
    clipboard_notice: Option<(Layer, Instant)>,
    unsaved: Option<app::Pending>,
    retry_transcription: Option<PathBuf>,
    starting: bool,
    stop_after_start: bool,
    startup_stop: Option<app::StopRequest>,
    working: bool,
    saving_audio: bool,
    exiting: bool,
    activity: VoiceActivity,
    elapsed: u64,
    caption: String,
    clock: Instant,
    period: u32,
    floating_dirty: bool,
    rows: app::Rows,
    history_indices: Vec<usize>,
    history_query: String,
    organized_visible: bool,
    selected: usize,
    editing: bool,
    play: Option<Playback>,
    panel: Panel,
    guide_step: usize,
    benchmark: benchmark::BenchmarkState,
    controls: Vec<HWND>,
    devices: Vec<(u32, String)>,
    model_catalog: Option<engines::ModelCatalog>,
    model_catalog_request: u64,
    loading_model_catalog: bool,
    status: String,
    auxiliary_busy: bool,
    taskbar_created: u32,
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub fn px(scale: f32, value: i32) -> i32 {
    (value as f32 * scale).round() as i32
}

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(null()) }
}

pub fn alert(hwnd: HWND, msg: &str) {
    unsafe {
        MessageBoxW(
            hwnd,
            wide(msg).as_ptr(),
            windows_sys::w!("Speaky"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        let lock = CreateMutexW(null(), 0, windows_sys::w!("Local\\SpeakyWin7App"));
        if lock.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let _lock = OwnedHandle::from_raw_handle(lock);
        if exists {
            let previous = FindWindowW(CLASS, null());
            if !previous.is_null() {
                let pid = &mut 0;
                GetWindowThreadProcessId(previous, pid);
                AllowSetForegroundWindow(*pid);
                PostMessageW(previous, WAKE, 0, 0);
            }
            return Ok(());
        }
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let _com = Com;
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS,
        });
        for (class, callback, background) in [
            (
                CLASS,
                Some(main_proc as unsafe extern "system" fn(_, _, _, _) -> _),
                (COLOR_WINDOW + 1) as HBRUSH,
            ),
            (
                windows_sys::w!("SpeakyWin7Layer"),
                Some(layer_proc as unsafe extern "system" fn(_, _, _, _) -> _),
                null_mut(),
            ),
        ] {
            if RegisterClassW(&WNDCLASSW {
                lpfnWndProc: callback,
                hInstance: instance(),
                hIcon: tray::icon(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: background,
                lpszClassName: class,
                ..Default::default()
            }) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        let mut work = windows_sys::Win32::Foundation::RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0);
        let scale = d2d::system_scale()
            .min(((work.right - work.left - 24) as f32 / (660 + RAIL_WIDTH) as f32).max(0.7));
        let mut bounds = windows_sys::Win32::Foundation::RECT {
            right: px(scale, 660 + RAIL_WIDTH),
            bottom: px(scale, 450),
            ..Default::default()
        };
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        AdjustWindowRectEx(&mut bounds, style, 0, 0);
        let hwnd = CreateWindowExW(
            0,
            CLASS,
            windows_sys::w!("Speaky · Windows 7"),
            style,
            work.left + ((work.right - work.left - (bounds.right - bounds.left)) / 2).max(0),
            work.top + ((work.bottom - work.top - (bounds.bottom - bounds.top)) / 2).max(0),
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            null_mut(),
            null_mut(),
            instance(),
            null_mut(),
        );
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let (tx, events) = mpsc::channel();
        let updates = Updates {
            window: hwnd as usize,
            events: tx,
        };
        let (config, mut status) = match Config::load() {
            Ok(config) => (config, "就绪".into()),
            Err(error) => (Config::default(), format!("读取配置失败：{error}")),
        };
        let shortcut = config
            .core
            .asr
            .windows_toggle_hotkey
            .as_deref()
            .and_then(Shortcut::from_id)
            .unwrap_or_default();
        let hotkey = GlobalHotkey::register(shortcut)
            .map_err(|error| status = error)
            .ok();
        let graphics = Graphics::new(scale)
            .map_err(|error| status = format!("悬浮窗无法启动：{error}"))
            .ok();
        let font = CreateFontW(
            -px(scale, 15),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            windows_sys::w!("Microsoft YaHei"),
        );
        let title_font = CreateFontW(
            -px(scale, 24),
            0,
            0,
            0,
            FW_SEMIBOLD as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            windows_sys::w!("Microsoft YaHei"),
        );
        theme::set_palette(&config.appearance.theme);
        let mut state = Window {
            hwnd,
            scale,
            font,
            title_font,
            config,
            tray: tray::Tray::new(hwnd),
            graphics,
            overlay: None,
            floating: None,
            hotkey,
            shortcut_capture: None,
            updates: updates.clone(),
            events,
            stop: None,
            cancel_after_start: false,
            escape_down: false,
            clipboard_notice: None,
            unsaved: None,
            retry_transcription: None,
            starting: false,
            stop_after_start: false,
            startup_stop: None,
            working: false,
            saving_audio: false,
            exiting: false,
            activity: VoiceActivity {
                level: 0.,
                pitch_hz: None,
            },
            elapsed: 0,
            caption: String::new(),
            clock: Instant::now(),
            period: 32,
            floating_dirty: true,
            rows: Vec::new(),
            history_indices: Vec::new(),
            history_query: String::new(),
            organized_visible: false,
            selected: 0,
            editing: false,
            play: None,
            panel: Panel::Home,
            guide_step: 0,
            benchmark: Default::default(),
            controls: Vec::new(),
            devices: Vec::new(),
            model_catalog: None,
            model_catalog_request: 0,
            loading_model_catalog: false,
            status,
            auxiliary_busy: false,
            taskbar_created: RegisterWindowMessageW(windows_sys::w!("TaskbarCreated")),
        };
        state.layout();
        state.sync_floating();
        STATE.with(|slot| *slot.borrow_mut() = Some(state));
        thread::spawn(move || updates.send(Event::History(app::rows())));
        SetTimer(hwnd, 1, 32, None);
        ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result == -1 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if result == 0 {
                break;
            }
            // Consume recording keys before dialog navigation handles Tab, Enter or Esc.
            // End the STATE borrow before any normal dispatch can synchronously reenter it.
            let captured = STATE.with(|slot| {
                slot.try_borrow_mut()
                    .ok()
                    .and_then(|mut state| state.as_mut().map(|state| state.capture_shortcut(&msg)))
                    .unwrap_or(false)
            });
            if captured {
                continue;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        STATE.with(|slot| drop(slot.borrow_mut().take()));
        DestroyWindow(hwnd);
        DeleteObject(font);
        DeleteObject(title_font);
        Ok(())
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if matches!(
        message,
        WM_ERASEBKGND | WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN
    ) {
        if let Some(result) = theme::paint_control(hwnd, message, w, l) {
            return result;
        }
    }
    if message == WM_DRAWITEM {
        if let Some(result) = STATE.with(|slot| {
            slot.try_borrow()
                .ok()
                .and_then(|slot| slot.as_ref().and_then(|state| state.paint(message, w, l)))
        }) {
            return result;
        }
    }
    let handled = STATE.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return false;
        };
        let Some(state) = slot.as_mut() else {
            return false;
        };
        match message {
            WM_TIMER => state.timer(),
            app::UPDATE => state.update(),
            WM_COMMAND => {
                let id = w & 0xffff;
                if id == PROVIDER {
                    if w >> 16 == CBN_SELCHANGE as usize {
                        state.provider_changed();
                    }
                } else if id == MODEL {
                    if w >> 16 == CBN_DROPDOWN as usize && state.model_catalog.is_none() {
                        state.fetch_models(true);
                    }
                } else if matches!(id, ADDRESS | KEY) {
                    if w >> 16 == EN_CHANGE as usize {
                        state.clear_models();
                    }
                } else if id == LIST {
                    if w >> 16 == LBN_SELCHANGE as usize {
                        state.select();
                    }
                } else if id == HISTORY_SEARCH {
                    if w >> 16 == EN_CHANGE as usize && !state.editing {
                        state.history_query = state.text(HISTORY_SEARCH);
                        state.fill_history();
                    }
                } else {
                    state.command(id);
                }
            }
            WM_CLOSE => {
                state.cancel_shortcut_capture();
                ShowWindow(hwnd, SW_HIDE);
            }
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => {
                state.cancel_shortcut_capture();
            }
            WM_QUERYENDSESSION => {
                if state.stop.is_some()
                    || state.starting
                    || state.saving_audio
                    || state.unsaved.is_some()
                {
                    state.command(tray::EXIT);
                    return true;
                }
                return false;
            }
            WAKE => state.open(),
            tray::MESSAGE => match l as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let id = state.tray.menu(
                        state.record_label(),
                        state.config.desktop.windows_floating_button,
                    );
                    if id != 0 {
                        state.command(id);
                    }
                }
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => state.open(),
                _ => {}
            },
            _ if message == state.taskbar_created && message != 0 => {
                state.tray.restore();
            }
            _ => return false,
        }
        true
    });
    if handled {
        0
    } else {
        DefWindowProcW(hwnd, message, w, l)
    }
}

unsafe extern "system" fn layer_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_MOUSEACTIVATE {
        return MA_NOACTIVATE as isize;
    }
    if matches!(message, WM_LBUTTONDOWN | WM_LBUTTONUP) {
        STATE.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return;
            };
            let Some(state) = slot.as_mut() else { return };
            let overlay = state
                .overlay
                .as_mut()
                .map(|overlay| &mut overlay.layer)
                .filter(|layer| layer.hwnd == hwnd);
            let is_overlay = overlay.is_some();
            let Some(layer) =
                overlay.or_else(|| state.floating.as_mut().filter(|layer| layer.hwnd == hwnd))
            else {
                return;
            };
            if message == WM_LBUTTONDOWN {
                layer.pointer_down();
            } else if layer.pointer_update(true, is_overlay) {
                if state.stop.is_some() && layer.cancel_hit() {
                    state.cancel();
                } else {
                    state.layer_click();
                }
            }
        });
        return 0;
    }
    DefWindowProcW(hwnd, message, w, l)
}

impl Window {
    fn control(
        &mut self,
        id: usize,
        class: &str,
        text: &str,
        (x, y, w, h): (i32, i32, i32, i32),
        style: u32,
    ) {
        self.control_at(id, class, text, (x + RAIL_WIDTH, y, w, h), style);
    }

    fn control_at(
        &mut self,
        id: usize,
        class: &str,
        text: &str,
        (x, y, w, h): (i32, i32, i32, i32),
        style: u32,
    ) {
        unsafe {
            let hwnd = CreateWindowExW(
                if matches!(class, "EDIT" | "LISTBOX") {
                    WS_EX_CLIENTEDGE
                } else {
                    0
                },
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                px(self.scale, x),
                px(self.scale, y),
                px(self.scale, w),
                px(self.scale, h),
                self.hwnd,
                id as _,
                instance(),
                null_mut(),
            );
            SendMessageW(hwnd, WM_SETFONT, self.font as usize, 1);
            self.controls.push(hwnd);
        }
    }

    fn button(&mut self, id: usize, text: &str, x: i32, y: i32, w: i32) {
        self.control(
            id,
            "BUTTON",
            text,
            (x, y, w, 30),
            WS_TABSTOP | BS_OWNERDRAW as u32,
        );
    }

    fn label(&mut self, text: &str, x: i32, y: i32, w: i32) {
        self.control(0, "STATIC", text, (x, y, w, 23), 0);
    }

    fn child(&self, id: usize) -> HWND {
        unsafe { GetDlgItem(self.hwnd, id as i32) }
    }

    fn set(&self, id: usize, text: &str) {
        let child = self.child(id);
        if child.is_null() {
            return;
        }
        unsafe {
            SetWindowTextW(child, wide(text).as_ptr());
            InvalidateRect(child, null(), 0);
        }
    }

    fn text(&self, id: usize) -> String {
        unsafe {
            let child = self.child(id);
            let mut data = vec![0; GetWindowTextLengthW(child) as usize + 1];
            let len = GetWindowTextW(child, data.as_mut_ptr(), data.len() as i32);
            String::from_utf16_lossy(&data[..len as usize])
        }
    }

    fn send(&self, id: usize, message: u32, w: usize, l: isize) -> isize {
        unsafe { SendMessageW(self.child(id), message, w, l) }
    }

    fn status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.set(STATUS, &self.status);
        self.set(RECORD, self.record_label());
        self.floating_dirty = true;
    }

    fn record_label(&self) -> &'static str {
        if self.unsaved.is_some() {
            "重试保存"
        } else if self.stop.is_some() || self.starting {
            "停止并保存"
        } else if self.working {
            "转写中…"
        } else {
            "开始录音"
        }
    }

    fn open(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_RESTORE);
            SetForegroundWindow(self.hwnd);
        }
    }

    fn layout(&mut self) {
        self.cancel_shortcut_capture();
        self.model_catalog = None;
        self.model_catalog_request += 1;
        self.loading_model_catalog = false;
        if self.status == LOADING_MODELS {
            self.status = "就绪".into();
        }
        for control in self.controls.drain(..) {
            unsafe {
                DestroyWindow(control);
            }
        }
        let status = self.status.clone();
        self.control(STATUS, "STATIC", &status, (14, 12, 630, 42), 0);
        self.navigation();
        match self.panel {
            Panel::Home => self.home_layout(),
            Panel::Files => self.files_layout(),
            Panel::Guide => self.guide_layout(),
            Panel::Organizer => self.organizer_layout(),
            Panel::Diagnostics => self.diagnostics_layout(),
            Panel::Benchmark => self.benchmark_layout(),
            Panel::History => {
                self.heading("历史记录");
                let query = self.history_query.clone();
                self.label("搜索", 14, 92, 54);
                self.control(
                    HISTORY_SEARCH,
                    "EDIT",
                    &query,
                    (66, 89, 228, 25),
                    WS_TABSTOP | ES_AUTOHSCROLL as u32,
                );
                self.button(EXPORT, "导出文字", 305, 87, 94);
                self.button(ORGANIZED, "原文 / 整理稿", 409, 87, 116);
                self.button(diagnostics::ORGANIZE_HISTORY, "整理文字", 535, 87, 109);
                self.label("录音", 14, 120, 200);
                self.label("正文", 260, 120, 200);
                self.control(
                    LIST,
                    "LISTBOX",
                    "",
                    (14, 145, 230, 248),
                    WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
                );
                self.control(
                    BODY,
                    "EDIT",
                    "",
                    (260, 145, 384, 248),
                    WS_VSCROLL | WS_TABSTOP | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                );
                for (id, label, x, width) in [
                    (COPY, "复制", 14, 72),
                    (PLAY, "回放 / 停止", 94, 104),
                    (RESUME, "续录", 206, 72),
                    (TRANSCRIBE, "重新转写", 286, 103),
                    (EDIT, "改错词", 397, 90),
                    (SAVE_EDIT, "保存改词", 495, 104),
                ] {
                    self.button(id, label, x, 404, width);
                }
                self.fill_history();
            }
            Panel::Settings => self.settings_layout(),
            Panel::Terms => {
                self.heading("词典");
                self.label("每行一条常用词或替换规则", 14, 100, 600);
                self.control(
                    DICTIONARY,
                    "EDIT",
                    "",
                    (14, 128, 630, 156),
                    WS_VSCROLL
                        | WS_TABSTOP
                        | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
                );
                self.label("自动学习的词条", 14, 292, 600);
                self.control(
                    LEARNED,
                    "EDIT",
                    "",
                    (14, 318, 630, 64),
                    WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                );
                self.button(SAVE_TERMS, "保存常用词", 14, 404, 130);
                self.control(
                    TERM_TO_FORGET,
                    "EDIT",
                    "",
                    (253, 405, 253, 27),
                    WS_TABSTOP | ES_AUTOHSCROLL as u32,
                );
                self.button(FORGET_TERM, "移除学习词", 518, 404, 126);
            }
        }
        unsafe {
            InvalidateRect(self.hwnd, null(), 1);
        }
    }

    fn heading(&mut self, text: &str) {
        self.control(0, "STATIC", text, (14, 51, 420, 33), 0);
        unsafe {
            SendMessageW(
                *self.controls.last().unwrap(),
                WM_SETFONT,
                self.title_font as usize,
                1,
            );
        }
    }

    fn navigation(&mut self) {
        self.control_at(0, "STATIC", "Speaky", (19, 23, 125, 35), 0);
        unsafe {
            SendMessageW(
                *self.controls.last().unwrap(),
                WM_SETFONT,
                self.title_font as usize,
                1,
            );
        }
        self.control_at(0, "STATIC", "Windows 7", (20, 61, 110, 24), 0);
        for (id, label, y) in [
            (NAV_HOME, "首页", 92),
            (NAV_MODELS, "模型选择", 129),
            (NAV_FILES, "转录文件", 166),
            (NAV_HISTORY, "历史记录", 203),
            (TERMS, "词典", 240),
            (NAV_BENCHMARK, "模型评测", 277),
            (NAV_GUIDE, "进入引导", 326),
            (NAV_DIAGNOSTICS, "设置", 363),
        ] {
            self.control_at(
                id,
                "BUTTON",
                label,
                (12, y, 125, 33),
                WS_TABSTOP | BS_OWNERDRAW as u32,
            );
        }
        let caption = if self.config.appearance.theme == "light" {
            "深色主题"
        } else {
            "浅色主题"
        };
        self.control_at(
            THEME,
            "BUTTON",
            caption,
            (12, 402, 125, 32),
            WS_TABSTOP | BS_OWNERDRAW as u32,
        );
    }

    fn home_layout(&mut self) {
        self.heading("首页");
        self.label("听写与录音", 14, 103, 260);
        self.button(RECORD, self.record_label(), 14, 136, 155);
        self.button(HOME_MODELS, "选择识别模型", 182, 136, 149);
        self.label(
            &format!("快捷键：{}", self.selected_shortcut().label()),
            354,
            141,
            280,
        );
        let model = self.config.engine.remote.label();
        self.label(&format!("识别模型：{model}"), 14, 184, 620);
        self.label("输入音量", 14, 222, 89);
        self.control(INPUT_LEVEL, "msctls_progress32", "", (111, 226, 533, 15), 0);
        self.label("实时转写", 14, 256, 150);
        self.control(
            LIVE_TEXT,
            "EDIT",
            "",
            (14, 283, 630, 87),
            WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
        );
        self.button(NAV_HISTORY, "查看历史", 14, 398, 109);
        self.control(HOME_RECENT, "STATIC", "", (135, 404, 508, 25), 0);
        self.refresh_home();
    }

    fn refresh_home(&self) {
        if self.panel != Panel::Home {
            return;
        }
        let label = self.rows.first().map_or_else(
            || "暂无录音".into(),
            |(_, label, _)| format!("最近：{label}"),
        );
        self.set(HOME_RECENT, &label);
    }

    fn audio_busy(&self) -> bool {
        self.working || self.starting || self.stop.is_some() || self.unsaved.is_some()
    }

    fn import_file(&mut self) {
        if self.audio_busy() || self.auxiliary_busy {
            self.status("请先完成当前录音或转写。");
            return;
        }
        match files::pick_wav(self.hwnd) {
            Ok(Some(path)) => {
                self.working = true;
                self.status("正在保存导入的录音…");
                let updates = self.updates.clone();
                thread::spawn(move || updates.send(Event::Imported(files::import_wav(&path))));
            }
            Ok(None) => {}
            Err(error) => self.status(error),
        }
    }

    fn files_layout(&mut self) {
        self.heading("转录文件");
        self.label(
            "选择 WAV 文件，保存到本地历史并用当前模型转写。",
            14,
            110,
            625,
        );
        self.button(IMPORT, "选择 WAV 文件", 14, 150, 156);
        self.button(NAV_HISTORY, "查看历史", 182, 150, 106);
    }

    fn guide_layout(&mut self) {
        self.heading("首次引导");
        let steps = [
            (
                "1 / 4   选择麦克风",
                "打开设置，测试麦克风并确认有输入。",
                NAV_DIAGNOSTICS,
                "打开麦克风测试",
            ),
            (
                "2 / 4   设置快捷键",
                "在模型设置页点击当前快捷键，按下你想使用的组合。",
                NAV_MODELS,
                "配置快捷键",
            ),
            (
                "3 / 4   配置识别模型",
                "选择模型和识别语言，用内置样本测试转写。",
                NAV_MODELS,
                "选择模型",
            ),
            (
                "4 / 4   试录一句话",
                "返回首页，点击开始录音，试说一句话，再停止并保存。",
                NAV_HOME,
                "开始演练",
            ),
        ];
        let (title, detail, target, label) = steps[self.guide_step];
        self.label(title, 14, 103, 620);
        self.label(detail, 14, 139, 620);
        self.button(target, label, 14, 178, 150);
        if self.guide_step > 0 {
            self.button(GUIDE_PREV, "上一步", 14, 404, 100);
        }
        if self.guide_step < 3 {
            self.button(GUIDE_NEXT, "下一步", 124, 404, 100);
        }
        self.button(NAV_HOME, "结束引导", 234, 404, 112);
    }

    fn show_panel(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }
        if self.editing || self.auxiliary_busy {
            self.status("请先完成当前操作。");
            return;
        }
        self.panel = panel;
        self.layout();
    }

    fn combo(&mut self, id: usize, title: &str, y: i32, items: &[&str], selected: usize) {
        self.label(title, 14, y + 3, 116);
        self.control(
            id,
            "COMBOBOX",
            "",
            (132, y, 390, 220),
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        );
        for item in items {
            self.send(id, CB_ADDSTRING, 0, wide(item).as_ptr() as isize);
        }
        self.send(id, CB_SETCURSEL, selected, 0);
    }

    fn settings_layout(&mut self) {
        let remote = self.config.engine.remote.clone();
        let volc_config = &self.config.engine.volc_bigmodel;
        let volc = self.config.engine.current == "volc_bigmodel"
            && (!volc_config.api_key.is_empty() || !volc_config.access_key.is_empty());
        let key = if volc {
            volc_config.api_key.clone()
        } else {
            remote.api_key
        };
        self.combo(
            PROVIDER,
            "识别服务",
            60,
            &["外部 ASR", "火山引擎"],
            usize::from(volc),
        );
        self.label("服务地址", 14, 103, 110);
        self.control(
            ADDRESS,
            "EDIT",
            &remote.address,
            (132, 100, 510, 26),
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
        );
        self.label("转写模型", 14, 139, 110);
        self.control(
            MODEL,
            "COMBOBOX",
            "",
            (132, 136, 390, 220),
            WS_TABSTOP | WS_VSCROLL | (CBS_DROPDOWN | CBS_AUTOHSCROLL) as u32,
        );
        self.set(MODEL, &remote.model);
        self.control(
            FETCH_MODELS,
            "BUTTON",
            "选择模型",
            (532, 136, 110, 26),
            WS_TABSTOP,
        );
        self.enable_model_fetch(!volc);
        self.label("API Key", 14, 175, 110);
        self.control(
            KEY,
            "EDIT",
            &key,
            (132, 172, 510, 26),
            WS_TABSTOP | (ES_AUTOHSCROLL | ES_PASSWORD) as u32,
        );
        self.combo(DEVICE, "麦克风", 212, &["系统默认"], 0);
        self.fill_devices();
        let language = LANGUAGES
            .iter()
            .position(|&(id, _)| id == self.config.core.asr.language)
            .unwrap_or(0);
        self.combo(
            LANGUAGE,
            "识别语言",
            249,
            &LANGUAGES.map(|(_, label)| label),
            language,
        );
        self.label("全局快捷键", 14, 289, 110);
        self.control(HOTKEY, "BUTTON", "", (132, 285, 390, 28), WS_TABSTOP);
        self.control(
            CANCEL_HOTKEY,
            "BUTTON",
            "取消",
            (532, 285, 110, 28),
            WS_TABSTOP,
        );
        self.control(
            HOTKEY_HINT,
            "STATIC",
            "按一次开始听写，再按一次结束。",
            (132, 316, 510, 23),
            0,
        );
        self.show_shortcut();
        self.control(
            FLOAT,
            "BUTTON",
            "显示桌面悬浮按钮",
            (132, 347, 245, 26),
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        self.send(
            FLOAT,
            BM_SETCHECK,
            usize::from(self.config.desktop.windows_floating_button),
            0,
        );
        self.button(DETECT, "保存识别服务", 14, 405, 150);
        self.button(CHECK, "测试连接", 174, 405, 108);
        self.button(PREFERENCES, "保存偏好", 292, 405, 114);
    }

    fn fill_devices(&self) {
        if self.panel != Panel::Settings {
            return;
        }
        self.send(DEVICE, CB_RESETCONTENT, 0, 0);
        self.send(DEVICE, CB_ADDSTRING, 0, wide("系统默认").as_ptr() as isize);
        let mut selected = 0;
        for (index, (id, name)) in self.devices.iter().enumerate() {
            self.send(DEVICE, CB_ADDSTRING, 0, wide(name).as_ptr() as isize);
            if self.config.core.asr.audio_device_name.as_deref() == Some(name)
                || (self.config.core.asr.audio_device_name.is_none()
                    && self.config.core.asr.audio_device == Some(*id))
            {
                selected = index + 1;
            }
        }
        self.send(DEVICE, CB_SETCURSEL, selected, 0);
    }

    fn fill_history(&mut self) {
        self.refresh_home();
        if self.panel != Panel::History || self.editing {
            return;
        }
        self.send(LIST, LB_RESETCONTENT, 0, 0);
        let query = self.history_query.trim().to_lowercase();
        self.history_indices.clear();
        for (index, (_, label, text)) in self.rows.iter().enumerate() {
            if query.is_empty()
                || label.to_lowercase().contains(&query)
                || text.to_lowercase().contains(&query)
            {
                self.send(LIST, LB_ADDSTRING, 0, wide(label).as_ptr() as isize);
                self.history_indices.push(index);
            }
        }
        self.selected = self
            .history_indices
            .iter()
            .copied()
            .find(|&index| index == self.selected)
            .or_else(|| self.history_indices.first().copied())
            .unwrap_or(usize::MAX);
        self.select_visible_row();
        self.show_body();
    }

    fn select_visible_row(&self) {
        let index = self
            .history_indices
            .iter()
            .position(|&index| index == self.selected);
        self.send(LIST, LB_SETCURSEL, index.unwrap_or(usize::MAX), 0);
    }

    fn select(&mut self) {
        if self.editing {
            self.select_visible_row();
            return;
        }
        let selected = self.send(LIST, LB_GETCURSEL, 0, 0);
        if selected >= 0 {
            if let Some(&index) = self.history_indices.get(selected as usize) {
                self.selected = index;
            }
            self.organized_visible = false;
            self.show_body();
        }
    }

    fn show_body(&self) {
        let text = self.rows.get(self.selected).map_or("", |(_, _, text)| text);
        let organized = if self.organized_visible {
            self.selected_draft()
                .and_then(|path| drafts::list().into_iter().find(|entry| entry.path == path))
                .and_then(|entry| entry.organized)
        } else {
            None
        };
        let text = organized.as_ref().map_or(text, |item| item.text.as_str());
        self.set(BODY, &text.replace("\r\n", "\n").replace('\n', "\r\n"));
    }

    fn selected_draft(&self) -> Option<PathBuf> {
        match &self.rows.get(self.selected)?.0 {
            history::Target::Draft(path) => Some(path.clone()),
            _ => None,
        }
    }

    fn settings(&mut self, choose_model: bool) {
        if self.editing || self.auxiliary_busy {
            self.status("请先完成当前操作。");
            return;
        }
        self.open();
        if self.panel == Panel::Settings {
            if choose_model {
                self.choose_model();
            }
            return;
        }
        self.panel = Panel::Settings;
        self.layout();
        self.fetch_models(choose_model);
        let updates = self.updates.clone();
        thread::spawn(move || updates.send(Event::Devices(AudioRecorder::get_devices())));
    }

    fn dictionary(&mut self) {
        if self.editing || self.auxiliary_busy {
            return;
        }
        self.panel = Panel::Terms;
        self.auxiliary_busy = true;
        self.layout();
        let updates = self.updates.clone();
        thread::spawn(move || {
            updates.send(Event::Dictionary(
                terms::load()
                    .and_then(|source| terms::learned().map(|learned| (source, learned)))
                    .map_err(|error| error.to_string()),
            ))
        });
    }

    fn save_terms(&mut self) {
        if self.auxiliary_busy {
            return;
        }
        let source = self.text(DICTIONARY);
        let updates = self.updates.clone();
        self.auxiliary_busy = true;
        thread::spawn(move || {
            updates.send(Event::Aux(
                terms::save(&source)
                    .map(|_| "常用词已保存。".into())
                    .unwrap_or_else(|error| error.to_string()),
            ))
        });
    }

    fn selected_shortcut(&self) -> Shortcut {
        self.config
            .core
            .asr
            .windows_toggle_hotkey
            .as_deref()
            .and_then(Shortcut::from_id)
            .unwrap_or_default()
    }

    fn show_shortcut(&self) {
        let capturing = self.shortcut_capture.is_some();
        self.set(
            HOTKEY,
            &if capturing {
                "请按快捷键…".into()
            } else {
                format!("{} · 更改", self.selected_shortcut().label())
            },
        );
        self.set(
            HOTKEY_HINT,
            if capturing {
                "按下任意单键或组合键，松开后保存。"
            } else if self.hotkey.is_none() {
                "快捷键未启用，请点击更改后重新设置。"
            } else {
                "按一次开始听写，再按一次结束。"
            },
        );
        unsafe {
            ShowWindow(
                self.child(CANCEL_HOTKEY),
                if capturing { SW_SHOWNA } else { SW_HIDE },
            );
        }
    }

    fn begin_shortcut_capture(&mut self) {
        if self.shortcut_capture.is_some() {
            return;
        }
        if self.starting
            || self.stop.is_some()
            || self.working
            || self.saving_audio
            || self.unsaved.is_some()
            || self.auxiliary_busy
            || self.editing
            || self.exiting
        {
            self.status("请等待录音、转写和保存完成后再更改快捷键。");
            return;
        }
        let previous = self.selected_shortcut();
        if let Some((handle, _)) = &mut self.hotkey {
            handle.suspend();
        }
        self.shortcut_capture = Some(ShortcutCapture {
            keys: ShortcutKeys::start(),
            previous,
        });
        self.show_shortcut();
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(self.child(HOTKEY));
        }
        self.status("按下任意单键或组合键，松开后保存；点击「取消」保留原快捷键。");
    }

    fn restore_shortcut_capture(&mut self) -> Option<String> {
        let capture = self.shortcut_capture.take()?;
        let error = self
            .hotkey
            .as_mut()
            .and_then(|(handle, _)| handle.resume(capture.previous).err());
        if error.is_some() {
            self.hotkey = None;
        }
        self.show_shortcut();
        error.map(|error| format!("原快捷键 {} 恢复失败：{error}", capture.previous.label()))
    }

    fn cancel_shortcut_capture(&mut self) {
        let Some(previous) = self
            .shortcut_capture
            .as_ref()
            .map(|capture| capture.previous)
        else {
            return;
        };
        let error = self.restore_shortcut_capture();
        self.status(
            error.unwrap_or_else(|| format!("已取消修改，保留快捷键 {}。", previous.label())),
        );
    }

    fn save_shortcut(&mut self, shortcut: Shortcut) {
        // Register and persist before replacing either the active key or in-memory config.
        // Other preference controls may contain unsubmitted edits; leave those alone.
        let mut config = self.config.clone();
        config.core.asr.windows_toggle_hotkey = Some(shortcut.id());
        let save = || config.save().map_err(|error| error.to_string());
        let result = if let Some((handle, _)) = &mut self.hotkey {
            handle.change(shortcut, save)
        } else {
            GlobalHotkey::register(shortcut).and_then(|registration| {
                save()?;
                self.hotkey = Some(registration);
                Ok(())
            })
        };
        match result {
            Ok(()) => {
                self.config = config;
                self.shortcut_capture = None;
                self.show_shortcut();
                self.status(format!("全局快捷键已保存：{}。", shortcut.label()));
            }
            Err(error) => {
                let restore_error = self.restore_shortcut_capture();
                self.status(match restore_error {
                    Some(restore_error) => format!("{error}；{restore_error}"),
                    None => format!("{error}；已保留原快捷键。"),
                });
            }
        }
    }

    fn capture_shortcut(&mut self, message: &MSG) -> bool {
        if self.shortcut_capture.is_none() {
            return false;
        }
        let belongs = message.hwnd == self.hwnd || unsafe { IsChild(self.hwnd, message.hwnd) != 0 };
        if belongs && hotkey_capture::is_keyboard_message(message.message) {
            let event = self
                .shortcut_capture
                .as_mut()
                .and_then(|capture| capture.keys.message(message));
            match event {
                Some(ShortcutEvent::Preview(shortcut)) => {
                    self.set(
                        HOTKEY,
                        &shortcut.map_or_else(|| "请按快捷键…".into(), Shortcut::label),
                    );
                    self.set(
                        HOTKEY_HINT,
                        &shortcut.map_or_else(
                            || "按下任意单键或组合键，松开后保存。".into(),
                            |shortcut| format!("松开 {} 后保存。", shortcut.label()),
                        ),
                    );
                }
                Some(ShortcutEvent::Chosen(shortcut)) => self.save_shortcut(shortcut),
                None => {}
            }
            return true;
        }
        if matches!(
            message.message,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_NCLBUTTONDOWN
                | WM_NCRBUTTONDOWN
        ) && message.hwnd != self.child(HOTKEY)
            && message.hwnd != self.child(CANCEL_HOTKEY)
        {
            self.cancel_shortcut_capture();
        }
        false
    }

    fn save_preferences(&mut self) {
        if self.auxiliary_busy {
            return;
        }
        let shortcut = self.selected_shortcut();
        let index = self.send(DEVICE, CB_GETCURSEL, 0, 0);
        let device = if index > 0 {
            self.devices.get(index as usize - 1)
        } else {
            None
        };
        let mut config = self.config.clone();
        config.core.asr.audio_device = device.map(|&(id, _)| id);
        config.core.asr.audio_device_name = device.map(|(_, name)| name.clone());
        config.core.asr.language = LANGUAGES
            .get(self.send(LANGUAGE, CB_GETCURSEL, 0, 0) as usize)
            .unwrap_or(&LANGUAGES[0])
            .0
            .into();
        config.core.asr.windows_toggle_hotkey = Some(shortcut.id());
        config.desktop.windows_floating_button = self.send(FLOAT, BM_GETCHECK, 0, 0) == 1;
        let save = || config.save().map_err(|error| error.to_string());
        let result = if let Some((handle, _)) = &mut self.hotkey {
            handle.change(shortcut, save)
        } else {
            GlobalHotkey::register(shortcut).and_then(|registration| {
                save()?;
                self.hotkey = Some(registration);
                Ok(())
            })
        };
        match result {
            Ok(()) => {
                self.config = config;
                self.sync_floating();
                self.status("偏好已保存。");
            }
            Err(error) => self.status(error),
        }
    }

    fn is_volc(&self) -> bool {
        self.send(PROVIDER, CB_GETCURSEL, 0, 0) == 1
    }

    fn provider_changed(&mut self) {
        let key = if self.is_volc() {
            &self.config.engine.volc_bigmodel.api_key
        } else {
            &self.config.engine.remote.api_key
        };
        self.set(KEY, key);
        self.clear_models();
        self.fetch_models(false);
    }

    fn choose_model(&mut self) {
        if self.model_catalog.is_some() {
            self.send(MODEL, CB_SHOWDROPDOWN, 1, 0);
        } else {
            self.fetch_models(true);
        }
    }

    fn enable_model_fetch(&self, enabled: bool) {
        unsafe { EnableWindow(self.child(FETCH_MODELS), i32::from(enabled)) };
    }

    fn fill_models(&self, models: &[String]) {
        let value = self.text(MODEL);
        self.send(MODEL, CB_RESETCONTENT, 0, 0);
        for model in models {
            self.send(MODEL, CB_ADDSTRING, 0, wide(model).as_ptr() as isize);
        }
        if let Some(index) = models.iter().position(|model| *model == value) {
            self.send(MODEL, CB_SETCURSEL, index, 0);
        } else {
            self.set(MODEL, &value);
        }
    }

    fn clear_models(&mut self) {
        self.model_catalog_request += 1;
        self.model_catalog = None;
        self.loading_model_catalog = false;
        self.fill_models(&[]);
        self.enable_model_fetch(!self.is_volc());
        if self.status == LOADING_MODELS {
            self.status("服务设置已修改，可重新获取模型。");
        }
    }

    fn service_key(&mut self, address: &str, volc: bool, announce: bool) -> Option<String> {
        let key = self.text(KEY).trim().to_owned();
        if !volc
            && address != self.config.engine.remote.address
            && !key.is_empty()
            && key == self.config.engine.remote.api_key
        {
            if announce {
                self.set(KEY, "");
                self.clear_models();
                self.status("服务地址已改变，请填写该服务的密钥。");
            }
            return None;
        }
        Some(key)
    }

    fn fetch_models(&mut self, announce: bool) {
        if self.panel != Panel::Settings || self.is_volc() || self.auxiliary_busy {
            return;
        }
        let address = self.text(ADDRESS).trim().to_owned();
        if address.is_empty() {
            if announce {
                self.status("请先填写服务地址。");
            }
            return;
        }
        let Some(key) = self.service_key(&address, false, announce) else {
            return;
        };
        if announce {
            self.status(LOADING_MODELS);
        }
        if self.loading_model_catalog {
            return;
        }
        self.loading_model_catalog = true;
        self.model_catalog_request += 1;
        let request = self.model_catalog_request;
        self.enable_model_fetch(false);
        let updates = self.updates.clone();
        thread::spawn(move || {
            let result = engines::list_models(&address, &key);
            updates.send(Event::ModelCatalog(request, result));
        });
    }

    fn check_service(&mut self, save: bool) {
        if self.auxiliary_busy {
            return;
        }
        let address = self.text(ADDRESS).trim().to_owned();
        let model = self.text(MODEL).trim().to_owned();
        let volc = self.is_volc();
        let Some(key) = self.service_key(&address, volc, true) else {
            return;
        };
        let catalog_endpoint = if volc {
            None
        } else if let Some(catalog) = &self.model_catalog {
            if model.is_empty() {
                self.status("请从下拉框选择要使用的模型。");
                return;
            }
            catalog
                .models
                .contains(&model)
                .then(|| catalog.endpoint.clone())
        } else {
            None
        };
        let mut config = self.config.clone();
        let updates = self.updates.clone();
        self.auxiliary_busy = true;
        self.status("正在检测识别服务…");
        thread::spawn(move || {
            let result = (|| {
                let text = if volc {
                    config.engine.current = "volc_bigmodel".into();
                    config.engine.volc_bigmodel.api_key = key;
                    let engine = engines::recognition_engine(&config)?;
                    engine
                        .transcribe(SAMPLE_AUDIO, "zh")
                        .map_err(|error| error.to_string())?
                } else if !save
                    && address == config.engine.remote.address
                    && model == config.engine.remote.model
                    && key == config.engine.remote.api_key
                {
                    engines::check_service(&config.engine.remote)?
                } else {
                    let (service, text) = if let Some(endpoint) = catalog_endpoint {
                        let service = RemoteService {
                            address,
                            endpoint,
                            protocol: engines::Protocol::OpenAI,
                            model,
                            api_key: key,
                        };
                        let text = engines::check_service(&service)?;
                        (service, text)
                    } else {
                        engines::detect_service(&address, &model, &key)?
                    };
                    config.engine.current = "remote".into();
                    config.engine.remote = service;
                    text
                };
                if save {
                    config.save().map_err(|error| error.to_string())?;
                }
                Ok((
                    config,
                    format!(
                        "{}：{text}",
                        if save {
                            "识别服务已保存"
                        } else {
                            "连接成功"
                        }
                    ),
                ))
            })();
            if save {
                updates.send(Event::Settings(result));
            } else {
                updates.send(Event::Aux(
                    result
                        .map(|(_, message)| message)
                        .unwrap_or_else(|error: String| error),
                ));
            }
        });
    }

    fn command(&mut self, id: usize) {
        if !matches!(id, HOTKEY | CANCEL_HOTKEY) {
            self.cancel_shortcut_capture();
        }
        if self.diagnostics_command(id) || self.benchmark_command(id) {
            return;
        }
        match id {
            HOTKEY => self.begin_shortcut_capture(),
            CANCEL_HOTKEY => self.cancel_shortcut_capture(),
            RECORD => self.toggle(false, None),
            SETTINGS => self.settings(false),
            NAV_MODELS | HOME_MODELS => self.settings(true),
            TERMS => self.dictionary(),
            NAV_HOME => self.show_panel(Panel::Home),
            NAV_HISTORY => self.show_panel(Panel::History),
            NAV_FILES => self.show_panel(Panel::Files),
            NAV_GUIDE => self.show_panel(Panel::Guide),
            NAV_DIAGNOSTICS => self.show_panel(Panel::Diagnostics),
            NAV_ORGANIZER => self.show_panel(Panel::Organizer),
            NAV_BENCHMARK => self.show_panel(Panel::Benchmark),
            GUIDE_NEXT | GUIDE_PREV if self.panel == Panel::Guide => {
                self.guide_step = if id == GUIDE_NEXT {
                    (self.guide_step + 1).min(3)
                } else {
                    self.guide_step.saturating_sub(1)
                };
                self.layout();
            }
            IMPORT => self.import_file(),
            EXPORT if !self.editing => {
                let text = self.text(BODY);
                if text.trim().is_empty() {
                    self.status("请先选择有转写的录音。");
                } else {
                    match files::export_text(self.hwnd, &text) {
                        Ok(true) => self.status("文字已导出。"),
                        Ok(false) => {}
                        Err(error) => self.status(error),
                    }
                }
            }
            ORGANIZED if !self.editing => {
                if self.organized_visible {
                    self.organized_visible = false;
                    self.show_body();
                    self.status("已显示 ASR 原文。");
                } else if self
                    .selected_draft()
                    .and_then(|path| drafts::list().into_iter().find(|entry| entry.path == path))
                    .is_some_and(|entry| entry.organized.is_some())
                {
                    self.organized_visible = true;
                    self.show_body();
                    self.status("已显示整理稿。");
                } else {
                    self.status("这条录音还没有整理稿。");
                }
            }
            THEME if !self.auxiliary_busy && !self.editing => {
                let mut config = self.config.clone();
                config.appearance.theme = if config.appearance.theme == "light" {
                    "dark"
                } else {
                    "light"
                }
                .into();
                match config.save() {
                    Ok(()) => {
                        theme::set_palette(&config.appearance.theme);
                        self.config = config;
                        // Repaint without recreating controls or discarding unsaved settings.
                        let label = if self.config.appearance.theme == "light" {
                            "深色主题"
                        } else {
                            "浅色主题"
                        };
                        self.set(THEME, label);
                        unsafe {
                            RedrawWindow(
                                self.hwnd,
                                null(),
                                null_mut(),
                                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                            );
                        }
                    }
                    Err(error) => self.status(error.to_string()),
                }
            }
            FORGET_TERM if !self.auxiliary_busy => {
                let word = self.text(TERM_TO_FORGET);
                if !word.trim().is_empty() {
                    match terms::forget_learned(word.trim()).and_then(|_| terms::learned()) {
                        Ok(learned) => {
                            self.set(LEARNED, &learned.join("\r\n"));
                            self.set(TERM_TO_FORGET, "");
                            self.status("已移除学习词。");
                        }
                        Err(error) => self.status(error.to_string()),
                    }
                }
            }
            tray::OPEN => self.open(),
            tray::EXIT => {
                if self.editing {
                    self.status("请先保存当前改词。");
                    return;
                }
                if self.unsaved.is_some() {
                    self.status("录音尚未保存，请重试保存后退出。");
                    return;
                }
                self.exiting = true;
                if self.stop.is_some() || self.starting {
                    self.toggle(false, None);
                } else if !self.working && !self.auxiliary_busy {
                    unsafe {
                        PostQuitMessage(0);
                    }
                } else {
                    self.status("正在保存结果，完成后退出。");
                }
            }
            tray::TOGGLE_FLOAT if !self.auxiliary_busy => {
                self.config.desktop.windows_floating_button =
                    !self.config.desktop.windows_floating_button;
                match self.config.save() {
                    Ok(()) => self.sync_floating(),
                    Err(error) => {
                        self.config.desktop.windows_floating_button =
                            !self.config.desktop.windows_floating_button;
                        self.status(error.to_string());
                    }
                }
            }
            BACK if !self.auxiliary_busy => {
                self.show_panel(Panel::History);
            }
            DETECT => self.check_service(true),
            FETCH_MODELS => self.choose_model(),
            CHECK => self.check_service(false),
            PREFERENCES => self.save_preferences(),
            SAVE_TERMS => self.save_terms(),
            COPY => {
                let (mut start, mut end) = (0_u32, 0_u32);
                self.send(
                    BODY,
                    EM_GETSEL,
                    &mut start as *mut u32 as usize,
                    &mut end as *mut u32 as isize,
                );
                if start == end {
                    self.send(BODY, EM_SETSEL, 0, -1);
                }
                self.send(BODY, WM_COPY, 0, 0);
                if start == end {
                    self.send(BODY, EM_SETSEL, start as usize, end as isize);
                }
            }
            EDIT if !self.working
                && self.stop.is_none()
                && !self.starting
                && self
                    .rows
                    .get(self.selected)
                    .is_some_and(|(_, _, text)| !text.is_empty()) =>
            {
                self.organized_visible = false;
                self.show_body();
                self.editing = true;
                self.send(BODY, EM_SETREADONLY, 0, 0);
                unsafe {
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(self.child(BODY));
                }
                self.status("编辑正文后点击「保存改词」。");
            }
            SAVE_EDIT if self.editing => self.save_edit(),
            PLAY | RESUME | TRANSCRIBE => self.audio_action(id),
            _ => {}
        }
    }

    fn save_edit(&mut self) {
        let Some((target, _, original)) = self.rows.get(self.selected).cloned() else {
            return;
        };
        let edited = self.text(BODY).replace("\r\n", "\n");
        self.editing = false;
        self.send(BODY, EM_SETREADONLY, 1, 0);
        self.working = true;
        let updates = self.updates.clone();
        thread::spawn(move || {
            let message = match history::edit(&target, &original, &edited) {
                Ok(()) => match terms::learn_edits_with_selection(&original, &edited, None) {
                    Ok(words) if !words.is_empty() => {
                        format!("改词已保存，自动学习：{}", words.join("、"))
                    }
                    Ok(_) => "改词已保存。".into(),
                    Err(error) => format!("改词已保存，学习词条失败：{error}"),
                },
                Err(error) => {
                    updates.send(Event::EditFailed(edited, error.to_string()));
                    return;
                }
            };
            updates.send(Event::Done(message, app::rows()));
        });
    }

    fn audio_action(&mut self, id: usize) {
        if self.editing
            || self.working
            || self.starting
            || self.stop.is_some()
            || self.unsaved.is_some()
        {
            return;
        }
        if id == PLAY && self.play.take().is_some() {
            self.status("已停止回放。");
            return;
        }
        let Some((history::Target::Draft(path), _, _)) = self.rows.get(self.selected) else {
            self.status("这条历史没有可用录音。");
            return;
        };
        let path = path.clone();
        match id {
            PLAY => match Playback::start(&path) {
                Ok(playback) => {
                    self.play = Some(playback);
                    self.status("正在回放。");
                }
                Err(error) => self.status(format!("回放失败：{error}")),
            },
            RESUME => self.toggle(false, Some(path)),
            TRANSCRIBE => self.retranscribe(path),
            _ => {}
        }
    }

    fn retranscribe(&mut self, path: PathBuf) {
        self.retry_transcription = None;
        self.working = true;
        self.status("正在转写…");
        app::retranscribe(self.config.clone(), path, self.updates.clone());
    }

    fn toggle(&mut self, external: bool, path: Option<PathBuf>) {
        if self.shortcut_capture.is_some() {
            return;
        }
        if self.editing {
            self.status("请先保存当前改词。");
            return;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(app::StopRequest::new(false));
            self.working = true;
            self.saving_audio = true;
            self.overlay.take();
            self.status("正在保存录音…");
        } else if self.starting {
            self.stop_after_start = true;
            if self.startup_stop.is_none() {
                self.startup_stop = Some(app::StopRequest::during_startup());
            }
        } else if let Some(pending) = self.unsaved.take() {
            self.working = true;
            self.saving_audio = true;
            self.status("正在保存录音…");
            app::retry(pending, self.updates.clone());
        } else if !self.working && !self.exiting {
            self.play.take();
            self.retry_transcription = None;
            self.starting = true;
            self.elapsed = 0;
            self.activity = VoiceActivity {
                level: 0.,
                pitch_hz: None,
            };
            self.caption.clear();
            self.status("正在打开麦克风…");
            app::start(self.config.clone(), path, external, self.updates.clone());
        }
    }

    fn update(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            let Some(event) = self.diagnostics_event(event) else {
                continue;
            };
            match event {
                Event::Imported(result) => match result {
                    Ok(path) => {
                        self.history_query.clear();
                        self.panel = Panel::History;
                        self.layout();
                        self.retranscribe(path);
                    }
                    Err(error) => {
                        self.working = false;
                        self.status(error);
                    }
                },
                Event::ClipboardNotice => {
                    if let Some(graphics) = &self.graphics {
                        if let Ok(layer) =
                            Layer::new(self.hwnd, graphics, 470, 70, "win7-clipboard-position")
                        {
                            self.clipboard_notice = Some((layer, Instant::now()));
                        }
                    }
                }
                Event::Started(stop) => {
                    self.starting = false;
                    self.stop = Some(stop);
                    self.escape_down = speaky_core::hotkey::escape_pressed();
                    if std::mem::take(&mut self.stop_after_start) {
                        if std::mem::take(&mut self.cancel_after_start) {
                            self.cancel();
                        } else {
                            if let Some(stop) = self.stop.take() {
                                let request = self
                                    .startup_stop
                                    .take()
                                    .unwrap_or_else(|| app::StopRequest::new(false));
                                let _ = stop.send(request);
                                self.working = true;
                                self.saving_audio = true;
                                self.status("正在保存录音…");
                            }
                        }
                    } else {
                        self.status("录音中…");
                        if let Some(graphics) = &self.graphics {
                            match Overlay::new(self.hwnd, graphics) {
                                Ok(overlay) => {
                                    self.overlay = Some(overlay);
                                    self.render();
                                }
                                Err(error) => {
                                    self.status(format!("录音中，悬浮窗无法打开：{error}"))
                                }
                            }
                        }
                    }
                }
                Event::Meter(activity, seconds, caption) if self.stop.is_some() => {
                    self.activity = activity;
                    self.elapsed = seconds;
                    if self.panel == Panel::Home {
                        self.send(
                            INPUT_LEVEL,
                            PBM_SETPOS,
                            (activity.level.clamp(0., 1.) * 100.) as usize,
                            0,
                        );
                        if self.caption != caption {
                            self.set(LIVE_TEXT, &caption);
                        }
                    }
                    self.caption = caption;
                }
                Event::Meter(..) => {}
                Event::Saved(rows) => {
                    self.saving_audio = false;
                    self.selected = 0;
                    self.rows = rows;
                    self.fill_history();
                    self.status("录音已保存，正在转写…");
                }
                Event::History(rows) => {
                    self.rows = rows;
                    self.fill_history();
                }
                Event::Done(status, rows) => {
                    self.saving_audio = false;
                    self.starting = false;
                    self.stop_after_start = false;
                    self.startup_stop = None;
                    self.working = false;
                    self.selected = 0;
                    self.organized_visible = false;
                    self.rows = rows;
                    self.fill_history();
                    self.status(status);
                }
                Event::TranscriptionFailed(path, status, rows) => {
                    self.working = false;
                    self.selected = rows
                        .iter()
                        .position(|(target, _, _)| matches!(target, history::Target::Draft(draft) if draft == &path))
                        .unwrap_or(0);
                    self.retry_transcription = Some(path);
                    self.rows = rows;
                    self.fill_history();
                    self.status(status);
                }
                Event::EditFailed(edited, error) => {
                    self.working = false;
                    self.exiting = false;
                    self.panel = Panel::History;
                    self.layout();
                    self.editing = true;
                    self.send(BODY, EM_SETREADONLY, 0, 0);
                    self.set(BODY, &edited.replace('\n', "\r\n"));
                    self.status(format!("保存改词失败：{error}"));
                }
                Event::Unsaved(pending, error) => {
                    self.saving_audio = false;
                    self.working = false;
                    self.exiting = false;
                    self.unsaved = Some(pending);
                    self.status(error);
                    self.open();
                }
                Event::Settings(result) => {
                    self.auxiliary_busy = false;
                    match result {
                        Ok((config, message)) => {
                            self.config = config;
                            if self.panel == Panel::Settings {
                                self.layout();
                                self.fetch_models(false);
                            }
                            self.status(message);
                        }
                        Err(error) => self.status(error),
                    }
                }
                Event::Aux(message) => {
                    self.auxiliary_busy = false;
                    if !self.benchmark_report(&message) {
                        self.status(message);
                    }
                }
                Event::ModelCatalog(request, result) => {
                    if self.panel != Panel::Settings || request != self.model_catalog_request {
                        continue;
                    }
                    self.loading_model_catalog = false;
                    self.enable_model_fetch(true);
                    let message = match result {
                        Ok(catalog) => {
                            self.fill_models(&catalog.models);
                            self.model_catalog = Some(catalog);
                            if self.status == LOADING_MODELS
                                && unsafe { GetForegroundWindow() } == self.hwnd
                            {
                                self.send(MODEL, CB_SHOWDROPDOWN, 1, 0);
                            }
                            "请选择模型，然后测试连接或保存。".into()
                        }
                        Err(error) => error,
                    };
                    if self.status == LOADING_MODELS {
                        self.status(message);
                    }
                }
                Event::Devices(devices) => {
                    self.devices = devices;
                    self.fill_devices();
                }
                Event::Dictionary(result) => {
                    self.auxiliary_busy = false;
                    match result {
                        Ok((source, learned)) => {
                            self.set(
                                DICTIONARY,
                                &source.replace("\r\n", "\n").replace('\n', "\r\n"),
                            );
                            self.set(LEARNED, &learned.join("\r\n"));
                        }
                        Err(error) => self.status(error),
                    }
                }
                _ => {}
            }
        }
        if self.exiting
            && !self.starting
            && !self.working
            && !self.auxiliary_busy
            && self.stop.is_none()
        {
            unsafe {
                PostQuitMessage(0);
            }
        }
    }

    fn sync_floating(&mut self) {
        if self.config.desktop.windows_floating_button {
            if self.floating.is_none() {
                if let Some(graphics) = &self.graphics {
                    match floating::create(self.hwnd, graphics) {
                        Ok(layer) => self.floating = Some(layer),
                        Err(error) => self.status(format!("悬浮按钮无法打开：{error}")),
                    }
                }
            }
        } else {
            self.floating.take();
        }
        self.floating_dirty = true;
        self.render();
    }

    fn layer_click(&mut self) {
        if self.stop.is_none()
            && !self.starting
            && !self.working
            && !self.editing
            && !self.exiting
            && self.unsaved.is_none()
        {
            if let Some(path) = self.retry_transcription.take() {
                self.retranscribe(path);
                return;
            }
        }
        self.toggle(true, None);
    }

    fn cancel(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(app::StopRequest::new(true));
            self.working = true;
            self.saving_audio = true;
            self.overlay.take();
            self.caption.clear();
            self.status("已取消转写，正在保存录音…");
        } else if self.starting {
            self.stop_after_start = true;
            self.cancel_after_start = true;
        }
    }

    fn timer(&mut self) {
        if self.shortcut_capture.is_some()
            && unsafe {
                GetForegroundWindow() != self.hwnd
                    || windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus()
                        != self.child(HOTKEY)
            }
        {
            self.cancel_shortcut_capture();
        }
        let down = speaky_core::hotkey::escape_pressed();
        if down && !self.escape_down && (self.stop.is_some() || self.starting) {
            self.cancel();
        }
        self.escape_down = down;
        if self
            .clipboard_notice
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed().as_secs() >= 8)
        {
            self.clipboard_notice.take();
        }
        let overlay_click = self
            .overlay
            .as_mut()
            .is_some_and(|overlay| overlay.layer.pointer_update(false, true));
        let floating_click = self
            .floating
            .as_mut()
            .is_some_and(|layer| layer.pointer_update(false, false));
        if overlay_click || floating_click {
            let cancel = self.stop.is_some()
                && ((overlay_click && self.overlay.as_ref().is_some_and(|o| o.layer.cancel_hit()))
                    || (floating_click && self.floating.as_ref().is_some_and(Layer::cancel_hit)));
            if cancel {
                self.cancel();
            } else {
                self.layer_click();
            }
        }
        let presses = self.hotkey.as_mut().map_or(0, |(_, events)| {
            let mut count = 0;
            while events.try_recv() {
                count += 1;
            }
            count
        });
        if self.shortcut_capture.is_none() {
            for _ in 0..presses {
                self.toggle(true, None);
            }
        }
        if let Some(play) = &mut self.play {
            match play.is_finished() {
                Ok(false) => {}
                result => {
                    self.play.take();
                    self.status(result.map_or_else(
                        |error| format!("回放失败：{error}"),
                        |_| "回放完毕。".into(),
                    ));
                }
            }
        }
        self.render();
    }

    fn render(&mut self) {
        let Some(graphics) = &self.graphics else {
            return;
        };
        let mut period = if self.stop.is_some() { 16 } else { 32 };
        if let Some((layer, _)) = &mut self.clipboard_notice {
            let _ = layer.surface.draw(layer.hwnd, |target| {
                d2d::fill(target, d2d::rect(0., 0., 470., 70.), 16., 0x25334a, 0.96)?;
                graphics.text(
                    target,
                    "语音识别内容已保存到剪贴板，请手动粘贴",
                    d2d::rect(18., 22., 458., 58.),
                )
            });
            layer.show(true);
        }
        if let Some(overlay) = &mut self.overlay {
            match overlay.render(graphics, self.activity, self.elapsed, &self.caption) {
                Ok(rate) => {
                    period = rate;
                    overlay.layer.show(true);
                }
                Err(error) => {
                    self.overlay.take();
                    self.status(format!("悬浮窗绘制失败：{error}"));
                    return;
                }
            }
        }
        if let Some(layer) = &mut self.floating {
            let recording = self
                .stop
                .is_some()
                .then_some((self.activity.level, self.elapsed));
            if self.floating_dirty || recording.is_some() {
                let status = if self.unsaved.is_some() {
                    "重试保存"
                } else if self.working {
                    "转写中…"
                } else if self.starting {
                    "启动中…"
                } else if self.retry_transcription.is_some() {
                    "重试转写"
                } else {
                    "听写"
                };
                if floating::render(
                    layer,
                    graphics,
                    recording,
                    self.clock.elapsed().as_secs_f32(),
                    status,
                )
                .is_ok()
                {
                    layer.show(true);
                }
                self.floating_dirty = false;
            }
        }
        if self.period != period {
            self.period = period;
            unsafe {
                SetTimer(self.hwnd, 1, period, None);
            }
        }
    }
}
