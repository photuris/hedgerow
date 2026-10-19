//! Omarchy theme detection and change watching.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher as _};
use serde::{Deserialize, Serialize};

/// How long to collect a burst of file events before re-reading the theme.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// The prefix of the CSS custom properties the theme sets: `--rk-accent`.
const CSS_PREFIX: &str = "rk";

/// Colour scheme read from an Omarchy `colors.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Theme {
    /// "dark" or "light".
    pub mode: String,
    /// Key to "#rrggbb".
    pub colors: BTreeMap<String, String>,
}

/// `$XDG_STATE_HOME` or `~/.local/state`, plus `omarchy/current/theme/colors.toml`.
pub fn omarchy_theme_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("omarchy/current/theme/colors.toml")
}

/// Parses the current Omarchy theme; `None` when absent or unparsable.
pub fn detect() -> Option<Theme> {
    detect_at(&omarchy_theme_path())
}

/// Parses the theme file at `path`; `None` when absent or unparsable.
pub fn detect_at(path: &Path) -> Option<Theme> {
    let table: toml::Table = std::fs::read_to_string(path).ok()?.parse().ok()?;
    let mut theme = Theme {
        mode: "dark".to_string(),
        colors: BTreeMap::new(),
    };
    for (key, value) in table {
        let Some(value) = value.as_str() else {
            continue;
        };
        if key == "mode" {
            theme.mode = value.to_string();
        } else {
            theme.colors.insert(key, value.to_string());
        }
    }
    Some(theme)
}

/// CSS custom properties for `t`, one `--<CSS_PREFIX>-<key>` per colour,
/// plus `--<CSS_PREFIX>-on-accent` (black or white, whichever reads on the
/// accent) when the theme has an `accent`.
pub fn css_vars(t: &Theme) -> String {
    let mut css = format!(":root{{color-scheme:{};", t.mode);
    for (key, value) in &t.colors {
        css.push_str(&format!(
            "--{CSS_PREFIX}-{}:{value};",
            key.replace('_', "-")
        ));
    }
    if let Some(on) = t.colors.get("accent").and_then(|hex| on_color(hex)) {
        css.push_str(&format!("--{CSS_PREFIX}-on-accent:{on};"));
    }
    css.push('}');
    css
}

/// `#000000` or `#ffffff`, whichever contrasts more with `hex` (`#rrggbb`);
/// `None` when `hex` is not that shape. Uses the WCAG relative luminance
/// with the 0.179 crossover, where black and white contrast equally.
fn on_color(hex: &str) -> Option<&'static str> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| -> Option<f64> {
        let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).ok()?) / 255.0;
        Some(if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        })
    };
    let luminance = 0.2126 * channel(0)? + 0.7152 * channel(2)? + 0.0722 * channel(4)?;
    Some(if luminance > 0.179 {
        "#000000"
    } else {
        "#ffffff"
    })
}

/// Watches the current Omarchy theme; see [`watch_at`].
pub fn watch(on_change: impl Fn(Theme) + Send + 'static) -> notify::Result<Option<Watcher>> {
    watch_at(&omarchy_theme_path(), on_change)
}

/// Calls `on_change` whenever the theme at `path` changes.
///
/// Watches the theme directory and its parent, since Omarchy switches
/// themes by swapping the `theme` symlink in the parent. Returns `None`
/// when `path` does not exist.
pub fn watch_at(
    path: &Path,
    on_change: impl Fn(Theme) + Send + 'static,
) -> notify::Result<Option<Watcher>> {
    if !path.exists() {
        return Ok(None);
    }
    let (Some(theme_dir), Some(current_dir)) =
        (path.parent(), path.parent().and_then(Path::parent))
    else {
        return Ok(None);
    };

    // Read before watching so a change during setup is not lost.
    let last = detect_at(path);

    let (tx, rx) = mpsc::channel();
    let event_tx = tx.clone();
    // The callback only signals: calling watch/unwatch from inside it deadlocks.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, EventKind::Access(_))
        {
            let _ = event_tx.send(Message::Changed);
        }
    })?;

    let resolved = theme_dir.canonicalize().map_err(notify::Error::io)?;
    watcher.watch(&resolved, RecursiveMode::NonRecursive)?;
    watcher.watch(current_dir, RecursiveMode::NonRecursive)?;

    let worker = Worker {
        path: path.to_path_buf(),
        watched: resolved,
        last,
        watcher,
        rx,
    };
    let handle = std::thread::spawn(move || worker.run(on_change));
    Ok(Some(Watcher {
        stop: tx,
        worker: Some(handle),
    }))
}

