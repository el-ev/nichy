use serde::Deserialize;

#[derive(Deserialize)]
pub struct SiteConfig {
    #[serde(default)]
    pub site_name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub author_url: Option<String>,
    #[serde(default = "default_listen")]
    pub listen: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: f64,
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_service_workers")]
    pub service_workers: usize,
    #[serde(default = "default_max_jobs_per_worker")]
    pub max_jobs_per_worker: usize,
    #[serde(default = "default_cache_capacity")]
    pub cache_capacity: usize,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            site_name: None,
            author: None,
            author_url: None,
            listen: default_listen(),
            timeout_secs: default_timeout_secs(),
            db_path: default_db_path(),
            service_workers: default_service_workers(),
            max_jobs_per_worker: default_max_jobs_per_worker(),
            cache_capacity: default_cache_capacity(),
        }
    }
}

fn default_listen() -> Vec<String> {
    vec!["127.0.0.1:3873".into()]
}

fn default_timeout_secs() -> f64 {
    2.0
}

fn default_db_path() -> String {
    "nichy-web.db".into()
}

fn default_service_workers() -> usize {
    4
}

fn default_max_jobs_per_worker() -> usize {
    500
}

fn default_cache_capacity() -> usize {
    4096
}

pub fn load() -> SiteConfig {
    let path = std::env::var("NICHY_WEB_CONFIG").unwrap_or_else(|_| "nichy-web.toml".into());
    let mut cfg: SiteConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    if !cfg.timeout_secs.is_finite() || cfg.timeout_secs <= 0.0 {
        eprintln!(
            "warning: timeout_secs={} invalid, falling back to {}",
            cfg.timeout_secs,
            default_timeout_secs()
        );
        cfg.timeout_secs = default_timeout_secs();
    }
    cfg
}
