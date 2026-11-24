use crate::{
    config::Config,
    engines::{OpenAIEngine, RUNTIME},
};
use parking_lot::Mutex;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs, io,
    net::TcpListener,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const HOST: &str = "127.0.0.1";
static MANAGER: Mutex<Manager> = Mutex::new(Manager { active: None });
static QUITTING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Deserialize)]
struct Manifest {
    schema_version: u32,
    id: String,
    name: String,
    os: Option<String>,
    arch: Option<String>,
    command: Vec<String>,
    #[serde(default = "default_health_path")]
    health_path: String,
    #[serde(default = "default_api_base_path")]
    api_base_path: String,
    model_id: String,
    #[serde(default)]
    use_vocabulary_prompt: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

fn default_health_path() -> String {
    "/health".into()
}

fn default_api_base_path() -> String {
    "/v1".into()
}

#[derive(Clone)]
pub struct Pack {
    pub id: String,
    pub label: String,
    directory: PathBuf,
    manifest: Manifest,
}

impl Pack {
    fn load(directory: &Path) -> Result<Option<Self>, String> {
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
        let source = fs::read(directory.join("manifest.json")).map_err(|e| e.to_string())?;
        let manifest: Manifest = serde_json::from_slice(&source).map_err(|e| e.to_string())?;
        if manifest.schema_version != 1 {
            return Err("离线包需要 schema_version 1。".into());
        }
        if manifest
            .os
            .as_deref()
            .is_some_and(|os| os != std::env::consts::OS)
            || manifest
                .arch
                .as_deref()
                .is_some_and(|arch| arch != std::env::consts::ARCH)
        {
            return Ok(None);
        }
        if [&manifest.id, &manifest.name, &manifest.model_id]
            .iter()
            .any(|value| value.trim().is_empty())
            || manifest
                .command
                .first()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err("离线包需要 id、name、command 和 model_id。".into());
        }
        for path in [&manifest.health_path, &manifest.api_base_path] {
            if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#', '\\']) {
                return Err("离线包的 HTTP 路径无效。".into());
            }
        }
        let pack = Self {
            id: manifest.id.clone(),
            label: manifest.name.clone(),
            directory,
            manifest,
        };
        pack.executable()?;
        Ok(Some(pack))
    }

    fn executable(&self) -> Result<PathBuf, String> {
        let path = Path::new(&self.manifest.command[0]);
        if path.is_absolute() {
            return path
                .is_file()
                .then(|| path.to_owned())
                .ok_or_else(|| format!("找不到离线包的可执行文件：{}", path.display()));
        }
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err("离线包的相对可执行路径必须位于包目录内。".into());
        }
        let executable = self
            .directory
            .join(path)
            .canonicalize()
            .map_err(|e| format!("找不到离线包的可执行文件 {}：{e}", path.display()))?;
        if !executable.starts_with(&self.directory) || !executable.is_file() {
            return Err("离线包的相对可执行路径必须指向包目录内的文件。".into());
        }
        Ok(executable)
    }
}

fn directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let extra = std::env::var_os("SPEAKY_PACKS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let portable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("packs")));
    for path in extra
        .into_iter()
        .chain(portable)
        .chain([Config::config_dir().join("packs")])
    {
        if seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())) {
            directories.push(path);
        }
    }
    directories
}

pub fn prepare_directory() -> Result<PathBuf, String> {
    let mut last_error = None;
    for path in directories() {
        match fs::create_dir_all(&path) {
            Ok(()) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map_or_else(|| "找不到模型包目录。".into(), |error| error.to_string()))
}

pub fn list() -> Result<Vec<Pack>, String> {
    let mut packs: Vec<Pack> = Vec::new();
    let mut error = None;
    for directory in directories() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                error = Some(format!("无法读取 {}：{e}", directory.display()));
                continue;
            }
        };
        let mut entries: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries
            .into_iter()
            .filter(|path| path.join("manifest.json").is_file())
        {
            match Pack::load(&path) {
                Ok(Some(pack)) if !packs.iter().any(|existing| existing.id == pack.id) => {
                    packs.push(pack)
                }
                Ok(_) => {}
                Err(e) => {
                    error = Some(format!("无法读取 {}：{e}", path.display()));
                    log::warn!("{}", error.as_deref().unwrap_or_default());
                }
            }
        }
    }
    if packs.is_empty() {
        if let Some(error) = error {
            return Err(error);
        }
    }
    Ok(packs)
}

