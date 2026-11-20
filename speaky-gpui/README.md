# Speaky GPUI macOS 原型

在仓库根目录运行 `cargo run --locked --manifest-path speaky-gpui/Cargo.toml`。原型使用 GPUI 的运行时着色器，可用 macOS Command Line Tools 构建。

按 `⌥⇧Space` 开始录音，切换到其他应用后继续说，再按一次结束。也可以用窗口中的按钮开始和结束。转写完成后，从窗口或最近转写中复制文字，再粘贴到目标输入框。

录音时，Speaky 每两秒检查一次是否有新音频，并将短片段送给已配置的识别服务。识别结果会陆续出现在面板里的「听写预览」。片段边界可能影响预览文字。停录后，Speaky 保存完整 WAV 并转写整段录音，用最终稿替换预览并写入历史。

录音期间每两秒尝试原子保存一次 WAV 快照。重启后可从最近的录音草稿重试转写。草稿存放在 Speaky 配置目录的 `drafts` 子目录。原型读取现有的语音识别配置，火山引擎也可用 `SPEAKY_VOLC_API_KEY` 环境变量配置。

开发运行时，由 macOS 控制启动该程序的终端进程及应用的麦克风访问。在「系统设置 → 隐私与安全性 → 麦克风」里检查权限。

## 内网 ASR 联调

原型读取 Speaky 配置目录中的 `config.yaml`。可合并以下字段，启用 `gpu11` 上的 SenseVoice-Small CPU 服务。录音会发送到该内网服务，`EMPTY` 是兼容适配器的占位值。

```yaml
engine:
  current: openai
  openai:
    base_url: http://192.168.130.11:8201/v1
    model: sensevoice-small
    api_key: EMPTY
```
