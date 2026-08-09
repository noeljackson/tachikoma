//! Configuration for the Tachikoma daemon.
//!
//! The JSON file describes both durable paths and adapter settings. Adapter
//! settings can be reloaded while the daemon is running; listener and storage
//! paths are resolved at startup and require a restart to take effect.

use anyhow::{Context, Result};
use pi_providers::{ModelSpec, builtin_models, find_model, merge_models};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::SystemTime,
};

const DEFAULT_LISTEN: &str = "127.0.0.1:7447";
const DEFAULT_POLL_SECONDS: u64 = 60;
const DEFAULT_OPEN_SNITCH_LIMIT: usize = 100;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// SQLite state path. Omit to use the XDG state default.
    pub database: Option<PathBuf>,
    /// Connect/gRPC Unix socket path. Omit to use the XDG runtime default.
    pub rpc_socket: Option<PathBuf>,
    /// Loopback-only web address by default.
    pub listen: Option<String>,
    pub opensnitch: OpenSnitchConfig,
    pub ai: AiConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct OpenSnitchConfig {
    /// A configured history path is not read unless this remains true.
    pub enabled: bool,
    /// OpenSnitch UI history database. Null or omitted disables the adapter.
    pub history: Option<PathBuf>,
    /// Seconds between history scans.
    pub poll_seconds: u64,
    /// Maximum number of recent rows read per scan.
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiConfig {
    pub enabled: bool,
    /// Enables durable proposal investigation chats.
    pub investigations: bool,
    pub max_concurrency: usize,
    pub request_timeout_seconds: u64,
    pub max_tool_calls: usize,
    pub max_tool_output_bytes: usize,
    pub redactions: Vec<String>,
    /// Qualified model reference or `auto`.
    pub default_model: String,
    /// Qualified model references attempted by `auto`, in order.
    pub model_preference: Vec<String>,
    pub disabled_models: Vec<String>,
    /// Custom models override compiled catalog entries with the same provider/id.
    pub models: Vec<AiModelConfig>,
    /// Canonical JSONL transcript directory. Omit for the XDG state default.
    pub sessions_dir: Option<PathBuf>,
    /// Legacy provider ids attempted by old review profiles that use `auto`.
    /// Provider ids attempted by review profiles that use `auto`.
    pub provider_preference: Vec<String>,
    /// Legacy provider definitions retained for config compatibility.
    pub providers: BTreeMap<String, AiProviderConfig>,
    pub tools: Vec<AiToolConfig>,
    pub reviews: Vec<AiReviewConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiProviderConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AiModelConfig {
    #[serde(flatten)]
    pub model: ModelSpec,
    /// Optional environment variable for custom/compatible endpoints.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiToolConfig {
    pub id: String,
    pub description: String,
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiReviewConfig {
    pub id: String,
    pub adapter: String,
    pub action: String,
    pub provider: String,
    pub prompt: String,
    pub tools: Vec<String>,
    pub automatic: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub database: Option<PathBuf>,
    pub rpc_socket: Option<PathBuf>,
    pub listen: Option<SocketAddr>,
    pub opensnitch_history: Option<PathBuf>,
    pub poll_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub database: PathBuf,
    pub rpc_socket: PathBuf,
    pub listen: SocketAddr,
    pub opensnitch: OpenSnitchConfig,
    pub ai: AiConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    modified: Option<SystemTime>,
    length: Option<u64>,
}

impl Default for OpenSnitchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history: None,
            poll_seconds: DEFAULT_POLL_SECONDS,
            limit: DEFAULT_OPEN_SNITCH_LIMIT,
        }
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            investigations: true,
            max_concurrency: 2,
            request_timeout_seconds: 90,
            max_tool_calls: 4,
            max_tool_output_bytes: 65_536,
            redactions: Vec::new(),
            default_model: "auto".into(),
            model_preference: vec![
                "openai-codex/gpt-5.4".into(),
                "anthropic/claude-sonnet-4-6".into(),
                "zai-coding/glm-5.1".into(),
                "kimi-coding/kimi-for-coding".into(),
            ],
            disabled_models: Vec::new(),
            models: Vec::new(),
            sessions_dir: None,
            provider_preference: vec![
                "codex".into(),
                "claude-code".into(),
                "openai".into(),
                "anthropic".into(),
            ],
            providers: BTreeMap::new(),
            tools: Vec::new(),
            reviews: Vec::new(),
        }
    }
}

impl Default for AiToolConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            description: String::new(),
            program: PathBuf::new(),
            argv: Vec::new(),
            timeout_seconds: 5,
        }
    }
}

impl OpenSnitchConfig {
    pub fn active(&self) -> bool {
        self.enabled && self.history.is_some()
    }

    fn normalized(mut self) -> Self {
        self.poll_seconds = self.poll_seconds.max(1);
        self.limit = self.limit.max(1);
        self
    }
}

