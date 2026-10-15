//! Configuration stored as TOML in the Retkerno data directory.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// File name of the configuration inside [`data_dir`].
const CONFIG_FILE: &str = "config.toml";

/// Returns `$RETKERNO_DATA_DIR`, else `dirs::data_dir()/retkerno`.
///
/// The environment is read on every call.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RETKERNO_DATA_DIR") {
        return PathBuf::from(dir);
    }

    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("retkerno")
}

/// How the client reaches the radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Bluetooth LE.
    #[default]
    Ble,
    /// USB serial.
    Serial,
    /// TCP socket.
    Tcp,
}

/// Radio connection settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    /// Which transport to use.
    pub transport: Transport,
    /// BLE device name or MAC address.
    pub ble_device: String,
    /// Serial port path.
    pub serial_port: String,
    /// Serial baud rate.
    pub baud: u32,
    /// TCP host.
    pub tcp_host: String,
    /// TCP port.
    pub tcp_port: u16,
    /// Connect at startup.
    pub auto_connect: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        ConnectionConfig {
            transport: Transport::Ble,
            ble_device: String::new(),
            serial_port: String::new(),
            baud: 115_200,
            tcp_host: String::new(),
            tcp_port: 5000,
            auto_connect: true,
        }
    }
}

/// GUI settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Window decorations: `"auto"`, `"on"`, or `"off"`.
    pub decorations: String,
    /// Width of the sidebar in pixels; the GUI clamps it to its limits.
    pub sidebar_width: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            decorations: "auto".into(),
            sidebar_width: 260,
        }
    }
}

/// Whole application configuration, stored as `config.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Radio connection settings.
    pub connection: ConnectionConfig,
    /// GUI settings.
    pub ui: UiConfig,
}

impl Config {
    /// Reads the configuration at `path`; a missing file gives the defaults.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be read and
    /// [`Error::Config`] when it is not valid TOML for this schema.
    pub fn load_at(path: &Path) -> Result<Config> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e.into()),
        };

        toml::from_str(&raw).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
    }

    /// Writes the configuration to `path`, creating parent directories.
    ///
    /// The data goes to a temporary sibling file that is then renamed over
    /// `path`, so a crash never leaves a half-written file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on filesystem failure and [`Error::Config`] if
    /// serialization fails.
    pub fn save_at(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut tmp_name = OsString::from(path.as_os_str());
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);

        fs::write(&tmp, raw)?;
        fs::rename(&tmp, path)?;

        Ok(())
    }

    /// Reads the configuration from [`data_dir`].
    ///
    /// # Errors
    ///
    /// See [`Config::load_at`].
    pub fn load() -> Result<Config> {
        Config::load_at(&data_dir().join(CONFIG_FILE))
    }

    /// Writes the configuration to [`data_dir`].
    ///
    /// # Errors
    ///
    /// See [`Config::save_at`].
    pub fn save(&self) -> Result<()> {
        self.save_at(&data_dir().join(CONFIG_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod sidebar_width {
        use super::*;

        #[test]
        fn should_default_to_260_when_an_old_file_lacks_it() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");

            fs::write(&path, "[ui]\ndecorations = \"on\"\n").unwrap();

            let config = Config::load_at(&path).unwrap();

            assert_eq!(config.ui.sidebar_width, 260);
            assert_eq!(config.ui.decorations, "on");
        }

        #[test]
        fn should_round_trip_through_a_saved_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            let mut config = Config::default();

            config.ui.sidebar_width = 333;
            config.save_at(&path).unwrap();

            assert_eq!(Config::load_at(&path).unwrap().ui.sidebar_width, 333);
        }
    }

    mod save_at {
        use super::*;

        #[test]
        fn should_leave_no_tmp_file_behind() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");

            Config::default().save_at(&path).unwrap();

            let names: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names, vec!["config.toml".to_string()]);
        }
    }
}
