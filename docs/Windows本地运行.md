# 在 Windows 启动 Speaky

拿到 Windows 便携包后，先解压，再双击顶层的 `start-windows.cmd`。启动器会打开同目录的 `speaky-gpui.exe`。如果只使用远程识别服务，也可直接打开应用，在设置里填写识别服务的地址、模型与 API 密钥。

在「设置 → 录音 → 全局快捷键」按下想用的按键，例如 `F8` 或 `Ctrl+Shift+R`。初始快捷键是 `Ctrl+Shift+空格`。先点击聊天输入框，让光标出现在要写字的位置，再按一次快捷键开始说话。在支持读取选区的输入框里，文字会随说话逐段出现；Speaky 会根据后面的语音修正刚写的词。再按一次结束，确认后自行发送。手动编辑、移动光标或切换输入框后，可到 Speaky 历史复制最终稿。仅支持一次性写入的输入框会在停止后收到最终稿，录音时可看悬浮字幕。

点击 Speaky 首页的麦克风按钮，即可在首页看实时文字。如果没有声音，在 Windows 麦克风隐私设置中允许桌面应用访问麦克风，再到 Speaky「设置 → 输入设备」选择麦克风。

也可以全程用鼠标听写。桌面右下角默认显示「开始听写」悬浮按钮，主窗口最小化后也能使用。先点目标输入框，再点悬浮按钮开始说话，点「停止并保存」结束。实时字幕下方的彩色声波会随麦克风音量起伏，第一句话出字前就能看到收音反馈。Speaky 会把识别结果填回仍有焦点的输入框。拖动悬浮按钮可以调整位置，在「设置 → 录音 → 桌面悬浮按钮」可以隐藏或重新显示它。

便携包包含桌面客户端和本机识别服务脚本。首次录音即可在本地保存和回放。转写需要配置识别服务，语音整理需要在「设置 → 语音整理 → 编辑模型」中另行配置兼容的聊天模型。

从源码运行时，先在安装了 Rust 和项目所需 Windows 构建工具的终端执行以下命令，再双击仓库根目录的 `start-windows.cmd`。启动器会选择最近构建的版本，识别 Cargo 的 debug、release 和 `CARGO_TARGET_DIR` 输出目录。

```powershell
cargo build --locked --manifest-path .\speaky-gpui\Cargo.toml
```

要在开发机上生成可分发的 release 版本，请安装包含 `fxc.exe` 的 Windows SDK，并设置 `GPUI_FXC_PATH` 指向此文件，例如 `C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe`，再执行 `cargo build --release --locked --manifest-path .\speaky-gpui\Cargo.toml`。这个版本包含已经编译的界面着色器。使用便携包的电脑可直接运行。

## 在听写首页选择本地模型

在「设置 → 识别 → 接入方式」输入服务地址后，Speaky 会自动检测协议并用内置录音试转写。三种接入方式和本机服务的启动步骤见 [识别服务接入指南](ASR-frameworks.md)。

点击「听写」标题旁的「模型」，从列表中选择已安装的语音识别模型。切换完成后，按钮会显示所选模型，重启后也会记住选择。录音预览和历史记录的「转写」都使用当前模型。要比较两个模型对同一句话的识别效果，可先记下第一次的结果，再切换模型，对同一条历史录音点「重新转写」。

把模型放在程序目录或其上一级的 `ASR-Models` 文件夹中，每个模型占一个子目录。其他位置可通过环境变量 `SPEAKY_MODELS_DIR` 指定，多个目录用分号隔开。打开模型列表时会重新扫描。便携包使用电脑上已安装的权重和推理环境。

Whisper 的多个 GGML 权重可以放在同一个 `models\whisper` 子目录。模型列表会分别显示 Tiny、Base、Small、Large v3 Turbo Q5 和 Large v3 Turbo；切换时实际加载对应的文件。旧版 `ggml-large-v3-turbo-q8_0.bin` 仍可使用。该目录只需一份 `runtime-cuda`，其中应包含与下表一致的 whisper.cpp 运行库。

若便携应用解压在一个子目录，而模型放在该子目录的上一级 `models\whisper`，模型选择器也会扫描上一级的模型目录。关闭并重新打开模型列表可重新扫描；无需复制权重文件。

其他已安装模型可在程序目录或上一级的 `models\model-roots.txt` 中逐行写入模型根目录的绝对路径。Speaky 会把这些目录中的模型加入同一列表；项目目录里的模型优先显示，其他模型可向下滚动查看。

每次只加载一个本地识别模型。切换时，Speaky 先结束旧模型的推理进程，再在 NVIDIA GPU 上加载下一项。应用退出后会释放所用显存，磁盘上的模型文件保留。录音、转写和试转写期间，模型选择暂时锁定。

