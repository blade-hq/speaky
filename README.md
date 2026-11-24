# Speaky

Speaky 是用 Rust 和 GPUI 开发的 macOS 和 Windows 语音输入工具。在 Windows 聊天输入框中按快捷键开始听写，再按一次即可把转写填入原输入框。在 Speaky 首页也可以单独录音，把原始音频保存在本地历史后按需转写。

## 从源码运行

先安装 Rust stable。macOS 需要 Command Line Tools。Windows 推荐使用 MSVC 工具链，并在 Visual Studio Build Tools 中勾选「使用 C++ 的桌面开发」和 Windows SDK。在仓库根目录运行下面的命令。

```sh
cargo run --locked --manifest-path speaky-gpui/Cargo.toml
```

Windows 构建后也可双击 `start-windows.cmd`。在「听写」首页点击「模型」，可以选择本机已安装的 Qwen3-ASR、SenseVoiceSmall 或 Whisper。首次录音前，请在 Windows 的麦克风隐私设置中允许桌面应用访问麦克风。macOS 首次录音时会请求麦克风权限。

## 使用

- Windows 按 `Ctrl+Shift+空格`，macOS 按 `⌥⇧Space` 开始录音。再按一次停止并保存。录音进度和波形显示在悬浮窗中。
- 录音时首页会逐段显示识别文字。Windows 聊天听写会在原输入框实时出字，停止后保存完整转写。
- 在「设置 → 识别 → 接入方式」输入服务地址，按需填写模型 ID 和 API 密钥。Speaky 会自动检测 OpenAI 兼容转写、whisper.cpp 原生 HTTP 或 FunASR WebSocket，用内置语音试转写，成功后启用服务。
- 在「历史」播放已保存的录音、复制转写文本或继续录制未转写的音频。

本机模型的配置见 [Windows 本地运行](docs/Windows本地运行.md)。常用词、语音整理和历史记录的用法见 [桌面版使用指南](speaky-gpui/README.md)。

早期 Python 版本的历史文件仍保存在 `~/.speaky/history.json`（默认路径），GPUI 暂不导入该格式。

## 开发验证

```sh
cargo test --locked --manifest-path speaky-gpui/Cargo.toml
cargo build --locked --manifest-path speaky-gpui/Cargo.toml
```

GPUI 界面、录音、历史和识别引擎都位于 `speaky-gpui/src/`。
