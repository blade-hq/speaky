use reqwest::Url;

pub const BASE_URL: &str = "https://www.autodl.art/api/v1";
pub(crate) const ORIGIN: &str = "https://www.autodl.art";
pub const TEXT_MODELS: &[&str] = &[
    "DeepSeek-V3.2",
    "DeepSeek-R1-0528",
    "GLM-5.1",
    "GLM-5",
    "MiniMax-M2.5",
    "Qwen3.5-397B-A17B",
    "Kimi-K2.5",
];

pub(crate) fn is_official_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("www.autodl.art")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

pub fn is_base_url(address: &str) -> bool {
    Url::parse(address.trim()).is_ok_and(|url| is_api_base(&url))
}

pub(crate) fn is_api_base(url: &Url) -> bool {
    is_official_url(url) && url.path().trim_end_matches('/') == "/api/v1"
}

pub fn list_models(api_key: &str) -> Result<Vec<String>, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("请填写 AutoDL API Key。".into());
    }
    let endpoint = Url::parse(&format!("{BASE_URL}/models")).expect("valid AutoDL catalog URL");
    let models =
        crate::engines::read_model_catalog(endpoint, api_key, std::time::Duration::from_secs(4))?;
    if models.is_empty() {
        return Err("AutoDL 没有返回可选模型。".into());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmConfig;

    #[test]
    fn shares_only_autodl_keys_and_preserves_the_saved_custom_service() {
        let mut config: LlmConfig = serde_yaml::from_str(
            "openai:\n  base_url: https://www.autodl.art/api/v1/\n  model: legacy-model\n  api_key: old-key\n",
        )
        .unwrap();
        assert_eq!(config.autodl_key(), Some("old-key"));
        assert_eq!(config.organizer_service().base_url, BASE_URL);
        assert_eq!(config.organizer_service().model, "legacy-model");

        config.autodl_api_key = " new-key ".into();
        assert_eq!(config.organizer_service().api_key, "new-key");

        config.openai.base_url = "https://example.org/api/v1".into();
        config.openai.api_key = "custom-key".into();
        assert_eq!(config.organizer_service().api_key, "custom-key");

        config.autodl_model = "DeepSeek-V3.2".into();
        let service = config.organizer_service();
        assert_eq!(service.base_url, BASE_URL);
        assert_eq!(service.api_key, "new-key");
        assert_eq!(service.model, "DeepSeek-V3.2");
        assert_eq!(config.openai.api_key, "custom-key");

        config.autodl_api_key.clear();
        assert!(config.autodl_key().is_none());
        assert!(config.organizer_service().api_key.is_empty());
    }

    #[test]
    fn matches_only_the_official_api_base() {
        assert!(is_base_url(BASE_URL));
        assert!(is_base_url(" https://www.autodl.art/api/v1/ "));
        for invalid in [
            "https://www.autodl.art.evil.com/api/v1",
            "https://www.autodl.art:8443/api/v1",
            "https://user@www.autodl.art/api/v1",
            "https://www.autodl.art/api/v1?query",
            "https://www.autodl.art/api/v1#fragment",
            "http://www.autodl.art/api/v1",
            "https://www.autodl.art/api/v1/chat/completions",
        ] {
            assert!(!is_base_url(invalid), "{invalid}");
        }
    }
}
