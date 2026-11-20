# Speaky macOS 使用指南

在仓库根目录运行 `cargo run --locked --manifest-path speaky-gpui/Cargo.toml`。应用使用 GPUI 的运行时着色器，可用 macOS Command Line Tools 构建。

默认按 `⌥⇧Space` 开始录音，再按一次停止并保存。主窗口和波形悬浮窗的方形按钮也可停止并保存。切换应用、切换桌面或最小化主窗口时，录音仍在同一会话中。主窗口的关闭按钮会最小化窗口，退出应用可用 `⌘Q`，退出时也会保存正在录制的音频。

侧栏「设置」或 `⌘,` 可更改录音快捷键、输入设备和识别语言。要切换 OpenAI 兼容服务，在「识别 → 编辑服务」填写服务地址（包含 API 前缀，例如 `/v1`）、模型 ID 和 API Key。「检查连接」通过服务的模型列表确认所选模型。「试转写」发送随应用附带的两秒合成中文 WAV，并显示转写和耗时，无需麦克风。

录音默认只保存在本地。录音期间可以点「开启转写预览」，将当次录音的短片段送给已配置的识别服务，文字会陆续出现在「听写预览」中。停录后可在首页或历史里点「转写」，用完整 WAV 生成最终稿，再从首页或历史复制文字。

录音期间每两秒尝试原子保存一次 WAV 快照。原录音保留在历史里，支持播放、转写，以及继续录制未转写的录音。旧版本标为已取消的录音仍可在历史中使用。首页显示录音数量、总时长、转写字符数和近七天活动。文件位于 Speaky 配置目录的 `drafts` 子目录。

常用词每行一个，或用 `误识别词 => 正确写法` 添加替换规则。所有引擎支持本地替换。已确认支持 `prompt` 的 Whisper 服务可以设置 `engine.openai.use_vocabulary_prompt: true`，把常用词作为识别提示发送。当前内网 SenseVoice 配置使用本地替换。火山引擎可用 `SPEAKY_VOLC_API_KEY` 环境变量配置。

开发运行时，由 macOS 控制启动该程序的终端进程及应用的麦克风访问。在「系统设置 → 隐私与安全性 → 麦克风」里检查权限。

## 内网 ASR 联调

应用读取 `~/Library/Application Support/speaky/config.yaml`。可合并以下字段，启用 `gpu11` 上的 SenseVoice-Small CPU 服务。录音会发送到该内网服务，`EMPTY` 是兼容适配器的占位值。

```yaml
engine:
  current: openai
  openai:
    base_url: http://192.168.130.11:8201/v1
    model: sensevoice-small
    api_key: EMPTY
```
