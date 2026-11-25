use super::{Config, Pack, RUNTIME};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    time::Duration,
};

const ID: &str = "whisper-base-managed";
const MODEL: &str = "ggml-base.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/fb5ed9369c26eaf08392504d8446da431e0fffb9/ggml-base.bin";
const MODEL_SIZE: u64 = 147_951_465;
const MODEL_SHA256: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";

#[derive(Default)]
pub struct WhisperDownload {
    received: AtomicU64,
    cancelled: AtomicBool,
    cancellation: tokio::sync::Notify,
}

impl WhisperDownload {
    pub fn percent(&self) -> u64 {
        self.received.load(Relaxed) * 100 / MODEL_SIZE
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
        self.cancellation.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Relaxed)
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("已取消下载。".into());
        }
        Ok(())
    }
}

pub fn is_managed_whisper(pack: &Pack) -> bool {
    pack.id == ID
}

pub fn whisper_runtime() -> Option<PathBuf> {
    let executable = format!("whisper-server{}", env::consts::EXE_SUFFIX);
    let app = env::current_exe().ok()?;
    let bundled = app.parent()?.join("whisper-runtime").join(&executable);
    let path = env::var_os("PATH").unwrap_or_default();
    let mut candidates = vec![bundled];
    candidates.extend(
        env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(&executable)),
    );
    #[cfg(target_os = "macos")]
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin"].map(|dir| Path::new(dir).join(&executable)),
    );
    candidates.into_iter().find(|path| path.is_file())
}

pub fn download_whisper(download: &WhisperDownload) -> Result<Pack, String> {
    download.check_cancelled()?;
    whisper_runtime().ok_or("找不到 Whisper 推理程序。")?;
    let config_dir = Config::config_dir();
    let root = config_dir.join("packs");
    let destination = root.join(ID);
    let installed = || Pack::load(&destination).ok().flatten();
    if let Some(pack) = installed() {
        return Ok(pack);
    }

    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let staging = tempfile::Builder::new()
        .prefix(".whisper-download-")
        .tempdir_in(&config_dir)
        .map_err(|error| error.to_string())?;
    let model_path = staging.path().join(MODEL);
    RUNTIME.block_on(async {
        tokio::select! {
            result = fetch_model(&model_path, download) => result,
            _ = download.cancellation.notified() => Err("已取消下载。".into()),
        }
    })?;
    download.check_cancelled()?;

    let manifest = serde_json::json!({
        "schema_version": 1,
        "id": ID,
        "name": "Whisper Base",
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "model_id": "whisper-base",
        "command": [
            "{whisper_runtime}", "--model", MODEL, "--host", "{host}", "--port", "{port}",
            "--language", "auto", "--inference-path", "/v1/audio/transcriptions"
        ]
    });
    fs::write(staging.path().join("manifest.json"), manifest.to_string())
        .map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(staging.path(), &destination) {
        return installed().ok_or_else(|| format!("无法安装模型包：{error}"));
    }
    Pack::load(&destination)?.ok_or_else(|| "模型包与当前系统不兼容。".into())
}

async fn fetch_model(path: &Path, download: &WhisperDownload) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(MODEL_URL)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.without_url().to_string())?;
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut received = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url().to_string())?
    {
        download.check_cancelled()?;
        received += chunk.len() as u64;
        if received > MODEL_SIZE {
            return Err("模型文件大小超出预期。".into());
        }
        file.write_all(&chunk).map_err(|error| error.to_string())?;
        digest.update(&chunk);
        download.received.store(received, Relaxed);
    }
    if received != MODEL_SIZE || format!("{:x}", digest.finalize()) != MODEL_SHA256 {
        return Err("模型文件校验失败，请重新下载。".into());
    }
    file.sync_all().map_err(|error| error.to_string())
}
