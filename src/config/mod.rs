use std::{
    env,
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Read},
    net::AddrParseError,
    sync::Arc,
};
use thiserror::Error;

use serde::{de, Deserialize, Deserializer, Serialize};
use tokio::{
    sync::Mutex,
    time::{self, sleep},
};
use tracing::level_filters::LevelFilter;
use url::Url;

const DEFAULT_PORT: u16 = 4000;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AegisConfig {
    #[serde(default = "default_server")]
    pub server: AegisServer,
    pub upstream: String,
    #[serde(default = "default_redis_config")]
    pub redis: RedisConfig,
    #[serde(
        default = "default_action",
        deserialize_with = "deserialize_limited_action"
    )]
    pub default_action: RuleAction,
    pub rules: Vec<AegisRule>,
    #[serde(skip)]
    pub config_hash: u64,
}

fn default_action() -> RuleAction {
    RuleAction::Allow
}

// Custom deserializer to restrict default action value
fn deserialize_limited_action<'de, D>(deserializer: D) -> Result<RuleAction, D::Error>
where
    D: Deserializer<'de>,
{
    let action = RuleAction::deserialize(deserializer)?;

    match action {
        RuleAction::Allow | RuleAction::Block => Ok(action),
        _ => Err(de::Error::custom(
            "Only Allow and Block variants are supported for this field",
        )),
    }
}

// Default values for server
fn default_server() -> AegisServer {
    let port = match env::var("PORT") {
        Ok(port) => port.parse::<u16>().unwrap_or(DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    };

    AegisServer {
        address: "0.0.0.0".to_string(),
        port,
        log_level: AegisServerLogLevel::INFO,
    }
}

// TODO: Set default redis config separately
fn default_redis_config() -> RedisConfig {
    RedisConfig {
        enabled: true,
        url: "redis://127.0.0.1/".to_string(),
    }
}

// Server config
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AegisServer {
    pub address: String,
    pub port: u16,
    pub log_level: AegisServerLogLevel,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum AegisServerLogLevel {
    INFO,
    ERROR,
    WARN,
    DEBUG,
    TRACE,
}

impl AegisServerLogLevel {
    pub fn into_level_filter(&self) -> LevelFilter {
        match self {
            AegisServerLogLevel::INFO => LevelFilter::INFO,
            AegisServerLogLevel::ERROR => LevelFilter::ERROR,
            AegisServerLogLevel::WARN => LevelFilter::WARN,
            AegisServerLogLevel::DEBUG => LevelFilter::DEBUG,
            AegisServerLogLevel::TRACE => LevelFilter::TRACE,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RedisConfig {
    pub enabled: bool,
    pub url: String,
}

// Regular rule statement config
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegularRuleStatement {
    pub inspect: RegularRuleStatementInspect,
    pub negate_statement: bool,
    pub match_type: RegularRuleStatementMatchType,
    pub match_string: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RegularRuleStatementMatchType {
    StartsWith,
    EndsWith,
    Contains,
    Exact,
    Regex,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum RegularRuleStatementInspectTypeScope {
    All,
    Keys,
    Values,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum RegularRuleStatementInspectTypeContentFilter {
    All,
    Include { key: String },
    Exclude { key: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RegularRuleStatementInspect {
    Header {
        key: String,
    },
    QueryParameter {
        key: String,
    },
    HttpMethod,
    UriPath,
    QueryString,
    AllHeaders {
        scope: RegularRuleStatementInspectTypeScope,
        content_filter: RegularRuleStatementInspectTypeContentFilter,
    },
    Cookies {
        scope: RegularRuleStatementInspectTypeScope,
        content_filter: RegularRuleStatementInspectTypeContentFilter,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RegularRuleCondition {
    One,
    All,
    None,
}

// ===

// Rate based rule config
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RateBasedRuleKey {
    SourceIp,
}
// ===

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RuleAction {
    Allow,
    Block,
    Count,
}

impl RuleAction {
    pub fn negate(&self) -> Self {
        match self {
            RuleAction::Allow => RuleAction::Block,
            RuleAction::Block => RuleAction::Allow,
            RuleAction::Count => RuleAction::Count,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum AegisRule {
    Regular {
        action: RuleAction,
        condition: RegularRuleCondition,
        statements: Vec<RegularRuleStatement>,
    },
    RateBased {
        limit: i64,
        evaluation_window_seconds: i64,
        key: RateBasedRuleKey,
    },
}

// Errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to read config file")]
    ConfigFileAccessError(#[from] io::Error),
    #[error("invalid config file")]
    ConfigFileDeserialiationError(#[from] serde_yaml::Error),
    #[error("invalid upstream address")]
    UpstreamAddressInvalidError(#[from] url::ParseError),
    #[error("invalid listen address")]
    ListenAddrInvalidError(#[from] AddrParseError),
}

impl AegisConfig {
    pub fn from_file(path: &String) -> Result<AegisConfig, ConfigError> {
        let mut config_hasher = DefaultHasher::new();
        let mut config_file = File::open(path)?;
        let mut config_buf = String::new();
        config_file.read_to_string(&mut config_buf)?;
        let mut aegis_config: AegisConfig = serde_yaml::from_str(&config_buf)?;
        config_buf.hash(&mut config_hasher);
        aegis_config.config_hash = config_hasher.finish();
        Ok(aegis_config)
    }

    pub fn validate(self: &Self) -> Result<bool, ConfigError> {
        let _ = Url::parse(self.upstream.as_str())?;
        Ok(true)
    }
}

pub async fn watch_config(path: String, config: Arc<Mutex<AegisConfig>>) {
    tracing::info!("⏱ Watching config file for changes");
    loop {
        sleep(time::Duration::from_secs(5)).await;
        if let Ok(new_config) = AegisConfig::from_file(&path) {
            let mut current_config: tokio::sync::MutexGuard<'_, AegisConfig> = config.lock().await;
            if new_config.config_hash != current_config.config_hash {
                match new_config.validate() {
                    Ok(_) => {
                        *current_config = new_config;
                        tracing::debug!("Config file updated successfully");
                    }
                    Err(e) => {
                        tracing::error!("Config file invalid: {e}");
                    }
                };
            };
        } else {
            tracing::error!("Failed to fetch new config");
        }
    }
}