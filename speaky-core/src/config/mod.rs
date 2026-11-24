use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

/// Configuration-related errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Config file not found at {path}")]
    NotFound { path: PathBuf },

    #[error("Failed to read config file: {source}")]
    ReadError { source: std::io::Error },

    #[error("Failed to parse config YAML: {source}")]
    ParseError { source: serde_yaml::Error },

    #[error("Failed to write config file: {source}")]
    WriteError { source: std::io::Error },

    #[error("Config directory creation failed: {source}")]
    DirCreationError { source: std::io::Error },

    #[error("Invalid config value: {message}")]
    ValidationError { message: String },
}

impl ConfigError {
    fn read_error(source: std::io::Error) -> Self {
        ConfigError::ReadError { source }
    }

    fn parse_error(source: serde_yaml::Error) -> Self {
        ConfigError::ParseError { source }
    }

    fn write_error(source: std::io::Error) -> Self {
        ConfigError::WriteError { source }
    }

    fn dir_creation_error(source: std::io::Error) -> Self {
        ConfigError::DirCreationError { source }
    }

    fn validation_error(message: String) -> Self {
        ConfigError::ValidationError { message }
    }
}

/// ASR configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_toggle_hotkey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_toggle_hotkey: Option<String>,
    #[serde(default = "default_hold_time")]
    pub hotkey_hold_time: f64,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub audio_device: Option<u32>,
    /// Stable device label used to recover the selection when ALSA/PipeWire
    /// renumbers devices after a reconnect or audio-service restart.
    #[serde(default)]
    pub audio_device_name: Option<String>,
    #[serde(default = "default_audio_gain")]
    pub audio_gain: f64,
    #[serde(default = "default_sound_notification")]
    pub sound_notification: bool,
    #[serde(default)]
    pub llm_polish: bool,
}

fn default_hotkey() -> String {
    "ctrl".to_string()
}
fn default_hold_time() -> f64 {
    1.0
}
fn default_language() -> String {
    "auto".to_string()
}
fn default_audio_gain() -> f64 {
    1.0
}
fn default_sound_notification() -> bool {
    true
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            macos_toggle_hotkey: None,
            windows_toggle_hotkey: None,
            hotkey_hold_time: default_hold_time(),
            language: default_language(),
            audio_device: None,
            audio_device_name: None,
            audio_gain: default_audio_gain(),
            sound_notification: default_sound_notification(),
            llm_polish: false,
        }
    }
}

/// OpenAI-compatible text model used only for optional recognition polishing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmOpenAIConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,
}

fn default_llm_model() -> String {
    "gpt-4o-mini".to_string()
}

impl Default for LlmOpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_llm_model(),
            base_url: default_openai_base_url(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub learn_preferences: bool,
    #[serde(default)]
    pub custom_prompt: String,
    #[serde(default)]
    pub openai: LlmOpenAIConfig,
}

impl LlmConfig {
    pub fn learns_preferences(&self) -> bool {
        self.enabled && self.learn_preferences && self.custom_prompt.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    #[serde(default = "desktop_default_on")]
    pub auto_start: bool,
    #[serde(default = "desktop_default_on")]
    pub windows_floating_button: bool,
    #[serde(default)]
    pub preload_local_model: bool,
}

fn desktop_default_on() -> bool {
    true
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            auto_start: desktop_default_on(),
            windows_floating_button: desktop_default_on(),
            preload_local_model: false,
        }
    }
}

impl AsrConfig {
    /// Validate ASR configuration
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate hotkey is not empty
        if self.hotkey.is_empty() {
            return Err(ConfigError::validation_error(
                "Hotkey cannot be empty".to_string(),
            ));
        }

        // Validate hold time is positive
        if self.hotkey_hold_time <= 0.0 {
            return Err(ConfigError::validation_error(format!(
                "Hold time must be positive, got {}",
                self.hotkey_hold_time
            )));
        }

        // Validate audio gain range
        if !(0.1..=10.0).contains(&self.audio_gain) {
            warn!(
                "Audio gain {} outside typical range [0.1, 10.0]",
                self.audio_gain
            );
        }

        Ok(())
    }
}

/// Core configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreConfig {
    #[serde(default)]
    pub asr: AsrConfig,
}

/// Volcengine BigModel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolcBigModelConfig {
    /// New console authentication (X-Api-Key).
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub app_key: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default = "default_volc_resource_id")]
    pub resource_id: String,
}

