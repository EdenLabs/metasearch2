use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::Path,
    sync::{Arc, LazyLock},
};

use serde::Deserialize;
use tracing::info;

use crate::engines::Engine;

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: "0.0.0.0:28019".parse().unwrap(),
            api: false,
            ui: UiConfig {
                show_engine_list_separator: false,
                show_version_info: false,
                site_name: "metasearch".to_string(),
                show_settings_link: true,
                stylesheet_url: "".to_string(),
                stylesheet_str: "".to_string(),
                favicon_url: "".to_string(),
                show_autocomplete: true,
            },
            image_search: ImageSearchConfig {
                enabled: false,
                show_engines: true,
                proxy: ImageProxyConfig {
                    enabled: true,
                    max_download_size: 10_000_000,
                },
            },
            engines: Arc::new(EnginesConfig::default()),
            urls: UrlsConfig {
                replace: vec![(
                    HostAndPath::new("minecraft.fandom.com/wiki/"),
                    HostAndPath::new("minecraft.wiki/w/"),
                )],
                weight: vec![],
            },
            rerank: RerankConfig::default(),
        }
    }
}

impl Default for EnginesConfig {
    fn default() -> Self {
        use toml::value::Value;

        let mut map = HashMap::new();
        // engines are enabled by default, so engines that aren't listed here are
        // enabled

        // main search engines
        map.insert(Engine::Google, EngineConfig::new().with_weight(1.05));
        map.insert(Engine::Bing, EngineConfig::new().with_weight(1.0));
        map.insert(Engine::Brave, EngineConfig::new().with_weight(1.25));
        map.insert(
            Engine::Marginalia,
            EngineConfig::new().with_weight(0.15).with_extra(
                vec![(
                    "args".to_string(),
                    Value::Table(
                        vec![
                            ("profile".to_string(), Value::String("corpo".to_string())),
                            ("js".to_string(), Value::String("default".to_string())),
                            ("adtech".to_string(), Value::String("default".to_string())),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                )]
                .into_iter()
                .collect(),
            ),
        );

        // additional search engines
        map.insert(
            Engine::GoogleScholar,
            EngineConfig::new().with_weight(0.50).disabled(),
        );
        map.insert(
            Engine::RightDao,
            EngineConfig::new().with_weight(0.10).disabled(),
        );
        map.insert(
            Engine::Stract,
            EngineConfig::new().with_weight(0.15).disabled(),
        );
        map.insert(
            Engine::Yep,
            EngineConfig::new().with_weight(0.10).disabled(),
        );

        // calculators (give them a high weight so they're always the first thing in
        // autocomplete)
        map.insert(Engine::Numbat, EngineConfig::new().with_weight(10.0));
        map.insert(
            Engine::Fend,
            EngineConfig::new().with_weight(10.0).disabled(),
        );

        // other engines
        map.insert(
            Engine::Mdn,
            EngineConfig::new().with_extra(
                vec![("max_sections".to_string(), Value::Integer(1))]
                    .into_iter()
                    .collect(),
            ),
        );

        Self { map }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weight: 1.0,
            extra: Default::default(),
        }
    }
}
static DEFAULT_ENGINE_CONFIG_REF: LazyLock<EngineConfig> = LazyLock::new(EngineConfig::default);
impl EngineConfig {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_weight(self, weight: f64) -> Self {
        Self { weight, ..self }
    }
    pub fn disabled(self) -> Self {
        Self {
            enabled: false,
            ..self
        }
    }
    pub fn with_extra(self, extra: toml::Table) -> Self {
        Self { extra, ..self }
    }
}

//

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// Whether the JSON API should be accessible.
    pub api: bool,
    pub ui: UiConfig,
    pub image_search: ImageSearchConfig,
    // wrapped in an arc to make Config cheaper to clone
    pub engines: Arc<EnginesConfig>,
    pub urls: UrlsConfig,
    pub rerank: RerankConfig,
}

