//! Optional config file loading. Search order: ./rdrscrape.toml, then
//! $XDG_CONFIG_HOME/rdrscrape/config.toml (or ~/.config/rdrscrape/config.toml).

use serde::Deserialize;
use std::path::PathBuf;

/// Config file contents. All fields optional; only present keys override defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct Config {
    /// Default output directory when -o is not set. Paths are relative to CWD.
    pub output_dir: Option<PathBuf>,
    /// HTTP User-Agent header.
    pub user_agent: Option<String>,
    /// Delay in seconds between requests.
    pub request_delay_secs: Option<u64>,
    /// Request timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Include a visible table-of-contents page after the cover in EPUB (default: true). Set to false to disable.
    pub toc_page: Option<bool>,
    /// Number of HTTP attempts for transient failures (default 3). Only used when retry_backoff_secs is not set or is non-empty.
    pub retry_count: Option<u32>,
    /// Delay in seconds before each retry (e.g. [1, 2, 4]). Length should be retry_count - 1. If not set, default [1, 2, 4] is used.
    pub retry_backoff_secs: Option<Vec<u64>>,
    /// How to handle chapters with empty body or missing content: skip (default), placeholder, or fail.
    pub empty_chapters: Option<String>,
}

/// Search order: (1) ./rdrscrape.toml, (2) $XDG_CONFIG_HOME/rdrscrape/config.toml.
/// Missing file returns Ok(None). Invalid TOML or I/O error reading a present file returns Err.
pub fn load_config() -> Result<Option<Config>, String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("Cannot determine current directory: {}", e))?;
    let mut paths = vec![cwd.join("rdrscrape.toml")];
    if let Some(d) = dirs::config_dir() {
        paths.push(d.join("rdrscrape").join("config.toml"));
    }
    for path in &paths {
        if path.exists() {
            let s = std::fs::read_to_string(path)
                .map_err(|e| format!("Cannot read config {}: {}", path.display(), e))?;
            let config: Config = toml::from_str(&s)
                .map_err(|e| format!("Invalid config {}: {}", path.display(), e))?;
            return Ok(Some(config));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_config() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.output_dir.is_none());
        assert!(c.user_agent.is_none());
        assert!(c.request_delay_secs.is_none());
        assert!(c.timeout_secs.is_none());
        assert!(c.toc_page.is_none());
        assert!(c.retry_count.is_none());
        assert!(c.retry_backoff_secs.is_none());
        assert!(c.empty_chapters.is_none());
    }

    #[test]
    fn parse_full_config() {
        let s = r#"
            output_dir = "out"
            user_agent = "Custom/1.0"
            request_delay_secs = 3
            timeout_secs = 60
            toc_page = true
            retry_count = 5
            retry_backoff_secs = [1, 2, 4, 8]
            empty_chapters = "placeholder"
        "#;
        let c: Config = toml::from_str(s).unwrap();
        assert_eq!(c.output_dir.as_deref(), Some(std::path::Path::new("out")));
        assert_eq!(c.user_agent.as_deref(), Some("Custom/1.0"));
        assert_eq!(c.request_delay_secs, Some(3));
        assert_eq!(c.timeout_secs, Some(60));
        assert_eq!(c.toc_page, Some(true));
        assert_eq!(c.retry_count, Some(5));
        assert_eq!(
            c.retry_backoff_secs.as_deref(),
            Some([1, 2, 4, 8].as_slice())
        );
        assert_eq!(c.empty_chapters.as_deref(), Some("placeholder"));
    }

    #[test]
    fn parse_partial_config() {
        let s = r#"
            request_delay_secs = 1
        "#;
        let c: Config = toml::from_str(s).unwrap();
        assert!(c.output_dir.is_none());
        assert!(c.user_agent.is_none());
        assert_eq!(c.request_delay_secs, Some(1));
        assert!(c.timeout_secs.is_none());
        assert!(c.toc_page.is_none());
    }

    #[test]
    fn parse_toc_page_false() {
        let s = "toc_page = false";
        let c: Config = toml::from_str(s).unwrap();
        assert_eq!(c.toc_page, Some(false));
    }

    #[test]
    fn invalid_toml_errors() {
        assert!(toml::from_str::<Config>("output_dir = [").is_err());
    }
}