fn default_volc_resource_id() -> String {
    "volc.bigasr.sauc.duration".to_string()
}

impl Default for VolcBigModelConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            app_key: String::new(),
            access_key: String::new(),
            resource_id: default_volc_resource_id(),
        }
    }
}

/// OpenAI configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_openai_model")]
    pub model: String,
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_vocabulary_prompt: bool,
}

fn default_openai_model() -> String {
    "gpt-4o-transcribe".to_string()
}
fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_openai_model(),
            base_url: default_openai_base_url(),
            use_vocabulary_prompt: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteService {
    pub address: String,
    pub endpoint: String,
    pub protocol: crate::engines::Protocol,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

impl RemoteService {
    fn is_empty(&self) -> bool {
        self.address.is_empty() && self.endpoint.is_empty()
    }

    pub fn label(&self) -> String {
        if self.model.is_empty() {
            self.protocol.label().to_owned()
        } else {
            self.model.clone()
        }
    }
}

/// Engine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    #[serde(default = "default_engine")]
    pub current: String,
    #[serde(default)]
    pub volc_bigmodel: VolcBigModelConfig,
    #[serde(default)]
    pub openai: OpenAIConfig,
    #[serde(default, skip_serializing_if = "RemoteService::is_empty")]
    pub remote: RemoteService,
    /// Last selected on-device recognizer, kept separately from external services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_pack: Option<String>,
}

fn default_engine() -> String {
    "volc_bigmodel".to_string()
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            current: default_engine(),
            volc_bigmodel: VolcBigModelConfig::default(),
            openai: OpenAIConfig::default(),
            remote: RemoteService::default(),
            local_pack: None,
        }
    }
}

/// Appearance configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_ui_language")]
    pub ui_language: String,
    #[serde(default = "default_show_waveform")]
    pub show_waveform: bool,
    #[serde(default = "default_window_opacity")]
    pub window_opacity: f64,
}

fn default_theme() -> String {
    "auto".to_string()
}
fn default_ui_language() -> String {
    "zh-CN".to_string()
}
fn default_show_waveform() -> bool {
    true
}
fn default_window_opacity() -> f64 {
    0.9
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            ui_language: default_ui_language(),
            show_waveform: default_show_waveform(),
            window_opacity: default_window_opacity(),
        }
    }
}

/// Main configuration struct
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub core: CoreConfig,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub desktop: DesktopConfig,
}

impl Config {
    /// Validate entire configuration
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.core.asr.validate()?;
        Ok(())
    }
    /// Get the config directory path
    pub fn config_dir() -> PathBuf {
        if let Some(directory) = std::env::var_os("SPEAKY_CONFIG_DIR").filter(|p| !p.is_empty()) {
            return PathBuf::from(directory);
        }
        let directory = user_config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("speaky");
        // The legacy Windows client uses the same case-insensitive directory
        // for an incompatible config.yaml and history.json.
        #[cfg(windows)]
        let directory = directory.join("gpui");
        directory
    }

    /// Get the config file path
    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.yaml")
    }

    /// Load configuration from file
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path();
        info!("Loading config from {:?}", path);

        if !path.exists() {
            info!("Config file not found, using defaults");
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path).map_err(|e| ConfigError::read_error(e))?;
        let config: Config =
            serde_yaml::from_str(&content).map_err(|e| ConfigError::parse_error(e))?;
        info!("Config loaded successfully");
        Ok(config)
    }

    /// Save configuration to file
    pub fn save(&self) -> Result<(), ConfigError> {
        let dir = Self::config_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir).map_err(|e| ConfigError::dir_creation_error(e))?;
        }

        let path = Self::config_path();
        info!("Saving config to {:?}", path);

        let content = serde_yaml::to_string(self).map_err(|e| ConfigError::parse_error(e))?;
        crate::drafts::write_atomically(&path, content.as_bytes())
            .map_err(ConfigError::write_error)?;
        info!("Config saved successfully");
        Ok(())
    }
}

fn user_config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{
            FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        };
        if let Ok(raw) =
            unsafe { SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None) }
        {
            let path = PathBuf::from(OsString::from_wide(unsafe { raw.as_wide() }));
            unsafe { CoTaskMemFree(Some(raw.0.cast())) };
            return Some(path);
        }
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(|home| PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
    }
}