enum Message {
    Changed,
    Stop,
}

/// Stops watching when dropped.
pub struct Watcher {
    stop: Sender<Message>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(Message::Stop);

        if let Some(worker) = self.worker.take() {
            // Dropped from inside `on_change`: the worker cannot join itself, so
            // it exits on the stop message once the callback returns.
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

struct Worker {
    path: PathBuf,
    /// The resolved theme directory currently watched.
    watched: PathBuf,
    last: Option<Theme>,
    /// Owned here so dropping the worker stops the watches.
    watcher: notify::RecommendedWatcher,
    rx: Receiver<Message>,
}

impl Worker {
    fn run(mut self, on_change: impl Fn(Theme)) {
        loop {
            match self.rx.recv() {
                Ok(Message::Changed) => {}
                Ok(Message::Stop) | Err(_) => return,
            }
            // Debounce: swallow events for a fixed window, so a steady stream
            // of unrelated events cannot postpone the check forever.
            let deadline = Instant::now() + DEBOUNCE;

            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match self.rx.recv_timeout(left) {
                    Ok(Message::Changed) => {}
                    Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => break,
                }
            }
            self.follow_symlink();
            if let Some(theme) = detect_at(&self.path)
                && self.last.as_ref() != Some(&theme)
            {
                self.last = Some(theme.clone());
                on_change(theme);
            }
        }
    }

    /// Re-attaches the watch when the `theme` symlink now points elsewhere.
    fn follow_symlink(&mut self) {
        let Some(dir) = self.path.parent().and_then(|d| d.canonicalize().ok()) else {
            return;
        };
        if dir == self.watched {
            return;
        }
        let _ = self.watcher.unwatch(&self.watched);
        match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => self.watched = dir,
            Err(e) => tracing::warn!("watch {}: {e}", dir.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn colors(accent: &str) -> String {
        format!("mode = \"dark\"\naccent = \"{accent}\"\n")
    }

    /// Waits for a theme whose accent is `accent`; earlier ones are skipped.
    fn expect_accent(rx: &mpsc::Receiver<Theme>, accent: &str) {
        expect_accent_within(rx, accent, TIMEOUT);
    }

    /// Like [`expect_accent`] with an explicit time limit.
    fn expect_accent_within(rx: &mpsc::Receiver<Theme>, accent: &str, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let theme = rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no theme with accent {accent} within {limit:?}"));
            if theme.colors.get("accent").map(String::as_str) == Some(accent) {
                return;
            }
        }
    }

    #[test]
    fn css_vars_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colors.toml");
        std::fs::write(
            &path,
            "mode = \"dark\"\naccent = \"#CC785C\"\ndark_background = \"#141311\"\nsize = 3\n",
        )
        .unwrap();
        let theme = detect_at(&path).unwrap();
        assert_eq!(theme.colors.len(), 2, "non-string values are ignored");
        let css = css_vars(&theme);
        assert!(css.starts_with(":root{"), "{css}");
        assert!(css.contains("color-scheme:dark;"), "{css}");
        assert!(css.contains("--rk-accent:#CC785C;"), "{css}");
        assert!(css.contains("--rk-dark-background:#141311;"), "{css}");
        assert!(css.contains("--rk-on-accent:#000000;"), "{css}");
        assert!(css.ends_with('}'), "{css}");
    }

    #[test]
    fn on_color_picks_black_for_light_accents() {
        // Terracotta reads better with black by WCAG (6.4:1 against 3.3:1).
        assert_eq!(on_color("#CC785C"), Some("#000000"));
        assert_eq!(on_color("#1e66f5"), Some("#ffffff"));
        assert_eq!(on_color("#f9e2af"), Some("#000000"));
        assert_eq!(on_color("#000000"), Some("#ffffff"));
        assert_eq!(on_color("#ffffff"), Some("#000000"));
        assert_eq!(on_color("CC785C"), None);
        assert_eq!(on_color("#fff"), None);
        assert_eq!(on_color("#zzzzzz"), None);
    }

    #[test]
    fn detect_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omarchy/current/theme/colors.toml");
        assert_eq!(detect_at(&path), None);
        assert!(watch_at(&path, |_| {}).unwrap().is_none());
    }

