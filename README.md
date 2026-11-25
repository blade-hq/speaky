# Speaky

Speaky 是用 Rust 和 GPUI 开发的 macOS 和 Windows 语音输入工具。在 Windows 聊天输入框中按快捷键开始听写，再按一次即可把转写填入原输入框。在 Speaky 首页录音时，应用实时显示转写。停止后，原始音频会保存在本地历史。

Windows 发布包解压后运行 `start-windows.cmd`。macOS 发布包解压后，在相应目录运行 `./speaky-gpui`。

## 从源码运行

先安装 Rust stable。macOS 需要 Command Line Tools。Windows 推荐使用 MSVC 工具链，并在 Visual Studio Build Tools 中勾选「使用 C++ 的桌面开发」和 Windows SDK。在仓库根目录运行下面的命令。

```sh
cargo run --locked -p speaky-gpui
```

Windows 构建后也可双击 `start-windows.cmd`。首次录音前，请在 Windows 的麦克风隐私设置中允许桌面应用访问麦克风。macOS 首次录音时会请求麦克风权限。

## Windows 7 独立版

在 Windows 7 SP1 x64 上，下载 `Speaky-Windows7-x64.zip`，解压后运行 `speaky-win7.exe`。独立版连接外部语音识别服务，支持全局快捷键和原生悬浮听写。参见 [Win7 部署说明](docs/Win7部署说明.md)。

## 使用

- Windows 按 `Ctrl+Shift+空格`，macOS 按 `⌥⇧Space` 开始录音。再按一次停止并保存。录音进度和波形显示在悬浮窗中。
- 录音时首页会逐段显示识别文字。Windows 聊天听写会在起始输入框实时出字。全局快捷键听写的最终稿会保存到历史，并写入转写完成时聚焦的输入框。
- 在「历史」播放已保存的录音、复制转写文本或继续录制未转写的音频。

## 接入语音识别

- **下载并启动 Whisper Base。** 在「工作台 → 识别模型」点击「下载并启动 Whisper Base」。首次下载约 148 MB，完成后应用会启动模型并切换到本地识别。macOS 和 Windows 桌面发布包附带推理程序。从源码运行时，应用也可以使用本机已安装的 `whisper-server`。

- **本地离线模型包。** 将适用于当前系统的包解压到可执行文件同级的 `packs/`，在「工作台 → 识别模型」选择。每个包自带推理运行时和权重，可在断网环境中启动。
- **外部服务。** 在「设置 → 识别 → 配置服务」输入已部署的地址，按需填写模型 ID 和 API 密钥。应用会试转写一段录音，检测 OpenAI 兼容 HTTP、whisper.cpp 原生 HTTP 或 FunASR WebSocket。火山引擎通过 `config.yaml` 配置。

模型包的目录、清单格式和离线部署步骤见 [Windows 本地运行](docs/Windows本地运行.md)，其中的清单也适用于 macOS。常用词、语音整理和历史记录见 [桌面版使用指南](speaky-gpui/README.md)。

## 开发验证

```sh
cargo test --workspace --locked
cargo build --locked -p speaky-gpui
```

GPUI 界面位于 `speaky-gpui/src/`，录音、历史和识别引擎位于 `speaky-core/src/`。
