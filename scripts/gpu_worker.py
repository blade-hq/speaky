"""One model per GPU worker, with a Windows Job that owns all its child processes."""

import contextlib
import ctypes as C
from ctypes import wintypes as W
import multiprocessing
import os
from pathlib import Path
import subprocess
import time


def _api(name, result, *args):
    fn = getattr(C.WinDLL("kernel32", use_last_error=True), name)
    fn.restype, fn.argtypes = result, args
    return fn


class _Limits(C.Structure):
    _fields_ = [("process_time", C.c_int64), ("job_time", C.c_int64),
                ("flags", W.DWORD), ("min_set", C.c_size_t), ("max_set", C.c_size_t),
                ("active_limit", W.DWORD), ("affinity", C.c_size_t),
                ("priority", W.DWORD), ("scheduling", W.DWORD)]


class _ExtendedLimits(C.Structure):
    _fields_ = [("basic", _Limits), ("io_counters", C.c_uint64 * 6),
                ("memory_limits", C.c_size_t * 4)]


class _Accounting(C.Structure):
    _fields_ = [("times", C.c_int64 * 4), ("page_faults", W.DWORD),
                ("total_processes", W.DWORD), ("active_processes", W.DWORD),
                ("terminated", W.DWORD)]


def _close(handle):
    _api("CloseHandle", W.BOOL, W.HANDLE)(handle)


class _Job:
    def __init__(self):
        self.handle = _api("CreateJobObjectW", W.HANDLE, C.c_void_p, W.LPCWSTR)(None, None)
        if not self.handle:
            raise C.WinError(C.get_last_error())
        limits = _ExtendedLimits()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not _api("SetInformationJobObject", W.BOOL, W.HANDLE, C.c_int, C.c_void_p,
                    W.DWORD)(self.handle, 9, C.byref(limits), C.sizeof(limits)):
            error = C.WinError(C.get_last_error())
            _close(self.handle)
            raise error

    def assign(self, process):
        if not _api("AssignProcessToJobObject", W.BOOL, W.HANDLE, W.HANDLE)(self.handle, process.sentinel):
            raise C.WinError(C.get_last_error())

    def stop(self):
        if not _api("TerminateJobObject", W.BOOL, W.HANDLE, W.UINT)(self.handle, 0):
            raise C.WinError(C.get_last_error())
        deadline = time.monotonic() + 20
        while True:
            info = _Accounting()
            if not _api("QueryInformationJobObject", W.BOOL, W.HANDLE, C.c_int, C.c_void_p,
                        W.DWORD, C.c_void_p)(self.handle, 1, C.byref(info), C.sizeof(info), None):
                raise C.WinError(C.get_last_error())
            if not info.active_processes:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("上一模型的 GPU 进程尚未退出，请稍后再试。")
            time.sleep(0.05)
        _close(self.handle)
        self.handle = None


def _gpus():
    result = subprocess.run(
        ["nvidia-smi", "--query-gpu=index,uuid,name,memory.free", "--format=csv,noheader,nounits"],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=8,
        creationflags=subprocess.CREATE_NO_WINDOW, check=True)
    rows = []
    for line in result.stdout.splitlines():
        idx, uuid, name, free = (field.strip() for field in line.split(",", 3))
        rows.append((int(idx), uuid, name, int(free)))
    return rows


def _target():
    try:
        rows = _gpus()
    except (OSError, ValueError, subprocess.SubprocessError):
        rows = []
    if not rows:
        raise RuntimeError("无法检测 NVIDIA GPU，请检查显卡驱动和 nvidia-smi。")
    return max(rows, key=lambda row: row[3])


@contextlib.contextmanager
def _exclusive(uuid):
    handle = _api("CreateMutexW", W.HANDLE, C.c_void_p, W.BOOL, W.LPCWSTR)(
        None, False, "Local\\SpeakyASR-" + uuid)
    if not handle:
        raise C.WinError(C.get_last_error())
    try:
        # An abandoned mutex is expected when the old isolated worker was stopped.
        status = _api("WaitForSingleObject", W.DWORD, W.HANDLE, W.DWORD)(handle, 0)
        if status not in (0, 0x80):
            raise RuntimeError("另一 Speaky 实例正在使用此显卡，请先退出该实例。")
        try:
            yield
        finally:
            _api("ReleaseMutex", W.BOOL, W.HANDLE)(handle)
    finally:
        _close(handle)


class PreviewCancelled(Exception):
    pass


class _Qwen:
    def __init__(self, directory, index):
        import torch
        from qwen_asr import Qwen3ASRModel
        from serve_qwen_asr import LANGUAGES
        self.languages = LANGUAGES
        device = torch.device("cuda", index)
        torch.cuda.set_device(device)
        self.device_name = torch.cuda.get_device_name(device)
        self.model = Qwen3ASRModel.from_pretrained(
            str(directory), device_map=str(device), local_files_only=True,
            dtype=torch.bfloat16 if torch.cuda.is_bf16_supported() else torch.float16,
            max_inference_batch_size=1, max_new_tokens=4096)
        if any(p.device != device for p in self.model.model.parameters()):
            raise RuntimeError("Qwen 权重未完整加载到选定的 GPU。")

    def transcribe(self, samples, rate, lang, prompt, preview_cancelled=None):
        if lang != "auto" and lang not in self.languages:
            raise ValueError(f"所选模型不支持此语言：{lang}")
        options = dict(audio=(samples, rate), language=self.languages.get(lang), context=prompt)
        if preview_cancelled is None:
            return self.model.transcribe(**options)[0].text.strip()

        from transformers import StoppingCriteriaList

        # The Qwen ASR wrapper does not forward generation options from
        # transcribe(). Add the stop check at its Transformers generation call
        # for this preview only. The worker processes one request at a time.
        model = self.model.model
        generate = model.generate

        def generate_preview(*args, **kwargs):
            if preview_cancelled.is_set():
                raise PreviewCancelled
            criteria = StoppingCriteriaList(kwargs.pop("stopping_criteria", None) or [])
            criteria.append(lambda *args, **kwargs: preview_cancelled.is_set())
            return generate(*args, stopping_criteria=criteria, **kwargs)

        model.generate = generate_preview
        try:
            return self.model.transcribe(**options)[0].text.strip()
        finally:
            model.generate = generate