impl AiConfig {
    pub fn normalized(mut self) -> Self {
        self.max_concurrency = self.max_concurrency.clamp(1, 8);
        self.request_timeout_seconds = self.request_timeout_seconds.clamp(1, 900);
        self.max_tool_calls = self.max_tool_calls.clamp(1, 16);
        self.max_tool_output_bytes = self.max_tool_output_bytes.clamp(1_024, 1_048_576);
        for tool in &mut self.tools {
            tool.timeout_seconds = tool.timeout_seconds.clamp(1, 60);
        }
        self
    }

    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let provider_ids = self
            .providers
            .keys()
            .collect::<std::collections::HashSet<_>>();
        let tool_ids = self
            .tools
            .iter()
            .map(|tool| tool.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut review_ids = std::collections::HashSet::new();
        for configured in &self.models {
            if configured.model.provider.trim().is_empty()
                || configured.model.id.trim().is_empty()
                || configured.model.name.trim().is_empty()
                || configured
                    .api_key_env
                    .as_deref()
                    .is_some_and(|name| name.trim().is_empty())
            {
                anyhow::bail!(
                    "custom AI models require provider, id, name, and a non-empty api_key_env when set"
                );
            }
        }
        for provider in self.providers.values() {
            if provider.base_url.is_empty()
                || provider.model.is_empty()
                || provider.api_key_env.is_empty()
            {
                anyhow::bail!("AI providers require base_url, model, and api_key_env");
            }
        }
        let models = self.available_models();
        for reference in &self.model_preference {
            if find_model(&models, reference).is_none() {
                anyhow::bail!("unknown AI model preference: {reference}");
            }
        }
        if self.default_model != "auto" && find_model(&models, &self.default_model).is_none() {
            anyhow::bail!("unknown default AI model: {}", self.default_model);
        }
        for tool in &self.tools {
            if tool.id.is_empty() || tool.description.is_empty() || !tool.program.is_absolute() {
                anyhow::bail!("AI tools require an id, description, and absolute program path");
            }
        }
        for review in &self.reviews {
            if review.id.is_empty()
                || review.adapter.is_empty()
                || review.action.is_empty()
                || review.provider.is_empty()
                || review.prompt.is_empty()
                || (!matches!(
                    review.provider.as_str(),
                    "auto" | "codex" | "claude" | "claude-code" | "openai" | "anthropic"
                ) && !provider_ids.contains(&review.provider)
                    && find_model(&models, &review.provider).is_none())
            {
                anyhow::bail!("AI reviews require valid id, adapter, action, provider, and prompt");
            }
            if !review_ids.insert(review.id.as_str()) {
                anyhow::bail!("duplicate AI review id: {}", review.id);
            }
            for tool in &review.tools {
                if !tool_ids.contains(tool.as_str()) {
                    anyhow::bail!("AI review {} references unknown tool {}", review.id, tool);
                }
            }
        }
        Ok(())
    }

    pub fn available_models(&self) -> Vec<ModelSpec> {
        merge_models(
            builtin_models(),
            self.models
                .iter()
                .map(|model| model.model.clone())
                .collect(),
        )
        .into_iter()
        .filter(|model| {
            !self
                .disabled_models
                .iter()
                .any(|disabled| disabled == &model.qualified_name() || disabled == &model.id)
        })
        .collect()
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.sessions_dir
            .clone()
            .unwrap_or_else(default_investigations_dir)
    }

    pub fn compatibility_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.providers.is_empty() {
            warnings.push("ai.providers is deprecated; use ai.models and standard credential environment variables".into());
        }
        if self.provider_preference != vec!["codex", "claude-code", "openai", "anthropic"] {
            warnings.push("ai.provider_preference is deprecated; use ai.model_preference".into());
        }
        if self
            .reviews
            .iter()
            .any(|review| !review.provider.contains('/') && review.provider != "auto")
        {
            warnings.push(
                "review.provider aliases are deprecated; use a qualified provider/model reference"
                    .into(),
            );
        }
        warnings
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("tachikoma")
            .join("config.json")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("read Tachikoma config {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("parse Tachikoma config {}", path.display()))
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("parse Tachikoma config {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(error).with_context(|| format!("read Tachikoma config {}", path.display()))
            }
        }
    }

    pub fn resolve(&self, overrides: &Overrides) -> Result<ResolvedConfig> {
        let listen_text = overrides
            .listen
            .map(|address| address.to_string())
            .or_else(|| self.listen.clone())
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen = listen_text
            .parse()
            .with_context(|| format!("parse Tachikoma listen address {listen_text:?}"))?;

        let history_override = overrides.opensnitch_history.clone();
        let opensnitch = OpenSnitchConfig {
            enabled: if history_override.is_some() {
                true
            } else {
                self.opensnitch.enabled
            },
            history: history_override.or_else(|| self.opensnitch.history.clone()),
            poll_seconds: overrides
                .poll_seconds
                .unwrap_or(self.opensnitch.poll_seconds),
            limit: self.opensnitch.limit,
        }
        .normalized();
        let ai = self.ai.clone().normalized();
        ai.validate()?;
        for warning in ai.compatibility_warnings() {
            tracing::warn!(%warning, "deprecated Tachikoma AI configuration");
        }

        Ok(ResolvedConfig {
            database: overrides
                .database
                .clone()
                .or_else(|| self.database.clone())
                .unwrap_or_else(default_database),
            rpc_socket: overrides
                .rpc_socket
                .clone()
                .or_else(|| self.rpc_socket.clone())
                .unwrap_or_else(default_rpc_socket),
            listen,
            opensnitch,
            ai,
        })
    }
}

