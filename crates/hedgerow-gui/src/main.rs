//! `hedgerow`: Dioxus desktop client over `hedgerow_core`.

mod app;
mod components;
mod format;
mod notifier;
mod theme;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use dioxus::desktop::{Config as WindowConfig, LogicalSize, WindowBuilder};
use hedgerow_core::config::{Config, data_dir};
use hedgerow_core::fixtures;
use hedgerow_core::store::Store;

use crate::app::{App, Boot, Hooks};

/// Installs the log subscriber before Dioxus does. `RUST_LOG` overrides the default, which is
/// `info` with the Dioxus edit-socket target silenced: WebKitGTK probes that socket with a plain
/// HTTP request before upgrading, and Dioxus logs the harmless rejection as an error on every start.
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,dioxus_desktop::edits=off,html5ever=error"));

    // A second subscriber (Dioxus installs its own when none exists) is not an error here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Reads the test hooks. The only place that touches the process environment.
fn hooks_from_env() -> Hooks {
    Hooks {
        exit_after_ms: std::env::var("HEDGEROW_EXIT_AFTER_MS")
            .ok()
            .and_then(|v| v.parse().ok()),
        dump_text: std::env::var_os("HEDGEROW_DUMP_TEXT").map(PathBuf::from),
        start_view: std::env::var("HEDGEROW_START_VIEW").ok(),
    }
}

/// Opens the store and config. UI-test mode uses an in-memory store, the default config and
/// the demo radio, and never reads or writes the config file or the data directory.
fn boot(ui_test: bool, hooks: Hooks) -> hedgerow_core::Result<Boot> {
    if ui_test {
        return Ok(Boot {
            store: Store::open_memory()?,
            config: Config::default(),
            fake: Some(fixtures::demo_radio()),
            hooks,
        });
    }

    std::fs::create_dir_all(data_dir())?;

    Ok(Boot {
        config: Config::load()?,
        store: Store::open(&data_dir().join("hedgerow.db"))?,
        fake: None,
        hooks,
    })
}

fn main() {
    init_logging();

    let ui_test = std::env::var("HEDGEROW_UI_TEST").is_ok_and(|v| !v.is_empty() && v != "0");
    let boot = match boot(ui_test, hooks_from_env()) {
        Ok(boot) => boot,
        Err(e) => {
            eprintln!("hedgerow: {e}");
            std::process::exit(1);
        }
    };
    let window = WindowBuilder::new()
        .with_title("Hedgerow")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(360.0, 320.0))
        .with_decorations(want_decorations(&boot.config.ui.decorations));

    // `with_window` replaces the whole window builder, so the icon goes on after it.
    let mut cfg = WindowConfig::new()
        .with_window(window)
        .with_menu(None::<dioxus::desktop::muda::Menu>);

    if let Some(icon) = window_icon() {
        cfg = cfg.with_icon(icon);
    }

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(boot)
        .launch(App);
}

/// The window icon: the 256 px Hedgerow icon, decoded at start.
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    dioxus::desktop::icon_from_memory(include_bytes!("../assets/icon-256.png"))
        .inspect_err(|e| tracing::warn!("window icon: {e}"))
        .ok()
}

/// Whether the window should carry a title bar. Config "on"/"off" wins; "auto" keeps native
/// decorations on macOS and Windows, and on Linux turns them off only under a tiling
/// compositor (tao's GTK backend otherwise paints its own header bar there).
fn want_decorations(setting: &str) -> bool {
    match setting {
        "on" => true,
        "off" => false,
        _ => {
            if !cfg!(target_os = "linux") {
                return true;
            }

            let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            let socket = |k: &str| std::env::var_os(k).is_some();

            !is_tiling_session(
                &desktop,
                socket("HYPRLAND_INSTANCE_SIGNATURE") || socket("SWAYSOCK") || socket("I3SOCK"),
            )
        }
    }
}

/// Heuristic: the session is a tiling window manager when a known tiler socket is set or
/// `XDG_CURRENT_DESKTOP` names one. Unknown desktops count as a DE (decorations stay on).
fn is_tiling_session(xdg_current_desktop: &str, tiler_socket: bool) -> bool {
    const TILERS: &[&str] = &[
        "hyprland",
        "sway",
        "i3",
        "river",
        "niri",
        "bspwm",
        "dwm",
        "qtile",
        "xmonad",
        "awesome",
        "herbstluftwm",
        "leftwm",
        "mango",
        "wayfire",
    ];

    tiler_socket
        || xdg_current_desktop
            .split(':')
            .map(|s| s.trim().to_ascii_lowercase())
            .any(|s| TILERS.contains(&s.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiling_detection() {
        assert!(is_tiling_session("Hyprland", false));
        assert!(is_tiling_session("sway", false));
        assert!(is_tiling_session("", true));
        assert!(!is_tiling_session("GNOME", false));
        assert!(!is_tiling_session("KDE", false));
        assert!(!is_tiling_session("COSMIC", false));
        assert!(!is_tiling_session("", false));
        assert!(is_tiling_session("ubuntu:GNOME:i3", false));
    }

    #[test]
    fn window_icon_decodes() {
        assert!(window_icon().is_some());
    }

    #[test]
    fn explicit_setting_wins() {
        assert!(want_decorations("on"));
        assert!(!want_decorations("off"));
    }
}