#[derive(Deserialize, Debug)]
pub struct PartialConfig {
    pub bind: Option<SocketAddr>,
    pub api: Option<bool>,
    pub ui: Option<PartialUiConfig>,
    pub image_search: Option<PartialImageSearchConfig>,
    pub engines: Option<PartialEnginesConfig>,
    pub urls: Option<PartialUrlsConfig>,
    pub rerank: Option<PartialRerankConfig>,
}

impl Config {
    pub fn overlay(&mut self, partial: PartialConfig) {
        self.bind = partial.bind.unwrap_or(self.bind);
        self.api = partial.api.unwrap_or(self.api);
        self.ui.overlay(partial.ui.unwrap_or_default());
        self.image_search
            .overlay(partial.image_search.unwrap_or_default());
        if let Some(partial_engines) = partial.engines {
            let mut engines = self.engines.as_ref().clone();
            engines.overlay(partial_engines);
            self.engines = Arc::new(engines);
        }
        self.urls.overlay(partial.urls.unwrap_or_default());
        self.rerank.overlay(partial.rerank.unwrap_or_default());
    }
}

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub show_engine_list_separator: bool,
    pub show_version_info: bool,
    /// Settings are always accessible anyways, this just controls whether the
    /// link to them in the index page is visible.
    pub show_settings_link: bool,
    pub site_name: String,
    pub show_autocomplete: bool,
    pub stylesheet_url: String,
    pub stylesheet_str: String,
    pub favicon_url: String,
}

#[derive(Deserialize, Debug, Default)]
pub struct PartialUiConfig {
    pub show_engine_list_separator: Option<bool>,
    pub show_version_info: Option<bool>,
    pub show_settings_link: Option<bool>,
    pub show_autocomplete: Option<bool>,

    pub site_name: Option<String>,
    pub stylesheet_url: Option<String>,
    pub stylesheet_str: Option<String>,
    pub favicon_url: Option<String>,
}

impl UiConfig {
    pub fn overlay(&mut self, partial: PartialUiConfig) {
        self.show_engine_list_separator = partial
            .show_engine_list_separator
            .unwrap_or(self.show_engine_list_separator);
        self.show_version_info = partial.show_version_info.unwrap_or(self.show_version_info);
        self.show_settings_link = partial
            .show_settings_link
            .unwrap_or(self.show_settings_link);
        self.show_autocomplete = partial.show_autocomplete.unwrap_or(self.show_autocomplete);
        self.site_name = partial.site_name.unwrap_or(self.site_name.clone());
        self.stylesheet_url = partial
            .stylesheet_url
            .unwrap_or(self.stylesheet_url.clone());
        self.stylesheet_str = partial
            .stylesheet_str
            .unwrap_or(self.stylesheet_str.clone());
        self.favicon_url = partial.favicon_url.unwrap_or(self.favicon_url.clone());
    }
}

#[derive(Debug, Clone)]
pub struct ImageSearchConfig {
    pub enabled: bool,
    pub show_engines: bool,
    pub proxy: ImageProxyConfig,
}

#[derive(Deserialize, Debug, Default)]
pub struct PartialImageSearchConfig {
    pub enabled: Option<bool>,
    pub show_engines: Option<bool>,
    pub proxy: Option<PartialImageProxyConfig>,
}

impl ImageSearchConfig {
    pub fn overlay(&mut self, partial: PartialImageSearchConfig) {
        self.enabled = partial.enabled.unwrap_or(self.enabled);
        self.show_engines = partial.show_engines.unwrap_or(self.show_engines);
        self.proxy.overlay(partial.proxy.unwrap_or_default());
    }
}

#[derive(Debug, Clone)]
pub struct ImageProxyConfig {
    /// Whether we should proxy remote images through our server. This is mostly
    /// a privacy feature.
    pub enabled: bool,
    /// The maximum size of an image that can be proxied. This is in bytes.
    pub max_download_size: u64,
}

#[derive(Deserialize, Debug, Default)]
pub struct PartialImageProxyConfig {
    pub enabled: Option<bool>,
    pub max_download_size: Option<u64>,
}