同一台电脑上的 Win7 虚拟机可通过[宿主机模型共享服务](Win7部署说明.md#共享-win10-或-win11-宿主机的模型)读取本地模型列表并使用已安装的权重。

若 CUDA 不可用、推理组件缺失或显存不足，Speaky 会显示错误。可以关闭其他占用显存的程序或选择较小的模型后重试。桌面显示和其他应用占用的显存由相应程序管理。

## 本地推理环境

| 模型 | Windows 上的 GPU 运行方式 |
| --- | --- |
| Qwen3-ASR-0.6B、Qwen3-ASR-1.7B | 使用支持 CUDA 的 PyTorch 加载全部参数。 |
| SenseVoiceSmall-FP32 | 使用同一发行版的 `model.onnx`（FP32）和 `tokens.txt`。推理使用 ONNX Runtime CUDA，并在加载时检查主要神经网络运算的实际设备。此前保存的 int8 选择会自动对应到 FP32。 |
| Whisper Tiny、Base、Small、Large v3 Turbo Q5、Large v3 Turbo，以及旧版 Whisper-large-v3-turbo | 分别加载同名的 `ggml-*.bin` 文件。模型目录的 `runtime-cuda` 下需有 whisper.cpp v1.8.3 的 Windows x64 CUDA 11.8 运行库，系统或该目录需提供 cuBLAS 11。Speaky 会检查模型权重实际分配的 GPU。 |

本机验证的 SenseVoice 运行环境使用 `onnxruntime-gpu==1.22.0`、`kaldi-native-fbank==1.23.3` 以及 CUDA 12 和 cuDNN 9。Python 组件可以随模型放在 `runtime/python` 子目录。[SenseVoice FP32 权重及词表](https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/tree/main) · [whisper.cpp v1.8.3 运行库](https://github.com/ggml-org/whisper.cpp/releases/tag/v1.8.3)

Qwen3-ASR-0.6B 和 Qwen3-ASR-1.7B 使用完整的本地检查点，支持单文件以及带索引的分片 safetensors。Python 环境需要支持 CUDA 的 PyTorch、`qwen-asr`、FastAPI、uvicorn、python-multipart 和 soundfile。便携版会查找 `models\asr-env\Scripts\python.exe` 和同级旧版 `speaky\models\asr-env\Scripts\python.exe`。可以在 PowerShell 中指定其他安装位置。

如果模型与 Python 环境分开放置，可在程序目录的 `models\asr-python-path.txt` 中写入 Python 可执行文件的绝对路径；环境变量 `SPEAKY_QWEN_PYTHON` 优先于该文件。

```powershell
$env:SPEAKY_QWEN_PYTHON = 'D:\asr-env\Scripts\python.exe'
$env:SPEAKY_MODELS_DIR = 'D:\ASR-Models'
.\start-windows.cmd
```

启动器会记住已有配置。首次创建配置时优先选择已安装的 Qwen3-ASR-0.6B。用「设置 → 识别 → 检测可用性」可以测试内置的中文录音。「常用词」里的术语可以提示本地 Qwen 和 Whisper 优先考虑相应写法。开启听写预览后，未说完的短句会随新音频修订，遇到停顿时定稿。历史页的「转写」会重新识别整段原始录音。

## 修改转写并自动添加常用词

在首页的「最近转写」或「历史」里点击「改错词」，直接修改文字，然后点「保存」。例如把「我们用 red 做检索」改为「我们用 RAG 做检索」，Speaky 会保存更正后的转写，并将识别出的术语 `RAG` 加入个人词典。改中文人名时，可以在编辑框中选中完整的正确写法再保存。

打开「常用词 → 自动添加」即可查看或移除学到的词。日常听写会将这些词作为词表提示传给支持的模型，例如本地 Qwen 和 Whisper。原始录音仍可在历史中播放；「模型评测」继续使用公开样本和原有评测设置。

## 在模型评测页试听与识别

在左侧打开「模型评测」。选择数据集和录音条目即可播放原声。选择本地模型，再识别当前条目，即可对照人工参考和模型转写。也可以保留这一条录音，切换模型再试。评测页与听写页共用同一个模型选择。

模型评测使用本地测试集。将每组测试集放入程序目录的 `ASR-Tests` 子目录，并为每组提供记录 WAV 路径和参考文本的 `manifest.json`。

## 配置和录音位置

Windows 默认将配置写入 `%APPDATA%\speaky\gpui\config.yaml`，原始 WAV 和对应转写存放在相邻的 `drafts\` 子目录。这与旧 Python 版本的目录独立。如需隔离测试，先将 `SPEAKY_CONFIG_DIR` 指向测试目录，再启动应用。
