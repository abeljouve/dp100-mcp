//! TOML configuration shared by `dp100-mcp` and `dp100ctl`.
//!
//! ```toml
//! [device]
//! serial = "3F0F22964000"     # optional: default unit
//!
//! [limits]
//! max_voltage_v = 12.0
//! max_current_a = 1.0
//! vin_dropout_mv = 500
//!
//! [server]
//! on_exit = "off"             # or "keep"
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::limits::LimitsConfig;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub device: DeviceSection,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub server: ServerSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSection {
    /// USB serial or hidraw path of the default unit.
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// "keep" or "off".
    pub on_exit: Option<String>,
    /// Address for Streamable HTTP, e.g. "127.0.0.1:8765".
    pub http: Option<String>,
    /// Extra Host header values accepted in HTTP mode.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let c: FileConfig = toml::from_str(
            "[device]\nserial = \"ABC\"\n[limits]\nmax_voltage_v = 12.0\nmax_current_a = 1.0\n[server]\non_exit = \"off\"\n",
        )
        .unwrap();
        assert_eq!(c.device.serial.as_deref(), Some("ABC"));
        assert_eq!(c.limits.max_voltage_v, Some(12.0));
        assert_eq!(c.server.on_exit.as_deref(), Some("off"));
        assert!(toml::from_str::<FileConfig>("[limits]\nmax_volt = 1\n").is_err());
        assert!(toml::from_str::<FileConfig>("").is_ok());
    }
}