pub fn selected_id(config: &Config) -> Option<&str> {
    crate::engines::uses_local_asr(config)
        .then_some(config.engine.local_pack.as_deref())
        .flatten()
        .filter(|id| !id.trim().is_empty())
}

pub fn selected_label(config: &Config) -> String {
    match config.engine.current.as_str() {
        "local" => selected_id(config).map_or_else(
            || "请选择本地模型包".into(),
            |id| {
                list()
                    .ok()
                    .and_then(|packs| packs.into_iter().find(|pack| pack.id == id))
                    .map_or_else(|| format!("{id}（未找到模型包）"), |pack| pack.label)
            },
        ),
        "remote" => config.engine.remote.label(),
        "openai" => config.engine.openai.model.clone(),
        "volc_bigmodel" => "火山引擎".into(),
        _ => "请选择识别服务或模型包".into(),
    }
}

pub fn activate(pack: &Pack) -> Result<OpenAIEngine, String> {
    MANAGER.lock().activate(pack, &QUITTING)
}

pub fn ready_engine(config: &Config) -> Result<OpenAIEngine, String> {
    let id = selected_id(config).ok_or("请先选择本地模型包。")?;
    let mut manager = MANAGER.lock();
    if let Some(engine) = manager.running_engine(id)? {
        return Ok(engine);
    }
    let pack = list()?
        .into_iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| format!("找不到本地模型包 {id}，请检查 packs 目录。"))?;
    manager.activate(&pack, &QUITTING)
}

pub fn release() -> Result<(), String> {
    MANAGER.lock().release()
}

pub fn shutdown() -> Result<(), String> {
    QUITTING.store(true, Ordering::Relaxed);
    release()
}

#[derive(Default)]
struct Manager {
    active: Option<ActivePack>,
}

impl Manager {
    fn running_engine(&mut self, id: &str) -> Result<Option<OpenAIEngine>, String> {
        if let Some(active) = self.active.as_mut().filter(|active| active.id == id) {
            if active
                .process
                .child
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Ok(Some(active.engine.clone()));
            }
        }
        Ok(None)
    }

    fn activate(&mut self, pack: &Pack, quitting: &AtomicBool) -> Result<OpenAIEngine, String> {
        if quitting.load(Ordering::Relaxed) {
            return Err("Speaky 正在退出。".into());
        }
        if let Some(engine) = self.running_engine(&pack.id)? {
            return Ok(engine);
        }
        let executable = pack.executable()?;
        self.release()?;

        let reservation = TcpListener::bind((HOST, 0)).map_err(|e| e.to_string())?;
        let port = reservation
            .local_addr()
            .map_err(|e| e.to_string())?
            .port()
            .to_string();
        let pack_dir = pack.directory.to_string_lossy();
        let expand = |value: &str| {
            value
                .replace("{host}", HOST)
                .replace("{port}", &port)
                .replace("{pack_dir}", &pack_dir)
        };
        let mut command = Command::new(executable);
        command
            .current_dir(&pack.directory)
            .args(pack.manifest.command.iter().skip(1).map(|arg| expand(arg)))
            .envs(
                pack.manifest
                    .env
                    .iter()
                    .map(|(name, value)| (name, expand(value))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let origin = format!("http://{HOST}:{port}");
        let health = format!("{origin}{}", pack.manifest.health_path);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .map_err(|e| e.to_string())?;
        drop(reservation);
        let mut process = Process::spawn(&mut command)
            .map_err(|e| format!("无法启动本地模型包 {}：{e}", pack.label))?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if quitting.load(Ordering::Relaxed) {
                return Err("Speaky 正在退出。".into());
            }
            if let Some(status) = process.child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("本地模型包 {} 提前退出（{status}）。", pack.label));
            }
            if RUNTIME
                .block_on(async { client.get(&health).send().await })
                .is_ok_and(|r| r.status().is_success())
                && process
                    .child
                    .try_wait()
                    .map_err(|e| e.to_string())?
                    .is_none()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("等待本地模型包 {} 就绪超时。", pack.label));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let engine = OpenAIEngine::with_timeout(
            "",
            &pack.manifest.model_id,
            &format!("{origin}{}", pack.manifest.api_base_path),
            Duration::from_secs(3600),
        )
        .with_vocabulary_prompt(pack.manifest.use_vocabulary_prompt);
        self.active = Some(ActivePack {
            id: pack.id.clone(),
            process,
            engine: engine.clone(),
        });
        Ok(engine)
    }

    fn release(&mut self) -> Result<(), String> {
        if let Some(active) = &mut self.active {
            active.process.stop().map_err(|e| e.to_string())?;
            self.active = None;
        }
        Ok(())
    }
}

