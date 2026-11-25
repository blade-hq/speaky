"""Windows regression checks. Every visible window and capture stays on a secondary monitor.

python scripts/qa-product-invariants.py --probe <dictation_probe.exe> [--app <speaky-gpui.exe>]
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import psutil

import win32api
import win32con as C
import win32gui as G
import win32clipboard as CB
import win32process
from PIL import ImageGrab

parser = argparse.ArgumentParser()
parser.add_argument("--probe", required=True, type=Path)
parser.add_argument("--app", type=Path)
parser.add_argument("--app-only", action="store_true")
parser.add_argument("--layout-only", action="store_true")
parser.add_argument("--out", type=Path, default=Path("target/product-qa/gui"))
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
u = ctypes.windll.user32
u.OpenInputDesktop.restype = ctypes.c_void_p
u.CloseDesktop.argtypes = [ctypes.c_void_p]
desktop = u.OpenInputDesktop(0, False, 0x101)
if not desktop:
    print(json.dumps({"skipped": "Interactive desktop is unavailable; unlock Windows before GUI verification"}))
    raise SystemExit(0)
u.CloseDesktop(desktop)
u.SetProcessDPIAware()
u.SetThreadDpiAwarenessContext.argtypes = [ctypes.c_void_p]
u.SetThreadDpiAwarenessContext.restype = ctypes.c_void_p
u.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
monitors = [win32api.GetMonitorInfo(h) for h, _, _ in win32api.EnumDisplayMonitors()]
secondary = next((m["Work"] for m in monitors if not m["Flags"] & 1), None)
if secondary is None:
    print(json.dumps({"skipped": "No secondary monitor"}))
    raise SystemExit(0)
sx, sy, ex, ey = secondary
checks = []
processes = []
windows = []
original_cursor = G.GetCursorPos()


def pump(seconds=.05):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        G.PumpWaitingMessages()
        time.sleep(.005)

def wait(predicate, timeout=15):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        value = predicate()
        if value:
            return value
        pump(.02)
    raise AssertionError("Timed out waiting for " + repr(predicate))

def verify(hwnd):
    left, top, right, bottom = G.GetWindowRect(hwnd)
    assert sx <= left < right <= ex and sy <= top < bottom <= ey, (G.GetWindowText(hwnd), (left, top, right, bottom), secondary)

def focus(parent, edit=None):
    verify(parent)
    if G.GetForegroundWindow() != parent:
        current = win32api.GetCurrentThreadId()
        foreground = win32process.GetWindowThreadProcessId(G.GetForegroundWindow())[0]
        attached = u.AttachThreadInput(current, foreground, True) if current != foreground else False
        try:
            G.BringWindowToTop(parent)
            u.SetForegroundWindow(parent)
        finally:
            if attached: u.AttachThreadInput(current, foreground, False)
        if G.GetForegroundWindow() != parent:
            verify(parent)
            left, top, _, _ = G.GetWindowRect(parent)
            G.SetWindowPos(parent, C.HWND_TOPMOST, 0, 0, 0, 0, C.SWP_NOMOVE|C.SWP_NOSIZE|C.SWP_NOACTIVATE)
            win32api.SetCursorPos((left+50, top+12))
            win32api.mouse_event(C.MOUSEEVENTF_LEFTDOWN, 0, 0)
            win32api.mouse_event(C.MOUSEEVENTF_LEFTUP, 0, 0)
            pump(.1)
            G.SetWindowPos(parent, C.HWND_NOTOPMOST, 0, 0, 0, 0, C.SWP_NOMOVE|C.SWP_NOSIZE|C.SWP_NOACTIVATE)
    if edit: G.SetFocus(edit)
    wait(lambda: G.GetForegroundWindow() == parent, 3)
    pump(.1)

def capture(hwnd, name):
    verify(hwnd)
    ImageGrab.grab(bbox=G.GetWindowRect(hwnd), all_screens=True).save(args.out / name)

def record(name):
    checks.append(name)
    print("PASS", name, flush=True)

input_host = None
def window(title, x):
    wc = G.WNDCLASS()
    wc.lpszClassName = title
    wc.lpfnWndProc = G.DefWindowProc
    wc.hInstance = win32api.GetModuleHandle(None)
    G.RegisterClass(wc)
    hwnd = G.CreateWindowEx(0, title, title, C.WS_OVERLAPPEDWINDOW, x, sy+40, 650, 230, 0, 0, wc.hInstance, None)
    windows.append(hwnd)
    edit = G.CreateWindowEx(C.WS_EX_CLIENTEDGE, "EDIT", "", C.WS_CHILD|C.WS_VISIBLE|C.ES_MULTILINE|C.ES_AUTOVSCROLL, 16, 18, 600, 145, hwnd, 101, wc.hInstance, None)
    verify(hwnd)
    G.ShowWindow(hwnd, C.SW_SHOWNOACTIVATE)
    return hwnd, edit

def text(edit, value, selection=None):
    G.SetWindowText(edit, value)
    start, end = selection or (len(value.encode("utf-16-le"))//2,)*2
    G.SendMessage(edit, C.EM_SETSEL, start, end)

def launch(exe, env=None):
    p = subprocess.Popen([str(exe.resolve())], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True, encoding="utf-8", env=env,
                         creationflags=subprocess.CREATE_NO_WINDOW)
    processes.append(p)
    return p

def visible(pid):
    found = []
    def visit(hwnd, _):
        if G.IsWindowVisible(hwnd) and win32process.GetWindowThreadProcessId(hwnd)[1] == pid:
            verify(hwnd)
            found.append((hwnd, G.GetWindowText(hwnd)))
    G.EnumWindows(visit, None)
    return found

# Preserve all memory-backed formats, including DIB/PNG/HTML, around fallback checks.
saved = []
owned_sequence = None
CB.OpenClipboard()
try:
    fmt = 0
    while True:
        fmt = CB.EnumClipboardFormats(fmt)
        if not fmt: break
        if fmt == C.CF_BITMAP and CB.IsClipboardFormatAvailable(C.CF_DIB): continue
        value = CB.GetClipboardData(fmt)
        if not isinstance(value, (str, bytes)):
            raise RuntimeError("Clipboard contains a non-memory format; leave it untouched and retry later")
        saved.append((fmt, value))
finally:
    CB.CloseClipboard()

def clipboard_text():
    CB.OpenClipboard()
    try: return CB.GetClipboardData(C.CF_UNICODETEXT) if CB.IsClipboardFormatAvailable(C.CF_UNICODETEXT) else ""
    finally: CB.CloseClipboard()

def seed_clipboard():
    global owned_sequence
    CB.OpenClipboard()
    try:
        CB.EmptyClipboard()
        CB.SetClipboardText("Speaky QA clipboard sentinel", C.CF_UNICODETEXT)
    finally: CB.CloseClipboard()
    owned_sequence = CB.GetClipboardSequenceNumber()

try:
    a, ea = window("Speaky QA input A", sx+35)
    b, eb = window("Speaky QA input B", sx+750)
    probe = launch(args.probe, {**os.environ, "SPEAKY_TARGET_TRACE": "1"})
    answers = queue.Queue()
    threading.Thread(target=lambda: [answers.put(json.loads(line)) for line in probe.stdout], daemon=True).start()
    def ask(op, value="", success=True):
        probe.stdin.write(json.dumps({"op": op, "text": value}, ensure_ascii=False)+"\n")
        probe.stdin.flush()
        reply = wait(lambda: answers.get_nowait() if not answers.empty() else None)
        if success: assert reply["ok"], reply
        return reply
    if not args.app_only and not args.layout_only:
        seed_clipboard()
        original = "原有：选中文字：尾部"
        text(ea, original, (3, 7)); focus(a, ea); pump(.25)
        G.SendMessage(ea, C.EM_SETSEL, 3, 7)
        ask("start"); pump(.15); ask("preview", "临时语音")
        pump(.5)
        print(json.dumps({"preview": G.GetWindowText(ea), "issue": ask("status", success=False)}, ensure_ascii=False), flush=True)
        wait(lambda: G.GetWindowText(ea) == "原有：临时语音：尾部")
        ask("cancel")
        assert G.GetWindowText(ea) == original
        record("cancel restores replaced user selection")
        text(ea, "原有："); focus(a, ea)
        ask("start"); pump(.1); ask("preview", "临时语音")
        wait(lambda: G.GetWindowText(ea) == "原有：临时语音")
        text(ea, "原有：临时语音 手动追加")
        ask("cancel")
        assert G.GetWindowText(ea) == "原有： 手动追加"
        record("cancel preserves manual edits outside preview")
        text(ea, "A 原文："); text(eb, "B 原文："); focus(a, ea)
        ask("start"); pump(.1); ask("preview", "预览")
        wait(lambda: G.GetWindowText(ea) == "A 原文：预览")
        focus(b, eb); ask("stop"); ask("deliver", "最终稿")
        wait(lambda: G.GetWindowText(ea) == "A 原文：")
        assert G.GetWindowText(eb) == "B 原文：最终稿"
        assert clipboard_text() == "Speaky QA clipboard sentinel"
        record("switching editors removes old preview and writes only to stop target")
        text(ea, "A 保留"); text(eb, "B 保留"); focus(a, ea)
        ask("start"); pump(.1); focus(b, eb); ask("stop"); focus(a, ea)
        reply = ask("deliver", "不得写入其他框", success=False)
        assert not reply["ok"]
        assert G.GetWindowText(ea) == "A 保留" and G.GetWindowText(eb) == "B 保留"
        ask("fallback", "手动粘贴的最终稿")
        assert clipboard_text() == "手动粘贴的最终稿"
        owned_sequence = CB.GetClipboardSequenceNumber()
        record("focus change during recognition falls back without writing another editor")
        capture(a, "native-dictation.png")
    if args.app:
        state = {"asr": 0, "llm": 0, "fail_llm": False}
        class Service(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                if self.path.endswith("/chat/completions"):
                    state["llm"] += 1
                    request = json.loads(body)
                    assert request["messages"][1]["content"] == "口述原文"
                    code = 503 if state["fail_llm"] else 200
                    result = {"choices": [{"finish_reason": "stop", "message": {"content": "整理后的最终稿。"}}]}
                else:
                    state["asr"] += 1
                    time.sleep(state.get("delay", 0))
                    code, result = 200, {"text": "口述原文"}
                payload = json.dumps(result, ensure_ascii=False).encode()
                self.send_response(code); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload))); self.end_headers(); self.wfile.write(payload)
            def log_message(self, *_): pass
        service = ThreadingHTTPServer(("127.0.0.1", 0), Service)
        threading.Thread(target=service.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{service.server_port}/v1"
        profile = args.out / "app-profile"
        profile.mkdir(exist_ok=True)
        (profile / "onboarding-v1.done").write_text("done\n")
        import yaml
        (profile / "config.yaml").write_text(yaml.safe_dump({
            "desktop": {"windows_floating_button": True, "debug_mode": True},
            "engine": {"current": "openai", "openai": {"base_url": base, "model": "test-asr", "api_key": "test-only"}},
            "llm": {"enabled": True, "openai": {"base_url": base, "model": "test-rewriter", "api_key": "test-only"}},
        }), encoding="utf-8")
        (profile / "floating-position.json").write_text(json.dumps({"x": sx+80, "y": ey-100}))
        app = launch(args.app, {**os.environ, "SPEAKY_CONFIG_DIR": str(profile.resolve()), "SPEAKY_START_ON_SECONDARY": "1"})
        main = wait(lambda: next((w for w, title in visible(app.pid) if title == "Speaky"), None), 30)
        pump(1)
        G.SetWindowPos(main, C.HWND_TOP, 0, 0, 0, 0, C.SWP_NOMOVE|C.SWP_NOSIZE|C.SWP_NOACTIVATE)
        capture(main, "application.png")
        record("application and floating control open fully on secondary monitor")
        def toggle():
            # Exercise the registered-hotkey event path without sending keys to the user's desktop.
            assert G.GetForegroundWindow() in (a, b, main)
            for thread in psutil.Process(app.pid).threads():
                try: win32api.PostThreadMessage(thread.id, C.WM_HOTKEY, 1, 0)
                except Exception: pass
            pump(.15)
        def recording_window():
            return next((w for w, title in visible(app.pid) if title == "Speaky · 录音"), None)
        def wavs(): return set((profile / "drafts").glob("*.wav"))
        if args.layout_only:
            verify(main)
            G.PostMessage(main, C.WM_LBUTTONDOWN, C.MK_LBUTTON, win32api.MAKELONG(720, 155))
            G.PostMessage(main, C.WM_LBUTTONUP, 0, win32api.MAKELONG(720, 155))
            floating = wait(lambda: next((w for w, title in visible(app.pid) if title == "Speaky · 录音" and G.GetWindowRect(w)[3]-G.GetWindowRect(w)[1] < 70), None), 15)
            pump(1)
            capture(floating, "recording-controls.png")
            rect = G.GetWindowRect(floating)
            assert rect[2]-rect[0] >= 285 and rect[3]-rect[1] >= 40, rect
            record("mixed-DPI recording bar keeps both cancel and confirm buttons visible")
            G.PostMessage(floating, C.WM_LBUTTONDOWN, C.MK_LBUTTON, win32api.MAKELONG(14, 20))
            G.PostMessage(floating, C.WM_LBUTTONUP, 0, win32api.MAKELONG(14, 20))
            wait(lambda: not recording_window())
            assert any(p.stat().st_size > 44 for p in wavs())
            G.PostMessage(main, C.WM_CLOSE, 0, 0)
            wait(lambda: app.poll() is not None, 10)
            (args.out / "results.json").write_text(json.dumps({"secondary": secondary, "passed": checks}, ensure_ascii=False, indent=2), encoding="utf-8")
            raise SystemExit(0)
        before = wavs(); focus(a, ea); toggle()
        floating = wait(lambda: next((w for w, title in visible(app.pid) if title == "Speaky · 录音" and G.GetWindowRect(w)[3]-G.GetWindowRect(w)[1] < 70), None), 15); pump(1.2)
        capture(floating, "recording-controls.png")
        verify(floating)
        G.PostMessage(floating, C.WM_LBUTTONDOWN, C.MK_LBUTTON, win32api.MAKELONG(14, 20))
        G.PostMessage(floating, C.WM_LBUTTONUP, 0, win32api.MAKELONG(14, 20))
        wait(lambda: not recording_window())
        cancelled = wait(lambda: next(iter(wavs() - before), None))
        assert cancelled.stat().st_size > 44
        assert not cancelled.with_suffix(".json").exists()
        record("cancel button preserves raw recording and skips final ASR/LLM")
        # With a live microphone, check the complete save -> ASR -> rewrite -> delivery flow.
        seed_clipboard(); text(ea, "A 原文："); text(eb, "B 原文："); before = wavs()
        focus(a, ea); toggle(); wait(recording_window); pump(2.3)
        focus(b, eb); toggle()
        completed = wait(lambda: next(iter(wavs() - before), None))
        pump(.7)
        pcm = completed.read_bytes()[44:]
        if pcm and any(pcm):
            wait(lambda: "整理后的最终稿。" in G.GetWindowText(eb), 20)
            wait(lambda: G.GetWindowText(ea) == "A 原文：", 5)
            assert G.GetWindowText(eb) == "B 原文：整理后的最终稿。"
            assert clipboard_text() == "Speaky QA clipboard sentinel"
            original = json.loads(completed.with_suffix(".json").read_text(encoding="utf-8"))
            organized = json.loads(completed.with_suffix(".polish.json").read_text(encoding="utf-8"))
            debug = completed.with_suffix(".debug.json").read_text(encoding="utf-8")
            assert original["text"] == organized["source"] == "口述原文"
            assert organized["text"] == "整理后的最终稿。" and "test-only" not in debug
            record("saved audio, ASR original and rewrite stay independent; rewrite is delivered")
            state["fail_llm"] = True; text(ea, "失败降级："); focus(a, ea); toggle(); wait(recording_window); pump(1.2); toggle()
            wait(lambda: G.GetWindowText(ea) == "失败降级：口述原文", 20)
            record("LLM failure automatically delivers saved ASR original")
            state["fail_llm"] = False
            state["delay"] = 1.2
            text(ea, "A 保留："); text(eb, "B 保留：")
            focus(a, ea); toggle(); wait(recording_window); pump(1.3)
            focus(b, eb); toggle(); focus(a, ea)
            wait(lambda: clipboard_text() == "整理后的最终稿。", 25)
            owned_sequence = CB.GetClipboardSequenceNumber()
            wait(lambda: G.GetWindowText(ea) == "A 保留：", 5)
            assert G.GetWindowText(eb) == "B 保留：", repr(G.GetWindowText(eb))
            notice = wait(lambda: next((w for w, title in visible(app.pid) if title == "Speaky · 剪贴板"), None))
            capture(notice, "clipboard-fallback.png")
            record("focus change after stop leaves editors untouched and shows clipboard notice")
            state["delay"] = 0
        else:
            print("SKIP live ASR delivery: microphone supplied digital silence", flush=True)
        before = wavs(); focus(a, ea); toggle(); wait(recording_window); pump(.7)
        verify(a); assert G.GetForegroundWindow() == a
        win32api.keybd_event(C.VK_ESCAPE, 0, 0, 0); pump(.1)
        win32api.keybd_event(C.VK_ESCAPE, 0, C.KEYEVENTF_KEYUP, 0)
        wait(lambda: not recording_window())
        escaped = wait(lambda: next(iter(wavs() - before), None))
        assert escaped.stat().st_size > 44 and not escaped.with_suffix(".json").exists()
        record("Escape cancels final transcription while saving recorded audio")
        before = wavs(); focus(a, ea); toggle(); wait(recording_window); pump(.7)
        G.PostMessage(main, C.WM_CLOSE, 0, 0)
        wait(lambda: app.poll() is not None, 10)
        exited = next(iter(wavs() - before))
        assert exited.stat().st_size > 44
        record("closing during recording saves raw audio before exit")
        tour_profile = args.out / "tour-profile"
        tour_profile.mkdir(exist_ok=True)
        (tour_profile / "config.yaml").write_text("desktop:\n  windows_floating_button: false\n", encoding="utf-8")
        tour = launch(args.app, {**os.environ, "SPEAKY_CONFIG_DIR": str(tour_profile.resolve()), "SPEAKY_START_ON_SECONDARY": "1"})
        tour_window = wait(lambda: next((w for w, title in visible(tour.pid) if title == "Speaky"), None), 30)
        pump(1.5); capture(tour_window, "onboarding.png")
        G.PostMessage(tour_window, C.WM_CLOSE, 0, 0)
        wait(lambda: tour.poll() is not None, 10)
        record("first-run onboarding opens and closes safely on secondary monitor")
        service.shutdown()
    (args.out / "results.json").write_text(json.dumps({"secondary": secondary, "passed": checks}, ensure_ascii=False, indent=2), encoding="utf-8")
finally:
    for p in processes:
        if p.poll() is None:
            p.terminate()
        p.wait(timeout=10)
        if p is probe:
            print(p.stderr.read(), flush=True)
    for hwnd in windows:
        if G.IsWindow(hwnd): G.DestroyWindow(hwnd)
    cursor = G.GetCursorPos()
    if sx <= cursor[0] < ex and sy <= cursor[1] < ey:
        win32api.SetCursorPos(original_cursor)
    if owned_sequence is not None and CB.GetClipboardSequenceNumber() == owned_sequence:
        CB.OpenClipboard()
        try:
            CB.EmptyClipboard()
            for fmt, value in saved: CB.SetClipboardData(fmt, value)
        finally: CB.CloseClipboard()