def _open_model(model, directory, cuda_index):
    if model.startswith("Qwen3-ASR-"):
        return _Qwen(directory, cuda_index)
    if model.startswith("SenseVoiceSmall"):
        from sense_gpu import SenseGPU
        return SenseGPU(directory, cuda_index)
    from local_models import WHISPER_MODELS
    if model in WHISPER_MODELS:
        from whisper_gpu import WhisperGPU, installed
        weight = WHISPER_MODELS[model]
        if not installed(directory, weight):
            raise RuntimeError("请在 Whisper 模型目录的 runtime-cuda 中安装 whisper.cpp CUDA 版及 Speaky GPU 桥接库。")
        return WhisperGPU(directory, cuda_index, weight)
    raise ValueError("未知的本地语音模型。")


def _child(pipe, name, directory, gpu, preview_cancelled):
    try:
        # Do not load weights or launch children until the parent assigns this
        # process to its kill-on-close Job.
        if pipe.recv() != "load":
            return
        index, uuid, expected_name, _ = gpu
        os.environ["CUDA_DEVICE_ORDER"] = "PCI_BUS_ID"
        os.environ["CUDA_VISIBLE_DEVICES"] = uuid
        os.environ["SPEAKY_GPU_NAME"] = expected_name
        with _exclusive(uuid):
            # CUDA sees just the chosen GPU as its logical device 0.
            runner = _open_model(name, Path(directory), 0)
            if runner.device_name.casefold() != expected_name.casefold():
                raise RuntimeError(f"模型设备与选定显卡不匹配：{runner.device_name} / {expected_name}")
            pipe.send((True, {"device": runner.device_name, "gpu_index": index, "gpu_uuid": uuid}))
            while True:
                preview, args = pipe.recv()
                try:
                    if preview and preview_cancelled.is_set():
                        raise PreviewCancelled
                    if preview and isinstance(runner, _Qwen):
                        text = runner.transcribe(*args, preview_cancelled=preview_cancelled)
                    else:
                        text = runner.transcribe(*args)
                    if preview and preview_cancelled.is_set():
                        raise PreviewCancelled
                    pipe.send((True, text))
                except PreviewCancelled:
                    pipe.send((True, ""))
                except Exception as error:
                    pipe.send((False, str(error)))
    except (EOFError, BrokenPipeError):
        pass
    except Exception as error:
        try:
            pipe.send((False, f"{name} 无法在 GPU 上运行：{error}"))
        except (OSError, EOFError):
            pass
    finally:
        pipe.close()


class Worker:
    def __init__(self, name, directory, cancelled=None):
        check_load(cancelled)
        gpu = _target()
        check_load(cancelled)
        self.name = name
        self.gpu = gpu
        self.info = {}
        self.job = _Job()
        ctx = multiprocessing.get_context("spawn")
        self.preview_cancelled = ctx.Event()
        self.pipe, child_pipe = ctx.Pipe()
        self.process = ctx.Process(
            target=_child, args=(child_pipe, name, directory, gpu, self.preview_cancelled),
            daemon=True,
        )
        try:
            self.process.start()
            self.job.assign(self.process)
            child_pipe.close()
            self.pipe.send("load")
            self.info = self._receive(240, cancelled)
        except BaseException:
            child_pipe.close()
            self.stop()
            raise

    def _receive(self, timeout, cancelled=None):
        deadline = time.monotonic() + timeout
        while True:
            check_load(cancelled)
            remaining = max(0, deadline - time.monotonic())
            interval = remaining if cancelled is None else min(remaining, 0.1)
            if self.pipe.poll(interval):
                check_load(cancelled)
                break
            if time.monotonic() >= deadline:
                raise TimeoutError(f"等待 {self.name} 的 GPU 响应超时。")
        try:
            ok, value = self.pipe.recv()
        except (EOFError, OSError) as error:
            raise RuntimeError(f"{self.name} 的 GPU 工作进程已退出。") from error
        if not ok:
            raise RuntimeError(value)
        return value

    def transcribe(self, *args, preview=False):
        self.pipe.send((preview, args))
        try:
            return self._receive(3600)
        except TimeoutError:
            self.stop()  # discard a late reply before a subsequent request
            raise

    def stop(self):
        if self.job.handle:
            self.job.stop()
        self.pipe.close()
        if self.process.pid is not None:
            if self.process.is_alive():
                self.process.terminate()  # covers the failure to assign a fresh process
            self.process.join(10)
            if self.process.is_alive():
                raise RuntimeError("GPU 工作进程尚未退出。")

    def free_after_stop(self):
        try:
            return next(row[3] for row in _gpus() if row[1] == self.gpu[1])
        except (OSError, ValueError, StopIteration, subprocess.SubprocessError):
            return None


class LoadCancelled(Exception):
    pass


def check_load(cancelled):
    if cancelled is not None and cancelled():
        raise LoadCancelled("已停止加载模型。")