    /// `themes/a` with accent #111111 and `current/theme -> ../themes/a`.
    #[cfg(unix)]
    fn omarchy_layout(root: &Path) -> PathBuf {
        let themes = root.join("themes");
        let current = root.join("current");
        std::fs::create_dir_all(themes.join("a")).unwrap();
        std::fs::create_dir_all(themes.join("b")).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(themes.join("a/colors.toml"), colors("#111111")).unwrap();
        std::fs::write(themes.join("b/colors.toml"), colors("#333333")).unwrap();
        std::os::unix::fs::symlink("../themes/a", current.join("theme")).unwrap();
        current.join("theme/colors.toml")
    }

    #[cfg(unix)]
    #[test]
    fn watch_fires_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = omarchy_layout(dir.path());
        let (tx, rx) = mpsc::channel();
        let _watcher = watch_at(&path, move |t| {
            let _ = tx.send(t);
        })
        .unwrap()
        .expect("theme exists");

        std::fs::write(dir.path().join("themes/a/colors.toml"), colors("#222222")).unwrap();
        expect_accent(&rx, "#222222");
    }

    #[cfg(unix)]
    #[test]
    fn watch_follows_symlink_swap() {
        let dir = tempfile::tempdir().unwrap();
        let path = omarchy_layout(dir.path());
        let (tx, rx) = mpsc::channel();
        let _watcher = watch_at(&path, move |t| {
            let _ = tx.send(t);
        })
        .unwrap()
        .expect("theme exists");

        let current = dir.path().join("current");
        std::os::unix::fs::symlink("../themes/b", current.join("theme.tmp")).unwrap();
        std::fs::rename(current.join("theme.tmp"), current.join("theme")).unwrap();
        expect_accent(&rx, "#333333");

        std::fs::write(dir.path().join("themes/b/colors.toml"), colors("#444444")).unwrap();
        expect_accent(&rx, "#444444");
    }

    #[cfg(unix)]
    #[test]
    fn watch_survives_continuous_unrelated_events() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let path = omarchy_layout(dir.path());
        let (tx, rx) = mpsc::channel();
        let _watcher = watch_at(&path, move |t| {
            let _ = tx.send(t);
        })
        .unwrap()
        .expect("theme exists");

        // Writes into the watched `current/` dir every 10 ms, far below the debounce.
        let running = Arc::new(AtomicBool::new(true));
        let noise = {
            let running = Arc::clone(&running);
            let file = dir.path().join("current/noise");
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    std::fs::write(&file, "x").unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };

        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.path().join("themes/a/colors.toml"), colors("#222222")).unwrap();

        let started = Instant::now();
        let seen = std::panic::catch_unwind(|| {
            expect_accent_within(&rx, "#222222", Duration::from_millis(1500));
        });

        running.store(false, Ordering::Relaxed);
        noise.join().unwrap();

        assert!(
            seen.is_ok(),
            "no update within {:?} while unrelated events kept arriving",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_watcher_inside_callback_does_not_panic() {
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let path = omarchy_layout(dir.path());
        let slot: Arc<Mutex<Option<Watcher>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let callback_slot = Arc::clone(&slot);
        let watcher = watch_at(&path, move |_| {
            // Unsubscribe from inside the callback, as a one-shot listener would.
            let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(callback_slot.lock().unwrap().take());
            }));
            let _ = tx.send(dropped.is_ok());
        })
        .unwrap()
        .expect("theme exists");

        *slot.lock().unwrap() = Some(watcher);
        std::fs::write(dir.path().join("themes/a/colors.toml"), colors("#222222")).unwrap();

        assert_eq!(
            rx.recv_timeout(TIMEOUT),
            Ok(true),
            "dropping the watcher inside its callback panicked"
        );
        assert!(slot.lock().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_watcher_stops_callbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = omarchy_layout(dir.path());
        let (tx, rx) = mpsc::channel();
        let watcher = watch_at(&path, move |t| {
            let _ = tx.send(t);
        })
        .unwrap()
        .expect("theme exists");

        drop(watcher);
        std::fs::write(dir.path().join("themes/a/colors.toml"), colors("#222222")).unwrap();

        // The worker is joined, so its sender is gone: a later change gets no callback.
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(600)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }
}
