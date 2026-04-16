use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct AppConfig {
    pub plugins: HashMap<String, PluginEntry>,
}

#[derive(Debug)]
pub struct PluginEntry {
    pub enabled: bool,
    pub settings: toml::Table,
}

impl AppConfig {
    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::parse(&contents),
            Err(_) => {
                log::debug!("No config file at {:?}, using defaults", path);
                Self::default()
            }
        }
    }

    pub fn plugin_enabled(&self, name: &str) -> Option<&toml::Table> {
        self.plugins
            .get(name)
            .filter(|e| e.enabled)
            .map(|e| &e.settings)
    }

    fn parse(contents: &str) -> Self {
        let table: toml::Table = match contents.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to parse config: {}", e);
                return Self::default();
            }
        };

        let mut plugins = HashMap::new();

        if let Some(toml::Value::Table(plugins_table)) = table.get("plugins") {
            for (name, value) in plugins_table {
                if let toml::Value::Table(plugin_table) = value {
                    let enabled = plugin_table
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let mut settings = plugin_table.clone();
                    settings.remove("enabled");

                    plugins.insert(
                        name.clone(),
                        PluginEntry { enabled, settings },
                    );
                }
            }
        }

        AppConfig { plugins }
    }

    fn config_path() -> PathBuf {
        directories::ProjectDirs::from("", "", "shadowcast-player")
            .map(|d| d.config_dir().join("shadowcast.toml"))
            .unwrap_or_else(|| PathBuf::from("shadowcast.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_config() {
        let config = AppConfig::parse("");
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn parse_plugin_enabled() {
        let toml = r#"
[plugins.mcp-server]
enabled = true
port = 8080
host = "127.0.0.1"
"#;
        let config = AppConfig::parse(toml);
        let settings = config.plugin_enabled("mcp-server");
        assert!(settings.is_some());
        let settings = settings.unwrap();
        assert_eq!(settings.get("port").unwrap().as_integer(), Some(8080));
        assert_eq!(
            settings.get("host").unwrap().as_str(),
            Some("127.0.0.1")
        );
        assert!(settings.get("enabled").is_none());
    }

    #[test]
    fn parse_plugin_disabled() {
        let toml = r#"
[plugins.hid-emulator]
enabled = false
device = "/dev/ttyACM0"
"#;
        let config = AppConfig::parse(toml);
        assert!(config.plugin_enabled("hid-emulator").is_none());
        assert!(config.plugins.contains_key("hid-emulator"));
    }

    #[test]
    fn parse_plugin_missing_enabled_defaults_false() {
        let toml = r#"
[plugins.some-plugin]
foo = "bar"
"#;
        let config = AppConfig::parse(toml);
        assert!(config.plugin_enabled("some-plugin").is_none());
    }

    #[test]
    fn parse_multiple_plugins() {
        let toml = r#"
[plugins.alpha]
enabled = true
[plugins.beta]
enabled = false
[plugins.gamma]
enabled = true
"#;
        let config = AppConfig::parse(toml);
        assert!(config.plugin_enabled("alpha").is_some());
        assert!(config.plugin_enabled("beta").is_none());
        assert!(config.plugin_enabled("gamma").is_some());
    }

    #[test]
    fn parse_invalid_toml_returns_default() {
        let config = AppConfig::parse("this is not valid toml {{{{");
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn plugin_not_in_config_returns_none() {
        let config = AppConfig::parse("");
        assert!(config.plugin_enabled("nonexistent").is_none());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let config = AppConfig::load();
        assert!(config.plugins.is_empty());
    }
}
