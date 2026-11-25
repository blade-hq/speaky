# 在 Speaky 中连接语音识别服务

在「设置 → 识别 → 接入方式」填写服务地址。服务启用了鉴权时，填写对应的 API 密钥。提交后，Speaky 会把内置的两秒中文录音发给该服务，显示检测到的接入方式和返回的文字。成功后，日常听写和历史录音转写都会使用已验证的接口。可在设置中再次检测可用性。

| 接入方式 | 可填写的地址示例 | 模型 ID |
| --- | --- | --- |
| OpenAI 兼容转写 | `http://localhost:8000/v1` 或完整的 `/v1/audio/transcriptions` 地址 | 填写部署的转写模型 ID。如果服务只列出一个模型，可留空。 |
| whisper.cpp 原生 HTTP | `http://localhost:8080` 或 `http://localhost:8080/inference` | 由 whisper-server 加载，可留空。 |
| FunASR WebSocket | `ws://localhost:10095` 或部署方提供的 `wss://` 地址 | 由 FunASR 服务加载，可留空。 |

Speaches 和启用转写模型的 vLLM 共用 OpenAI 兼容入口。服务器使用反向代理时，请保留服务地址中的路径前缀。[Speaches 转写文档](https://speaches.ai/usage/speech-to-text/) · [vLLM 转写文档](https://docs.vllm.ai/en/latest/serving/online_serving/speech_to_text/)

## 在 Windows 电脑上试转写

1. 双击 Speaky 目录里的 `start-asr.cmd`。输入 `1` 启动 sherpa-onnx 的 SenseVoice，或输入 `2` 启动 Vosk 的小型中文模型。看到 `Uvicorn running` 后保持窗口打开。想来回比较时，可以再打开一次，启动另一项。
2. 打开「设置 → 识别 → 接入方式」，填写启动窗口显示的地址。例如 Vosk 预置服务使用 `http://127.0.0.1:18764`。这些脚本都提供 OpenAI 兼容接口，并在模型列表中报告当前模型。
3. 想比较效果时，两次都选同一条样本。想用在聊天里，先点目标输入框，按一次自己的快捷键开始。Speaky 会边听边写，并随上下文修正刚写的文字。再按一次结束，确认文字后可自行发送。

本机菜单中的 `1 sherpa-onnx` 和 `7 FunASR` 使用各自框架支持的 SenseVoice 模型格式。`2 Vosk`、`4 WeNet`、`5 ESPnet` 和 `8 Kaldi` 提供普通话模型。`3 SpeechBrain` 使用 LibriSpeech 英文模型，`6 NeMo` 使用英文 Conformer CTC 模型。试这两项时请用英文听写，也可以用模型目录 `speechbrain-librispeech` 中的 `example.wav` 做对照。

本机验收使用六个中文识别服务分别转写同一个 FLEURS 样本 `fleurs_10040380210557600780.wav`。参考句是“钙、钾等元素属于金属，银和金等元素当然也是金属。”各框架返回了各自模型的结果。两个英文模型用同一个 `example.wav` 转写出了“The birch canoe slid on the smooth planks”。桌面端另外用 Vosk 实测了设置切换、内置样本、历史记录和评测页。

启动菜单显示每个框架加载的模型名称。模型目录和 Python 环境列在 `scripts/asr-presets.json`。sherpa-onnx、Vosk 和 Kaldi 共用 `ASR-Models\bridge-env` 中的 Python，Kaldi 的原生程序可放在 `kaldi-env\Library\bin`。其他框架使用各自的 `ASR-Models\框架ID-env`。如需额外参数，把 `speaky-options.json` 放在模型目录中。启动器会在程序目录和上一级目录寻找 `ASR-Models`，也可在 `SPEAKY_MODELS_DIR` 中用分号列出其他模型目录。迁移到其他电脑时，需要在那里创建 Python 环境并准备模型。按 Ctrl+C 可停止对应服务。

## 部署其他模型，或连接另一台机器

每个框架使用自己的服务进程，可以使用不同的 Python 环境、操作系统和 GPU。在运行模型的机器上安装相应的框架和本地权重，并把便携包的 `scripts` 目录复制过去。在该框架的 Python 环境中运行以下命令。以 FunASR 为例，将模型目录换为实际路径。

```powershell
python -m pip install fastapi uvicorn python-multipart
python scripts/asr_bridge.py --framework funasr --model-dir "D:\models\paraformer" --model-id paraformer-zh
```

在桌面端填入 `http://127.0.0.1:18761/v1` 和 `paraformer-zh`。自部署时可用 `--model-id` 指定模型的真实名称。省略时，内置模型使用预置名称，其他模型使用目录名。模型名称区分大小写。

| 框架 ID | 默认端口 | 此适配器需要的模型与推理环境 |
| --- | ---: | --- |
| `funasr` | 18761 | FunASR 的 `AutoModel`。模型目录含 `configuration.json` 或 `config.yaml` 及 `model.pt`，可用 `checkpoint` 指定其他权重文件。[FunASR](https://github.com/modelscope/FunASR) |
| `sherpa-onnx` | 18762 | `sherpa-onnx` 和 NumPy。支持 SenseVoice 的单文件 ONNX 模型及 `tokens.txt`，可用 `model_file` 指定文件。使用 CPU。[sherpa-onnx SenseVoice](https://k2-fsa.github.io/sherpa/onnx/sense-voice/index.html) |
| `wenet` | 18763 | 官方 WeNet 3.1.0 使用包含 `final.zip` 和 `units.txt` 的 LibTorch 模型目录。提供新版 `load_model` 的 SDK 使用配套的 `train.yaml`、`final.pt`、`units.txt` 和分词文件。[WeNet Python 接口](https://github.com/wenet-e2e/wenet/blob/main/wenet/cli/model.py) |
| `vosk` | 18764 | `vosk`，解压完整 Vosk 模型目录，内含 `am/final.mdl` 或 `final.mdl` 及其解码资源。语言由模型决定，使用 CPU。[Vosk 模型](https://alphacephei.com/vosk/models) |
| `nemo` | 18765 | NVIDIA NeMo ASR 环境，支持 `ASRModel.restore_from` 加载的语音识别模型，默认读取目录中唯一的 `*.nemo`。也可指定 `checkpoint`。[NeMo ASR](https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/intro.html) |
| `espnet` | 18766 | ESPnet2 的 `Speech2Text`，需要训练配置 `config.yaml`、对应的 `*.pth` 权重和模型使用的分词资源。可指定 `config`、`checkpoint`、`bpemodel`，以及全局归一化使用的 `stats_file`。[ESPnet 推理接口](https://github.com/espnet/espnet/blob/master/espnet2/bin/asr_inference.py) |
| `speechbrain` | 18767 | SpeechBrain 1.1.x，准备 `hyperparams.yaml` 与其引用的全部本地权重和分词资源。按模型卡选择 `EncoderDecoderASR`（默认）或 `EncoderASR`。[SpeechBrain 接口](https://speechbrain.readthedocs.io/en/latest/API/speechbrain.inference.ASR.html) |
| `kaldi` | 18768 | 原生 Kaldi nnet3 online 模型，需要 `final.mdl`、`graph/HCLG.fst`、`graph/words.txt`、`conf/online.conf` 及配置中引用的文件。把 `online2-wav-nnet3-latgen-faster`、`lattice-best-path` 加入 PATH，或用 `bin_dir` 指定绝对路径。[Kaldi 在线解码](https://kaldi-asr.org/doc/online_decoding.html) |

框架专用参数可以写入 UTF-8 JSON 文件，通过 `--options 文件路径` 传入。例如 ESPnet 可以使用 `{"checkpoint":"valid.acc.best.pth","beam_size":8,"device":"cpu"}`。模型文件路径相对于 `--model-dir`。单语模型按训练语言识别。桌面听写提交 16 kHz、单声道、16 位 PCM WAV，实时预览按录音片段调用识别。

在其他电脑上提供服务时，在服务端设置 `SPEAKY_ASR_TOKEN`，用 `--host` 指定监听地址，桌面端填写该服务器的地址和相同的密钥。跨公网部署请使用 HTTPS 反向代理。默认仅监听本机。

## 检查连接

点「检测可用性」会用同一段内置录音重新试转写。Speaky 显示耗时与实际文字，并自动核对参考句“你好，语音识别测试。”。标点、空格和这句参考文本的简繁差异均可忽略。要判断较长录音的识别效果，可以在「模型评测」中比较不同样本。

如果连接失败，先看对应的服务窗口是否已打印监听地址，再核对端口、模型 ID 和密钥。若某一框架缺少包或模型文件，启动窗口会显示具体错误；可以切到另一个已启动的框架继续使用。同一条录音可从「历史」重新转写。