pub fn default_database() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("tachikoma")
        .join("tachikoma.sqlite3")
}

pub fn default_rpc_socket() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| dirs::state_dir().unwrap_or_else(|| PathBuf::from(".local/state")))
        .join("tachikoma")
        .join("tachikoma.sock")
}

pub fn default_investigations_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("tachikoma")
        .join("investigations")
}

pub fn fingerprint(path: &Path) -> std::io::Result<FileFingerprint> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(FileFingerprint {
            modified: metadata.modified().ok(),
            length: Some(metadata.len()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileFingerprint {
            modified: None,
            length: None,
        }),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr, path::PathBuf, time::SystemTime};

    use super::{AiConfig, AiReviewConfig, Config, OpenSnitchConfig, Overrides};

    #[test]
    fn json_config_resolves_defaults_and_adapter_settings() {
        let config: Config = serde_json::from_str(
            r#"{
                "listen": "127.0.0.1:7555",
                "opensnitch": {
                    "history": "/tmp/opensnitch.sqlite3",
                    "poll_seconds": 0,
                    "limit": 0
                }
            }"#,
        )
        .expect("config parses");

        let resolved = config
            .resolve(&Overrides::default())
            .expect("config resolves");
        assert_eq!(
            resolved.listen,
            "127.0.0.1:7555".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            resolved.opensnitch.history,
            Some(PathBuf::from("/tmp/opensnitch.sqlite3"))
        );
        assert_eq!(resolved.opensnitch.poll_seconds, 1);
        assert_eq!(resolved.opensnitch.limit, 1);
        assert!(resolved.opensnitch.active());
    }

    #[test]
    fn command_line_overrides_take_precedence() {
        let config = Config {
            database: Some(PathBuf::from("config.sqlite3")),
            rpc_socket: None,
            listen: Some("127.0.0.1:7555".into()),
            opensnitch: OpenSnitchConfig {
                enabled: false,
                history: Some(PathBuf::from("config.sqlite3")),
                poll_seconds: 60,
                limit: 100,
            },
            ai: AiConfig::default(),
        };
        let resolved = config
            .resolve(&Overrides {
                database: Some(PathBuf::from("override.sqlite3")),
                listen: Some("127.0.0.1:7666".parse().unwrap()),
                opensnitch_history: Some(PathBuf::from("override.sqlite3")),
                poll_seconds: Some(5),
                ..Default::default()
            })
            .expect("config resolves");

        assert_eq!(resolved.database, PathBuf::from("override.sqlite3"));
        assert_eq!(
            resolved.listen,
            "127.0.0.1:7666".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            resolved.opensnitch.history,
            Some(PathBuf::from("override.sqlite3"))
        );
        assert_eq!(resolved.opensnitch.poll_seconds, 5);
        assert!(resolved.opensnitch.active());
    }

    #[test]
    fn missing_config_loads_as_defaults() {
        let path = std::env::temp_dir().join(format!(
            "tachikoma-config-test-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = Config::load_or_default(&path).expect("missing config is optional");
        assert_eq!(config, Config::default());
        fs::remove_file(path).ok();
    }

    #[test]
    fn auto_provider_profile_can_use_local_login_without_api_keys() {
        let mut config = Config::default();
        config.ai.enabled = true;
        config.ai.reviews = vec![AiReviewConfig {
            id: "review-denial".into(),
            adapter: "opensnitch".into(),
            action: "review_denied_connection".into(),
            provider: "auto".into(),
            prompt: "classify this denial".into(),
            ..Default::default()
        }];
        assert!(config.resolve(&Overrides::default()).is_ok());

        config.ai.model_preference = vec!["not-a-provider/not-a-model".into()];
        assert!(config.resolve(&Overrides::default()).is_err());
    }

    #[test]
    fn documented_config_supports_custom_anthropic_compatible_models() {
        let mut config: Config =
            serde_json::from_str(include_str!("../contrib/tachikoma/config.example.json"))
                .expect("documented config parses");
        config.ai.enabled = true;
        let resolved = config
            .resolve(&Overrides::default())
            .expect("documented config validates when enabled");
        let custom = resolved
            .ai
            .available_models()
            .into_iter()
            .find(|model| model.provider == "anthropic-compatible")
            .expect("custom endpoint is in the merged catalog");
        assert_eq!(custom.api, pi_ai::ProviderApi::Anthropic);
        assert_eq!(
            resolved.ai.models[0].api_key_env.as_deref(),
            Some("CUSTOM_ANTHROPIC_API_KEY")
        );
    }
}
