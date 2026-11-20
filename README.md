# Speaky

Speaky 是用 Rust 和 GPUI 开发的 macOS 语音输入工具。用全局快捷键录音，停止后将原始音频保存在本地历史，再按需转写。

## 从源码运行

安装 Rust stable 和 macOS Command Line Tools，然后在仓库根目录运行下面的命令。

```sh
cargo run --locked --manifest-path speaky-gpui/Cargo.toml
```

首次录音时，在 macOS 的麦克风权限提示中允许访问。

## 使用

- 按 `⌥⇧Space` 开始录音，再按一次停止并保存。录音进度和波形显示在悬浮窗中。
- 在首页或历史中选择「转写」。需要边录边出字时，可在当次录音中开启「转写预览」。
- 在「设置 → 识别」填写 OpenAI 兼容服务的地址、模型 ID 和 API Key，再用「检查连接」和「试转写」验证配置。
- 在「历史」播放已保存的录音、复制转写文本或继续录制未转写的音频。

更多说明见 [macOS 使用与服务配置](speaky-gpui/README.md)。

早期 Python 版本的历史文件仍保存在 `~/.speaky/history.json`（默认路径），GPUI 暂不导入该格式。

## 开发验证

```sh
cargo test --locked --manifest-path speaky-gpui/Cargo.toml
cargo build --locked --manifest-path speaky-gpui/Cargo.toml
```

GPUI 界面、录音、历史和识别引擎都位于 `speaky-gpui/src/`。