struct ActivePack {
    id: String,
    process: Process,
    engine: OpenAIEngine,
}

struct Process {
    child: Child,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

impl Process {
    #[cfg(not(windows))]
    fn spawn(command: &mut Command) -> io::Result<Self> {
        Ok(Self {
            child: command.spawn()?,
        })
    }

    #[cfg(windows)]
    fn spawn(command: &mut Command) -> io::Result<Self> {
        use std::os::windows::{
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::CommandExt,
        };
        use windows_sys::Win32::System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED},
        };
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // Assign the job before the executable can start any descendants.
        let mut child = command
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
            .spawn()?;
        let assigned =
            unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        let result = if assigned == 0 {
            Err(io::Error::last_os_error())
        } else {
            Self::resume(child.id())
        };
        if let Err(error) = result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self { child, job })
    }

    #[cfg(windows)]
    fn resume(pid: u32) -> io::Result<()> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                    THREADENTRY32,
                },
                Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
            },
        };
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut entry = THREADENTRY32::default();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::last_os_error())
    }

    fn stop(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::{
                JobObjectBasicAccountingInformation, QueryInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            };
            if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                if unsafe {
                    QueryInformationJobObject(
                        self.job.as_raw_handle(),
                        JobObjectBasicAccountingInformation,
                        (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        std::mem::size_of_val(&info) as u32,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if info.ActiveProcesses == 0 {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "模型包进程未退出。",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        #[cfg(not(windows))]
        if self.child.try_wait()?.is_none() {
            if let Err(error) = self.child.kill() {
                if self.child.try_wait()?.is_none() {
                    return Err(error);
                }
            }
        }
        self.child.wait().map(|_| ())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::Engine;
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest() -> Value {
        json!({
            "schema_version": 1,
            "id": "test-pack",
            "name": "Test Pack",
            "command": ["runtime/server.exe"],
            "model_id": "mock-asr"
        })
    }

    fn load(directory: &Path, data: &Value) -> Result<Option<Pack>, String> {
        fs::write(directory.join("manifest.json"), data.to_string()).unwrap();
        Pack::load(directory)
    }

    #[test]
    fn manifest_defaults_and_requires_supported_schema_and_nonempty_fields() {
        let dir = TempDir::new();
        fs::create_dir(dir.0.join("runtime")).unwrap();
        fs::write(dir.0.join("runtime/server.exe"), []).unwrap();
        let mut data = manifest();
        let pack = load(&dir.0, &data).unwrap().unwrap();
        assert_eq!(pack.id, "test-pack");
        assert_eq!(pack.manifest.health_path, "/health");
        assert_eq!(pack.manifest.api_base_path, "/v1");
        assert!(!pack.manifest.use_vocabulary_prompt);
        for required in ["schema_version", "id", "name", "command", "model_id"] {
            let mut invalid = data.clone();
            invalid.as_object_mut().unwrap().remove(required);
            assert!(load(&dir.0, &invalid).is_err(), "missing {required}");
        }
        data["schema_version"] = json!(2);
        assert!(load(&dir.0, &data).is_err());
        data["schema_version"] = json!(1);
        data["command"] = json!([]);
        assert!(load(&dir.0, &data).is_err());
    }

    #[test]
    fn only_lists_packs_for_the_running_os_and_arch() {
        let dir = TempDir::new();
        let mut data = manifest();
        data["os"] = json!("wrong-os");
        data["arch"] = json!(std::env::consts::ARCH);
        assert!(load(&dir.0, &data).unwrap().is_none());
        data["os"] = json!(std::env::consts::OS);
        data["arch"] = json!("wrong-arch");
        assert!(load(&dir.0, &data).unwrap().is_none());
        data["arch"] = json!(std::env::consts::ARCH);
        data["command"] = json!([std::env::current_exe().unwrap()]);
        assert!(load(&dir.0, &data).unwrap().is_some());
    }

    #[test]
    fn rejects_missing_files_and_relative_executable_escape() {
        let dir = TempDir::new();
        let pack = dir.0.join("pack");
        fs::create_dir(&pack).unwrap();
        fs::write(dir.0.join("outside.exe"), []).unwrap();
        let mut data = manifest();
        assert!(load(&pack, &data).is_err());
        data["command"] = json!(["../outside.exe"]);
        assert!(load(&pack, &data).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.0.join("outside.exe"), pack.join("alias")).unwrap();
            data["command"] = json!(["alias"]);
            assert!(load(&pack, &data).is_err());
        }
    }

    #[test]
    fn starts_probes_reuses_switches_and_releases_a_local_server() {
        let dir = TempDir::new();
        let executable = std::env::current_exe().unwrap();
        let mut data = manifest();
        data["command"] = json!([
            executable,
            "--exact",
            "model_packs::tests::mock_server",
            "--skip={port}"
        ]);
        data["health_path"] = json!("/ready");
        data["api_base_path"] = json!("/api");
        data["use_vocabulary_prompt"] = json!(true);
        data["env"] = json!({
            "SPEAKY_TEST_SERVER": "{host}:{port}",
            "SPEAKY_TEST_PACK": "{pack_dir}",
            "SPEAKY_TEST_MODEL": "first",
        });
        let first = load(&dir.0, &data).unwrap().unwrap();
        let mut manager = Manager::default();
        let quitting = AtomicBool::new(false);
        let transcribe = |engine: &OpenAIEngine| {
            engine.transcribe_with_vocabulary(
                b"audio",
                "zh",
                &["测试词"],
                crate::engines::TranscriptionPass::Final,
            )
        };
        let first_engine = manager.activate(&first, &quitting).unwrap();
        assert_eq!(transcribe(&first_engine).unwrap(), "first");
        let first_pid = manager.active.as_ref().unwrap().process.child.id();
        manager.activate(&first, &quitting).unwrap();
        assert_eq!(
            manager.active.as_ref().unwrap().process.child.id(),
            first_pid
        );

        data["id"] = json!("second");
        data["env"]["SPEAKY_TEST_MODEL"] = json!("second");
        let second = load(&dir.0, &data).unwrap().unwrap();
        let second_engine = manager.activate(&second, &quitting).unwrap();
        assert_eq!(transcribe(&second_engine).unwrap(), "second");
        assert_ne!(
            first_pid,
            manager.active.as_ref().unwrap().process.child.id()
        );
        // The OS may reuse the first port after the old process has exited.
        assert_ne!(transcribe(&first_engine).ok().as_deref(), Some("first"));
        manager.release().unwrap();
        assert!(manager.active.is_none());
        assert!(transcribe(&second_engine).is_err());

        data["id"] = json!("broken");
        data["env"]["SPEAKY_TEST_MODEL"] = json!("exit");
        let broken = load(&dir.0, &data).unwrap().unwrap();
        assert!(manager
            .activate(&broken, &quitting)
            .unwrap_err()
            .contains("提前退出"));
        assert!(manager.active.is_none());
    }

    // Reuse this test executable as a portable HTTP server fixture.
    #[test]
    fn mock_server() {
        let Ok(address) = std::env::var("SPEAKY_TEST_SERVER") else {
            return;
        };
        let model = std::env::var("SPEAKY_TEST_MODEL").unwrap();
        if model == "exit" {
            std::process::exit(42);
        }
        let pack = PathBuf::from(std::env::var_os("SPEAKY_TEST_PACK").unwrap());
        assert_eq!(
            pack,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        assert!(std::env::args()
            .any(|arg| arg == format!("--skip={}", address.split_once(':').unwrap().1)));
        let listener = TcpListener::bind(address).unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut payload = vec![0; length];
            reader.read_exact(&mut payload).unwrap();
            let (status, body) = if request.starts_with("GET /ready ") {
                (200, "")
            } else if request.starts_with("POST /api/audio/transcriptions ")
                && String::from_utf8_lossy(&payload).contains("测试词")
            {
                (200, model.as_str())
            } else {
                (404, "")
            };
            write!(
                stream,
                "HTTP/1.1 {status} Mock\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    }
}
