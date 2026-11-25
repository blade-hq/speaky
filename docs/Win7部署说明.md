# Speaky Windows 7 部署说明

适用环境为 Windows 7 SP1 x64。解压 `Speaky-Windows7-x64.zip` 后双击 `speaky-win7.exe`。发布包静态链接 Visual C++ 运行库，无需单独安装。录音悬浮窗和桌面按钮使用 Direct2D 软件绘制，关闭 Aero 也可使用。

## 连接内网语音识别服务

1. 确认识别服务已启动，Win7 电脑可以访问它的地址和端口。
2. 打开「设置服务」，选择「外部 ASR」，输入服务地址，例如 `http://asr.example.internal:8000`，以及服务要求的 API Key。
3. OpenAI 兼容服务可点击「获取模型」，在「模型 ID」下拉框中选择一项。服务未提供列表时可直接填写模型 ID。点击「保存识别服务」并查看试转写结果。也可连接 whisper.cpp 或 FunASR WebSocket。使用火山引擎时选择对应服务并填写 API Key。
4. 在同一页选择麦克风、识别语言、热键及悬浮按钮，再点「保存偏好」。

使用 HTTPS 或 WSS 时，Win7 客户端需具备 TLS 1.2 支持并信任服务证书的签发链。检查系统补丁、时钟及根证书。使用内部 CA 时，由管理员在目标设备导入可信根证书。含音频的 HTTP 或 WS 流量应限制在隔离的可信内网。

## 共享 Win10 或 Win11 宿主机的模型

在宿主机的 Speaky 目录中，使用 Windows 本地识别环境里的 Python 运行共享服务。该环境需要 `fastapi`、`uvicorn`、`python-multipart`、`requests`，以及所选模型的本地运行库。模型目录沿用 [Windows 本地运行说明](Windows本地运行.md) 中的设置。

```powershell
$env:SPEAKY_ASR_TOKEN = '<自行设置的访问密钥>'
$vmHostIp = Read-Host '输入宿主机在隔离虚拟网络中的 IPv4 地址'
& .\models\asr-env\Scripts\python.exe .\scripts\share-windows-models.py --host $vmHostIp --port 18769
```

将示例里的 Python 路径换成实际路径。脚本默认只监听回环地址，供本机测试。连接 Win7 虚拟机时，指定隔离虚拟网卡上的宿主机地址。Win7 填写该地址和端口，输入相同的密钥，再点击「获取模型」。按 `Ctrl+C` 停止共享。

服务列出已发现的内置模型，以及当前可访问的本地模型服务。选中宿主机已经加载的同一模型时，直接复用已有服务。宿主机正在使用另一模型时，共享服务会尝试用 CPU 加载所选模型。共享服务自行加载的模型空闲约 30 秒后会释放。

自定义模型目录可用 `SPEAKY_MODELS_DIR` 指定。`--packs-dir` 可补充信任的离线模型包目录。Whisper CPU 需要 `whisper-runtime\whisper-server.exe`，或通过 `--whisper-server` 指定已安装的程序。SenseVoice 和 Vosk 的 CPU 环境可用 `--cpu-python` 指定；未指定时优先使用模型目录下的 `bridge-env`。

## 使用

- 在输入框取得焦点后按默认热键 `Ctrl+Shift+Space`，或单击桌面「听写」按钮。实时文字会尝试写入原输入框，再按一次结束。内容由用户自行发送。
- 焦点移开后可到主窗口的历史列表复制结果。录音优先保存到本地配置目录的 `drafts`，服务失败后可重新转写。
- 拖动悬浮窗可保存位置。关闭主窗口会隐藏到托盘，通过托盘菜单可重新打开或退出。退出会先保存当前录音。