impl ImageProxyConfig {
    pub fn overlay(&mut self, partial: PartialImageProxyConfig) {
        self.enabled = partial.enabled.unwrap_or(self.enabled);
        self.max_download_size = partial.max_download_size.unwrap_or(self.max_download_size);
    }
}

#[derive(Debug, Clone)]
pub struct EnginesConfig {
    pub map: HashMap<Engine, EngineConfig>,
}

#[derive(Deserialize, Debug, Default)]
pub struct PartialEnginesConfig {
    #[serde(flatten)]
    pub map: HashMap<Engine, PartialDefaultableEngineConfig>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum PartialDefaultableEngineConfig {
    Boolean(bool),
    Full(PartialEngineConfig),
}

impl EnginesConfig {
    pub fn overlay(&mut self, partial: PartialEnginesConfig) {
        for (key, value) in partial.map {
            let full = match value {
                PartialDefaultableEngineConfig::Boolean(enabled) => PartialEngineConfig {
                    enabled: Some(enabled),
                    ..Default::default()
                },
                PartialDefaultableEngineConfig::Full(full) => full,
            };
            if let Some(existing) = self.map.get_mut(&key) {
                existing.overlay(full);
            } else {
                let mut new = EngineConfig::default();
                new.overlay(full);
                self.map.insert(key, new);
            }
        }
    }

