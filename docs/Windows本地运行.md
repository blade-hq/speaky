# Windows 本地运行

解压 `Speaky-Windows-x64.zip` 后，双击 `start-windows.cmd`。首次录音前，在 Windows 麦克风隐私设置中允许桌面应用访问麦克风。

## 离线模型包

将适用于本机系统和架构的模型包解压到 `speaky-gpui.exe` 同级的 `packs` 目录。每个包占一个子目录，`manifest.json`、推理运行时和权重都在该目录内。离线机器只需复制 Speaky 和这些已备齐文件的模型包。

在「工作台 → 识别模型」打开模型列表。可以点「打开模型包目录」投放新包，关闭后重新打开列表即可刷新。选择模型包后，Speaky 启动包内的服务并等待健康检查通过。在「设置 → 识别」可开启启动预加载，或释放当前模型占用的资源。释放后，下次转写会重新加载。

Speaky 按以下顺序扫描，同名 `id` 采用最先找到的包。

1. `SPEAKY_PACKS_DIR` 指定的绝对目录。
2. 可执行文件同级的 `packs/`。macOS 可在解压后的 `speaky-gpui` 旁创建该目录。
3. 用户配置目录下的 `packs/`。Windows 默认是 `%APPDATA%\speaky\gpui\packs`，macOS 默认是 `~/Library/Application Support/speaky/packs`。

### 模型包清单

下例假定推理程序在 `runtime/asr-server.exe`，模型权重在 `model/model.bin`。实际启动参数按包内程序填写。

```json
{
  "schema_version": 1,
  "id": "example-asr-pack",
  "name": "Example ASR Pack",
  "os": "windows",
  "arch": "x86_64",
  "command": ["runtime/asr-server.exe", "--model", "model/model.bin", "--host", "{host}", "--port", "{port}"],
  "health_path": "/health",
  "api_base_path": "/v1",
  "model_id": "example-asr",
  "use_vocabulary_prompt": false,
  "env": {}
}
```

`schema_version`、`id`、`name`、`command` 和 `model_id` 必填。`schema_version` 固定为 `1`。指定 `os` 或 `arch` 时，包只在匹配的平台显示，例如 `macos` 和 `aarch64`。可执行文件使用相对路径时必须位于包目录内，进程的工作目录也是包目录。

启动参数和 `env` 的值可引用 `{host}`、`{port}` 和 `{pack_dir}`，分别对应 `127.0.0.1`、Speaky 分配的临时端口和包目录的绝对路径。健康检查路径默认是 `/health`，应在模型就绪时返回 HTTP 2xx。API 基础路径默认是 `/v1`，服务须在其下提供 OpenAI 兼容的 `POST /audio/transcriptions` 接口。`use_vocabulary_prompt` 默认关闭，服务支持 `prompt` 字段时可启用。

## 外部识别服务

在「设置 → 识别 → 配置服务」填入已部署的地址，按需填写模型 ID 和 API 密钥。Speaky 用内置录音检测 OpenAI 兼容 HTTP、whisper.cpp 原生 HTTP 或 FunASR WebSocket 服务，转写成功后保存连接。火山引擎可通过 `config.yaml` 的 `engine.current: volc_bigmodel` 和 `engine.volc_bigmodel` 配置，见仓库中的 `config.example.yaml`。
