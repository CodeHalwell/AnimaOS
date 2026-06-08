//! Config loading from file, environment, and defaults.

use crate::schema::{AnimaConfig, CURRENT_SCHEMA_VERSION};

/// Describes where a loaded `AnimaConfig` originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Loaded from a specific TOML file on disk.
    File(std::path::PathBuf),
    /// Constructed from built-in defaults (no file present).
    Defaults,
}

/// Errors that can occur while loading a configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read (I/O error).
    Io(std::io::Error),
    /// The TOML content was syntactically invalid.
    Parse(toml::de::Error),
    /// The config's `schema.version` is newer than this binary understands.
    SchemaTooNew {
        /// Version recorded in the config file.
        found: u32,
        /// Maximum version this binary supports.
        max: u32,
    },
    /// The config failed semantic validation.
    Invalid(crate::ValidationError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config I/O error: {e}"),
            Self::Parse(e) => write!(f, "config parse error: {e}"),
            Self::SchemaTooNew { found, max } => write!(
                f,
                "config schema version {found} is newer than this binary supports (max {max})"
            ),
            Self::Invalid(e) => write!(f, "config validation error: {e}"),
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        Self::Parse(e)
    }
}

impl From<crate::ValidationError> for ConfigError {
    fn from(e: crate::ValidationError) -> Self {
        Self::Invalid(e)
    }
}

/// Load an `AnimaConfig` from a TOML file, validate it, and return.
pub fn load_from_file(path: impl AsRef<std::path::Path>) -> Result<AnimaConfig, ConfigError> {
    let raw = std::fs::read_to_string(path.as_ref())?;
    let cfg: AnimaConfig = toml::from_str(&raw)?;
    if cfg.schema.version > CURRENT_SCHEMA_VERSION {
        return Err(ConfigError::SchemaTooNew {
            found: cfg.schema.version,
            max: CURRENT_SCHEMA_VERSION,
        });
    }
    cfg.validate()?;
    Ok(cfg)
}

/// Load from a file if it exists, otherwise return defaults.
pub fn load_or_defaults(path: impl AsRef<std::path::Path>) -> (AnimaConfig, ConfigSource) {
    let p = path.as_ref();
    if p.exists() {
        match load_from_file(p) {
            Ok(cfg) => (cfg, ConfigSource::File(p.to_path_buf())),
            Err(_) => (AnimaConfig::from_defaults(), ConfigSource::Defaults),
        }
    } else {
        (AnimaConfig::from_defaults(), ConfigSource::Defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_from_file_succeeds_on_valid_toml() {
        let cfg = AnimaConfig::from_defaults();
        let toml_str = cfg.to_toml_string().unwrap();

        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(toml_str.as_bytes()).unwrap();

        let loaded = load_from_file(f.path()).unwrap();
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn load_from_file_returns_io_error_on_missing_file() {
        let result = load_from_file("/nonexistent/path/anima.toml");
        assert!(matches!(result, Err(ConfigError::Io(_))));
    }

    #[test]
    fn load_from_file_returns_parse_error_on_invalid_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"this is not valid [[[ toml").unwrap();
        let result = load_from_file(f.path());
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn load_from_file_rejects_future_schema_version() {
        let toml_str = format!(
            "[schema]\nversion = {}\n[agent]\nid = \"anima\"\nstate_dir = \"~/.anima/anima\"\n",
            CURRENT_SCHEMA_VERSION + 1
        );
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(toml_str.as_bytes()).unwrap();
        let result = load_from_file(f.path());
        assert!(matches!(result, Err(ConfigError::SchemaTooNew { .. })));
    }

    #[test]
    fn load_or_defaults_returns_defaults_when_no_file() {
        let (cfg, src) = load_or_defaults("/nonexistent/path/anima.toml");
        assert_eq!(src, ConfigSource::Defaults);
        assert_eq!(cfg, AnimaConfig::from_defaults());
    }

    #[test]
    fn load_or_defaults_loads_file_when_present() {
        let cfg = AnimaConfig::from_defaults();
        let toml_str = cfg.to_toml_string().unwrap();

        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(toml_str.as_bytes()).unwrap();

        let (loaded, src) = load_or_defaults(f.path());
        assert!(matches!(src, ConfigSource::File(_)));
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn config_error_display_is_non_empty() {
        let err = ConfigError::SchemaTooNew { found: 99, max: 1 };
        assert!(!format!("{err}").is_empty());
    }
}