    pub fn get(&self, engine: Engine) -> &EngineConfig {
        self.map.get(&engine).unwrap_or(&DEFAULT_ENGINE_CONFIG_REF)
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub enabled: bool,
    /// The priority of this engine relative to the other engines.
    pub weight: f64,
    /// Per-engine configs. These are parsed at request time.
    pub extra: toml::Table,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct PartialEngineConfig {
    pub enabled: Option<bool>,
    pub weight: Option<f64>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl EngineConfig {
    pub fn overlay(&mut self, partial: PartialEngineConfig) {
        self.enabled = partial.enabled.unwrap_or(self.enabled);
        self.weight = partial.weight.unwrap_or(self.weight);
        self.extra.extend(partial.extra);
    }
}

/// Weights for Layer 2 vocabulary sophistication features.
/// Each weight controls how much its corresponding feature
/// contributes to the audience sophistication score.
#[derive(Debug, Clone)]
pub struct L2FeatureWeights {
    /// Ratio of capitalized words in the text.
    pub capitalized_word_ratio   : f64,
    /// Ratio of unique words to total words.
    pub unique_word_ratio        : f64,
    /// Density of domain-specific technical terms.
    pub technical_term_density   : f64,
    /// Average number of characters per word.
    pub avg_word_length          : f64,
    /// Characters per word, measured differently from avg_word_length.
    pub char_per_word            : f64,
}

impl Default for L2FeatureWeights {
    fn default() -> Self {
        Self {
            capitalized_word_ratio   : 0.20,
            unique_word_ratio        : 0.20,
            technical_term_density   : 0.20,
            avg_word_length          : 0.20,
            char_per_word            : 0.20,
        }
    }
}

/// Partial overlay for `L2FeatureWeights`.
#[derive(Deserialize, Debug, Default)]
pub struct PartialL2FeatureWeights {
    pub capitalized_word_ratio   : Option<f64>,
    pub unique_word_ratio        : Option<f64>,
    pub technical_term_density   : Option<f64>,
    pub avg_word_length          : Option<f64>,
    pub char_per_word            : Option<f64>,
}

impl L2FeatureWeights {
    pub fn overlay(&mut self, partial: PartialL2FeatureWeights) {
        self.capitalized_word_ratio = partial.capitalized_word_ratio.unwrap_or(self.capitalized_word_ratio);
        self.unique_word_ratio      = partial.unique_word_ratio.unwrap_or(self.unique_word_ratio);
        self.technical_term_density = partial.technical_term_density.unwrap_or(self.technical_term_density);
        self.avg_word_length        = partial.avg_word_length.unwrap_or(self.avg_word_length);
        self.char_per_word          = partial.char_per_word.unwrap_or(self.char_per_word);
    }
}

/// Weights for URL structure signals used in Layer 1 scoring.
/// Each weight controls how much its corresponding URL signal
/// contributes to the source identity score.
#[derive(Debug, Clone)]
pub struct UrlSignalWeights {
    /// Penalty based on URL path depth.
    pub url_depth              : f64,
    /// Weight for word count in the URL slug.
    pub url_slug_word_count    : f64,
    /// Boost for paths that look like documentation.
    pub is_docs_path           : f64,
    /// Boost for paths that look like forums or Q&A.
    pub is_forum_path          : f64,
    /// Boost for institutional TLDs (.edu, .gov, .org).
    pub is_institutional_tld   : f64,
    /// Penalty for domains with many tokens (long domain names).
    pub domain_token_count     : f64,
    /// Weight for presence of a subdomain.
    pub has_subdomain          : f64,
    /// Penalty for commercial URL patterns.
    pub is_commercial_pattern  : f64,
}

impl Default for UrlSignalWeights {
    fn default() -> Self {
        Self {
            url_depth              :  0.05,
            url_slug_word_count    :  0.10,
            is_docs_path           :  0.25,
            is_forum_path          :  0.15,
            is_institutional_tld   :  0.15,
            domain_token_count     : -0.10,
            has_subdomain          :  0.10,
            is_commercial_pattern  : -0.20,
        }
    }
}

/// Partial overlay for `UrlSignalWeights`.
#[derive(Deserialize, Debug, Default)]
pub struct PartialUrlSignalWeights {
    pub url_depth              : Option<f64>,
    pub url_slug_word_count    : Option<f64>,
    pub is_docs_path           : Option<f64>,
    pub is_forum_path          : Option<f64>,
    pub is_institutional_tld   : Option<f64>,
    pub domain_token_count     : Option<f64>,
    pub has_subdomain          : Option<f64>,
    pub is_commercial_pattern  : Option<f64>,
}

impl UrlSignalWeights {
    pub fn overlay(&mut self, partial: PartialUrlSignalWeights) {
        self.url_depth             = partial.url_depth.unwrap_or(self.url_depth);
        self.url_slug_word_count   = partial.url_slug_word_count.unwrap_or(self.url_slug_word_count);
        self.is_docs_path          = partial.is_docs_path.unwrap_or(self.is_docs_path);
        self.is_forum_path         = partial.is_forum_path.unwrap_or(self.is_forum_path);
        self.is_institutional_tld  = partial.is_institutional_tld.unwrap_or(self.is_institutional_tld);
        self.domain_token_count    = partial.domain_token_count.unwrap_or(self.domain_token_count);
        self.has_subdomain         = partial.has_subdomain.unwrap_or(self.has_subdomain);
        self.is_commercial_pattern = partial.is_commercial_pattern.unwrap_or(self.is_commercial_pattern);
    }
}

/// Configuration for the post-merge result re-ranking system.
/// Controls domain reputation scoring (Layer 1) and vocabulary
/// sophistication scoring (Layer 2).
#[derive(Debug, Clone)]
pub struct RerankConfig {
    /// Whether re-ranking is enabled.
    pub enabled            : bool,
    /// Weight for the upstream search engine score.
    pub alpha              : f64,
    /// Weight for Layer 1 (source identity) score.
    pub beta               : f64,
    /// Weight for Layer 2 (audience sophistication) score.
    pub gamma              : f64,
    /// Penalty strength for query relevance. 0.0 disables the
    /// penalty, 1.0 makes scoring fully proportional to relevance.
    pub delta              : f64,
    /// Path to the domain blocklist file.
    pub blocklist_path     : String,
    /// Path to the domain reputation TOML file.
    pub reputation_path    : String,
    /// Weights for Layer 2 vocabulary features.
    pub l2_weights         : L2FeatureWeights,
    /// Weights for URL structure signals.
    pub url_signal_weights : UrlSignalWeights,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled            : true,
            alpha              : 0.50,
            beta               : 0.30,
            gamma              : 0.20,
            delta              : 0.90,
            blocklist_path     : "res/blocked_domains_slim.txt".to_string(),
            reputation_path    : "res/domain_reputation.toml".to_string(),
            l2_weights         : L2FeatureWeights::default(),
            url_signal_weights : UrlSignalWeights::default(),
        }
    }
}

/// Partial overlay for `RerankConfig`.
#[derive(Deserialize, Debug, Default)]
pub struct PartialRerankConfig {
    pub enabled            : Option<bool>,
    pub alpha              : Option<f64>,
    pub beta               : Option<f64>,
    pub gamma              : Option<f64>,
    pub delta              : Option<f64>,
    pub blocklist_path     : Option<String>,
    pub reputation_path    : Option<String>,
    pub l2_weights         : Option<PartialL2FeatureWeights>,
    pub url_signal_weights : Option<PartialUrlSignalWeights>,
}

impl RerankConfig {
    pub fn overlay(&mut self, partial: PartialRerankConfig) {
        self.enabled         = partial.enabled.unwrap_or(self.enabled);
        self.alpha           = partial.alpha.unwrap_or(self.alpha);
        self.beta            = partial.beta.unwrap_or(self.beta);
        self.gamma           = partial.gamma.unwrap_or(self.gamma);
        self.delta           = partial.delta.unwrap_or(self.delta);
        self.blocklist_path  = partial.blocklist_path.unwrap_or(self.blocklist_path.clone());
        self.reputation_path = partial.reputation_path.unwrap_or(self.reputation_path.clone());
        self.l2_weights.overlay(partial.l2_weights.unwrap_or_default());
        self.url_signal_weights.overlay(partial.url_signal_weights.unwrap_or_default());
    }
}

impl Config {
    pub fn read_or_create(config_path: &Path) -> eyre::Result<Self> {
        let mut config = Config::default();

        if !config_path.exists() {
            info!("No config found, creating one at {config_path:?}");
            let default_config_str = include_str!("../config-default.toml");
            if let Some(parent_path) = config_path.parent() {
                let _ = fs::create_dir_all(parent_path);
            }
            fs::write(config_path, default_config_str)?;
        }

        let given_config = toml::from_str::<PartialConfig>(&fs::read_to_string(config_path)?)?;
        config.overlay(given_config);
        Ok(config)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostAndPath {
    pub host: String,
    pub path: String,
}
impl HostAndPath {
    pub fn new(s: &str) -> Self {
        let (host, path) = s.split_once('/').unwrap_or((s, ""));
        Self {
            host: host.to_owned(),
            path: path.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UrlsConfig {
    pub replace: Vec<(HostAndPath, HostAndPath)>,
    pub weight: Vec<(HostAndPath, f64)>,
}
#[derive(Deserialize, Debug, Default)]
pub struct PartialUrlsConfig {
    #[serde(default)]
    pub replace: HashMap<String, String>,
    #[serde(default)]
    pub weight: HashMap<String, f64>,
}
impl UrlsConfig {
    pub fn overlay(&mut self, partial: PartialUrlsConfig) {
        for (from, to) in partial.replace {
            let from = HostAndPath::new(&from);
            if to.is_empty() {
                // setting the value to an empty string removes it
                let index = self.replace.iter().position(|(u, _)| u == &from);
                // swap_remove is fine because the order of this vec doesn't matter
                self.replace.swap_remove(index.unwrap());
            } else {
                let to = HostAndPath::new(&to);
                self.replace.push((from, to));
            }
        }

        for (url, weight) in partial.weight {
            let url = HostAndPath::new(&url);
            self.weight.push((url, weight));
        }

        // sort by length so that more specific checks are done first
        self.weight.sort_by(|(a, _), (b, _)| {
            let a_len = a.path.len() + a.host.len();
            let b_len = b.path.len() + b.host.len();
            b_len.cmp(&a_len)
        });
    }
}
