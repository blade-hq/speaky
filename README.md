# Speaky

Speaky 是用 Rust 和 GPUI 开发的 macOS 和 Windows 语音输入工具。按快捷键开始听写，再按一次停止，将最终文字填入停止时聚焦的输入框。也可以在 Speaky 首页录音，边录边看转写预览。原始音频、ASR 原文和可选的大模型整理稿分别保存在本地历史中。

## 从源码运行

先安装 Rust stable。macOS 需要 Command Line Tools。Windows 推荐使用 MSVC 工具链，并在 Visual Studio Build Tools 中勾选「使用 C++ 的桌面开发」和 Windows SDK。在仓库根目录运行下面的命令。

```sh
cargo run --locked -p speaky-gpui
```

Windows 构建后也可双击 `start-windows.cmd`。在「听写」首页点击「模型」，可以选择本机已安装的 Qwen3-ASR、SenseVoiceSmall 或 Whisper。首次录音前，请在 Windows 的麦克风隐私设置中允许桌面应用访问麦克风。macOS 首次录音时会请求麦克风权限。

## Windows 7 独立版

在 Windows 7 SP1 x64 上，下载 `Speaky-Windows7-x64.zip`，解压后运行 `speaky-win7.exe`。独立版连接外部语音识别服务，支持全局快捷键和原生悬浮听写。参见 [Win7 部署说明](docs/Win7部署说明.md)。

## 使用

- 首次引导依次帮助开通权限、设置快捷键、选择推荐识别语言、检测麦克风并练习真实输入；可随时跳过，也可从左下角「进入引导」重新开始。macOS 需允许麦克风和辅助功能权限。
- Windows 按 `Ctrl+Shift+空格`，macOS 按 `⌥⇧Space` 开始录音，再按一次「停止并保存」。录音期间可以切换应用；最终转写写入停止时聚焦的可写输入框，保留已有文字和手动编辑，由用户决定发送。
- 悬浮条左侧 `✕` 或 `Esc` 取消本次转写并安全清除中间预览，已录原声仍保存在历史；右侧 `✓` 保存后正常转写和写入。
- 应用内录音始终边录边转写，无需另开预览。停止时先保存原始音频，再生成最终 ASR 文字；识别失败后可从历史「重新转写」，未转写的录音也可继续录制。
- 若无法自动写入，结果会保存在剪贴板，并显示「语音识别内容已保存到剪贴板，请手动粘贴」悬浮提醒；成功写入时恢复用户原有剪贴板内容。
- 在「设置 → 识别 → 接入方式」输入服务地址，按需填写模型 ID 和 API 密钥。Speaky 会自动检测 OpenAI 兼容转写、whisper.cpp 原生 HTTP 或 FunASR WebSocket，用内置语音试转写，成功后启用服务。
- 在「设置 → 语音整理」配置并启用 OpenAI 兼容大模型（建议开启）。最终转写自动整理，优先使用整理稿；调用失败自动使用 ASR 原文。录音、ASR 原文和整理稿互不覆盖。
- 首页与设置中可查看当前麦克风、音量和 ASR/LLM 模型；「测试麦克风」「检测可用性」「测试大模型」分别检查录音、识别和整理。开启「Debug 模式」后，可在历史条目「查看改写请求」中查看脱敏后的模型名、系统提示词和实际输入。

## 接入语音识别

- **下载并启动 Whisper Base。** 在「工作台 → 识别模型」点击「下载并启动 Whisper Base」。首次下载约 148 MB，完成后应用会启动模型并切换到本地识别。macOS 和 Windows 桌面发布包附带推理程序。从源码运行时，应用也可以使用本机已安装的 `whisper-server`。

- **本地离线模型包。** 将适用于当前系统的包解压到可执行文件同级的 `packs/`，在「工作台 → 识别模型」选择。每个包自带推理运行时和权重，可在断网环境中启动。
- **外部服务。** 在「设置 → 识别 → 配置服务」输入已部署的地址，按需填写模型 ID 和 API 密钥。应用会试转写一段录音，检测 OpenAI 兼容 HTTP、whisper.cpp 原生 HTTP 或 FunASR WebSocket。火山引擎通过 `config.yaml` 配置。

本机模型的配置见 [Windows 本地运行](docs/Windows本地运行.md)。常用词、语音整理和历史记录的用法见 [桌面版使用指南](speaky-gpui/README.md)。

早期 Python 版本的历史文件仍保存在 `~/.speaky/history.json`（默认路径），GPUI 暂不导入该格式。

## 开发验证

```sh
cargo test --workspace --locked
cargo build --locked -p speaky-gpui
```

GPUI 界面位于 `speaky-gpui/src/`，录音、历史和识别引擎位于 `speaky-core/src/`。
