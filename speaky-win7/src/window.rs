use crate::{
    app::{self, Event, Updates},
    d2d::{self, Graphics},
    floating,
    overlay::{Layer, Overlay},
    tray,
};
use speaky_core::{
    audio::{AudioRecorder, VoiceActivity},
    config::Config,
    engines, history,
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
            InitCommonControlsEx, EM_GETSEL, EM_SETREADONLY, EM_SETSEL, ICC_STANDARD_CLASSES,
            INITCOMMONCONTROLSEX,
        },
        WindowsAndMessaging::*,
    },
};

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

thread_local! {
    static STATE: RefCell<Option<Window>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    History,
    Settings,
    Terms,
}

struct Window {
    hwnd: HWND,
    scale: f32,
    font: HFONT,
    config: Config,
    tray: tray::Tray,
    graphics: Option<Graphics>,
    overlay: Option<Overlay>,
    floating: Option<Layer>,
    hotkey: Option<(GlobalHotkey, HotkeyEvents)>,
    updates: Updates,
    events: Receiver<Event>,
    stop: Option<Sender<()>>,
    unsaved: Option<app::Pending>,
    retry_transcription: Option<PathBuf>,
    starting: bool,
    stop_after_start: bool,
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
    selected: usize,
    editing: bool,
    play: Option<Playback>,
    panel: Panel,
    controls: Vec<HWND>,
    devices: Vec<(u32, String)>,
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
            dwICC: ICC_STANDARD_CLASSES,
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
        let scale = d2d::system_scale();
        let mut bounds = windows_sys::Win32::Foundation::RECT {
            right: px(scale, 660),
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
            CW_USEDEFAULT,
            CW_USEDEFAULT,
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
            windows_sys::w!("Microsoft YaHei UI"),
        );
        let mut state = Window {
            hwnd,
            scale,
            font,
            config,
            tray: tray::Tray::new(hwnd),
            graphics,
            overlay: None,
            floating: None,
            hotkey,
            updates: updates.clone(),
            events,
            stop: None,
            unsaved: None,
            retry_transcription: None,
            starting: false,
            stop_after_start: false,
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
            selected: 0,
            editing: false,
            play: None,
            panel: Panel::History,
            controls: Vec::new(),
            devices: Vec::new(),
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
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        STATE.with(|slot| drop(slot.borrow_mut().take()));
        DestroyWindow(hwnd);
        DeleteObject(font);
        Ok(())
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
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
                } else if id == LIST {
                    if w >> 16 == LBN_SELCHANGE as usize {
                        state.select();
                    }
                } else {
                    state.command(id);
                }
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
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
                state.layer_click();
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
        self.control(id, "BUTTON", text, (x, y, w, 30), WS_TABSTOP);
    }

    fn label(&mut self, text: &str, x: i32, y: i32, w: i32) {
        self.control(0, "STATIC", text, (x, y, w, 23), 0);
    }

    fn child(&self, id: usize) -> HWND {
        unsafe { GetDlgItem(self.hwnd, id as i32) }
    }

    fn set(&self, id: usize, text: &str) {
        unsafe {
            SetWindowTextW(self.child(id), wide(text).as_ptr());
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
        for control in self.controls.drain(..) {
            unsafe {
                DestroyWindow(control);
            }
        }
        let status = self.status.clone();
        self.control(STATUS, "STATIC", &status, (14, 12, 630, 42), 0);
        if self.panel != Panel::History {
            self.button(BACK, "返回历史", 532, 59, 112);
        }
        match self.panel {
            Panel::History => {
                self.button(RECORD, self.record_label(), 14, 56, 125);
                self.button(SETTINGS, "设置服务", 148, 56, 106);
                self.button(TERMS, "常用词", 264, 56, 106);
                self.label("录音与历史", 14, 101, 200);
                self.label("转写正文", 260, 101, 200);
                self.control(
                    LIST,
                    "LISTBOX",
                    "",
                    (14, 128, 230, 264),
                    WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
                );
                self.control(
                    BODY,
                    "EDIT",
                    "",
                    (260, 128, 384, 264),
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
                    (14, 318, 630, 72),
                    WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                );
                self.button(SAVE_TERMS, "保存常用词", 14, 404, 130);
            }
        }
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
        for (id, label, value, y) in [
            (ADDRESS, "服务地址", remote.address, 100),
            (MODEL, "模型 ID", remote.model, 136),
            (KEY, "API Key", key, 172),
        ] {
            self.label(label, 14, y + 3, 110);
            self.control(
                id,
                "EDIT",
                &value,
                (132, y, 510, 26),
                WS_TABSTOP | (ES_AUTOHSCROLL | if id == KEY { ES_PASSWORD } else { 0 }) as u32,
            );
        }
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
        self.control(
            HOTKEY,
            "COMBOBOX",
            "",
            (132, 285, 390, 180),
            WS_TABSTOP | (CBS_DROPDOWN | CBS_AUTOHSCROLL) as u32,
        );
        for binding in [
            "control_shift_space",
            "control_shift_d",
            "key:0:f8",
            "key:0:vka5",
        ] {
            self.send(HOTKEY, CB_ADDSTRING, 0, wide(binding).as_ptr() as isize);
        }
        self.set(
            HOTKEY,
            self.config
                .core
                .asr
                .windows_toggle_hotkey
                .as_deref()
                .unwrap_or("control_shift_space"),
        );
        self.control(
            FLOAT,
            "BUTTON",
            "显示桌面悬浮按钮",
            (132, 324, 245, 26),
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        self.send(
            FLOAT,
            BM_SETCHECK,
            usize::from(self.config.desktop.windows_floating_button),
            0,
        );
        self.button(DETECT, "自动探测并保存", 14, 405, 150);
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
        if self.panel != Panel::History || self.editing {
            return;
        }
        self.send(LIST, LB_RESETCONTENT, 0, 0);
        for (_, label, _) in &self.rows {
            self.send(LIST, LB_ADDSTRING, 0, wide(label).as_ptr() as isize);
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.send(LIST, LB_SETCURSEL, self.selected, 0);
        self.show_body();
    }

    fn select(&mut self) {
        if self.editing {
            self.send(LIST, LB_SETCURSEL, self.selected, 0);
            return;
        }
        let selected = self.send(LIST, LB_GETCURSEL, 0, 0);
        if selected >= 0 {
            self.selected = selected as usize;
            self.show_body();
        }
    }

    fn show_body(&self) {
        let text = self.rows.get(self.selected).map_or("", |(_, _, text)| text);
        self.set(BODY, &text.replace("\r\n", "\n").replace('\n', "\r\n"));
    }

    fn settings(&mut self) {
        if self.editing {
            self.status("请先保存当前改词。");
            return;
        }
        self.open();
        if self.panel == Panel::Settings {
            return;
        }
        self.panel = Panel::Settings;
        self.layout();
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

    fn save_preferences(&mut self) {
        if self.auxiliary_busy {
            return;
        }
        let Some(shortcut) = Shortcut::from_id(self.text(HOTKEY).trim()) else {
            self.status("请选择预设，或填写有效的快捷键 ID。");
            return;
        };
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

    fn provider_changed(&self) {
        let key = if self.send(PROVIDER, CB_GETCURSEL, 0, 0) == 1 {
            &self.config.engine.volc_bigmodel.api_key
        } else {
            &self.config.engine.remote.api_key
        };
        self.set(KEY, key);
    }

    fn check_service(&mut self, save: bool) {
        if self.auxiliary_busy {
            return;
        }
        let address = self.text(ADDRESS).trim().to_owned();
        let model = self.text(MODEL).trim().to_owned();
        let key = self.text(KEY).trim().to_owned();
        let volc = self.send(PROVIDER, CB_GETCURSEL, 0, 0) == 1;
        let mut config = self.config.clone();
        if !volc
            && address != config.engine.remote.address
            && !key.is_empty()
            && key == config.engine.remote.api_key
        {
            self.set(KEY, "");
            self.status("服务地址已改变，请重新填写该服务的密钥。");
            return;
        }
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
                    let (service, text) = engines::detect_service(&address, &model, &key)?;
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
        match id {
            RECORD => self.toggle(false, None),
            SETTINGS => self.settings(),
            TERMS => self.dictionary(),
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
                self.panel = Panel::History;
                self.layout();
            }
            DETECT => self.check_service(true),
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
        if self.editing {
            self.status("请先保存当前改词。");
            return;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
            self.working = true;
            self.saving_audio = true;
            self.overlay.take();
            self.status("正在保存录音…");
        } else if self.starting {
            self.stop_after_start = true;
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
            match event {
                Event::Started(stop) => {
                    self.starting = false;
                    self.stop = Some(stop);
                    if std::mem::take(&mut self.stop_after_start) {
                        self.toggle(false, None);
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
                    self.working = false;
                    self.selected = 0;
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
                            }
                            self.status(message);
                        }
                        Err(error) => self.status(error),
                    }
                }
                Event::Aux(message) => {
                    self.auxiliary_busy = false;
                    self.status(message);
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

    fn timer(&mut self) {
        let overlay_click = self
            .overlay
            .as_mut()
            .is_some_and(|overlay| overlay.layer.pointer_update(false, true));
        let floating_click = self
            .floating
            .as_mut()
            .is_some_and(|layer| layer.pointer_update(false, false));
        if overlay_click || floating_click {
            self.layer_click();
        }
        let presses = self.hotkey.as_mut().map_or(0, |(_, events)| {
            let mut count = 0;
            while events.try_recv() {
                count += 1;
            }
            count
        });
        for _ in 0..presses {
            self.toggle(true, None);
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
