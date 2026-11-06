//! App state around the engine: the runner, the store, the windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use estimlock_engine::machine::{Command, Snapshot};
use estimlock_engine::runner::Runner;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::{
    Code, Error as GlobalShortcutError, GlobalShortcutExt, Modifiers, Shortcut,
};

use crate::store::{Saved, Store, WidgetState};
use crate::tray::Tray;
use crate::widget;

/// How often to retry panic keys another app is holding.
const PANIC_RETRY: Duration = Duration::from_secs(2);

/// About 20 pushes a second.
const PUSH_INTERVAL: Duration = Duration::from_millis(50);

pub const MAIN: &str = "main";
pub const WIDGET: &str = "widget";
const SNAPSHOT_EVENT: &str = "snapshot";
const SHELL_EVENT: &str = "shell";

/// Shell state the UI shows, sent with the `shell` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellInfo {
    pub widget_visible: bool,
    pub start_with_windows: bool,
    /// The engine died or is hanging, so the snapshot is frozen. Clears if a hang ends.
    pub engine_stopped: bool,
    /// The latest state could not be saved. Clears once a save succeeds.
    pub save_failed: bool,
    /// The panic hotkey is enabled but not working, usually because another app holds the keys.
    pub panic_failed: bool,
}

pub struct Shell {
    /// `None` once stopped. Dropping the runner is the engine's only stop.
    runner: Mutex<Option<Runner>>,
    store: Store,
    widget: Mutex<WidgetState>,
    start_with_windows: AtomicBool,
    /// Lock was sent and no snapshot has shown it yet. Snapshots lag a tick or more behind,
    /// and Quit must not pass in that gap.
    lock_sent: AtomicBool,
    panic_failed: AtomicBool,
    /// Launched by Windows at login: stay in the tray until asked.
    pub start_hidden: bool,
}

impl Shell {
    pub fn new(
        runner: Runner,
        store: Store,
        widget: WidgetState,
        start_with_windows: bool,
        start_hidden: bool,
    ) -> Self {
        Self {
            runner: Mutex::new(Some(runner)),
            store,
            widget: Mutex::new(widget),
            start_with_windows: AtomicBool::new(start_with_windows),
            lock_sent: AtomicBool::new(false),
            panic_failed: AtomicBool::new(false),
            start_hidden,
        }
    }

    pub fn send(&self, command: Command) {
        if matches!(command, Command::Lock) {
            self.lock_sent.store(true, Ordering::SeqCst);
        }
        if let Some(runner) = &*self.runner() {
            runner.send(command);
        }
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        let snapshot = self.runner().as_ref().map(Runner::snapshot)?;
        // From here the snapshot speaks for the lock, including when it later unlocks.
        if snapshot.locked {
            self.lock_sent.store(false, Ordering::SeqCst);
        }
        Some(snapshot)
    }

    /// Whether the lock is on, counting a Lock the engine has not reported yet.
    pub fn is_locked(&self) -> bool {
        let reported = self.snapshot().is_some_and(|snapshot| snapshot.locked);
        reported || self.lock_sent.load(Ordering::SeqCst)
    }

    fn engine_alive(&self) -> bool {
        self.runner().as_ref().is_some_and(Runner::alive)
    }

    /// Quit is refused while locked, unless the engine is dead or hanging: the lock can no
    /// longer be unlocked then, and a frozen snapshot must not trap the user.
    pub fn quit_blocked(&self) -> bool {
        self.is_locked() && self.engine_alive()
    }

    pub fn info(&self) -> ShellInfo {
        ShellInfo {
            widget_visible: self.widget_visible(),
            start_with_windows: self.start_with_windows.load(Ordering::Relaxed),
            engine_stopped: !self.engine_alive(),
            save_failed: self.store.save_failed(),
            panic_failed: self.panic_failed.load(Ordering::Relaxed),
        }
    }

    pub fn widget_visible(&self) -> bool {
        self.widget().visible
    }

    pub fn note_widget_position(&self, x: i32, y: i32) {
        self.widget().position = Some((x, y));
    }

    pub fn note_start_with_windows(&self, enabled: bool) {
        self.start_with_windows.store(enabled, Ordering::Relaxed);
    }

    /// Saves, then stops the engine. In that order: stopping turns the machine Off, and a
    /// locked session ending with Windows has to come back On.
    pub fn stop(&self) {
        if let Some(snapshot) = self.snapshot() {
            self.store.flush(&self.saved(&snapshot));
        }
        drop(self.runner().take());
    }

    fn saved(&self, snapshot: &Snapshot) -> Saved {
        Saved {
            engine: snapshot.persisted.clone(),
            widget: *self.widget(),
        }
    }

    fn runner(&self) -> MutexGuard<'_, Option<Runner>> {
        self.runner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn widget(&self) -> MutexGuard<'_, WidgetState> {
        self.widget.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Pushes snapshots and shell state to both windows when they change, and keeps the tray and
/// the file current. Runs until the runner is dropped, so it keeps going after the engine dies
/// and can report that. Never touches the device, so it cannot stall on one.
pub fn spawn_pump(app: AppHandle) {
    thread::spawn(move || {
        let shell = app.state::<Shell>();
        let tray = app.state::<Tray>();
        let mut last = None;
        let mut last_info = shell.info();
        let mut panic_keys = PanicKeys::default();
        while let Some(snapshot) = shell.snapshot() {
            shell.store.save_throttled(&shell.saved(&snapshot));
            let wanted = snapshot.persisted.config.panic_hotkey;
            let failed = panic_keys.update(wanted, Instant::now(), |wanted| {
                sync_panic_hotkey(&app, wanted).is_ok()
            });
            shell.panic_failed.store(failed, Ordering::Relaxed);
            let info = shell.info();
            if info != last_info {
                // Only fails if a window is mid-teardown, like the push below.
                let _ = app.emit(SHELL_EVENT, info);
                last_info = info;
            }
            // Every pass, as Quit depends on the lock flag and the heartbeat too.
            tray.sync(&snapshot, info.engine_stopped, shell.quit_blocked());
            if last.as_ref() != Some(&snapshot) {
                // Only fails if a window is mid-teardown. The next push catches it up.
                let _ = app.emit(SNAPSHOT_EVENT, &snapshot);
                last = Some(snapshot);
            }
            thread::sleep(PUSH_INTERVAL);
        }
    });
}

/// Keeps the panic keys registered while wanted. Another app can hold them and let go later,
/// and a locked session cannot reach the switch, so a failed registration keeps retrying.
#[derive(Debug, Default)]
struct PanicKeys {
    held: bool,
    retry_at: Option<Instant>,
}

impl PanicKeys {
    /// Brings the keys in line with `wanted` and returns whether they are wanted but missing.
    /// `sync` registers or releases them and returns success.
    fn update(&mut self, wanted: bool, now: Instant, sync: impl FnOnce(bool) -> bool) -> bool {
        if !wanted {
            self.retry_at = None;
        }
        if wanted != self.held && self.retry_at.is_none_or(|at| now >= at) {
            // A failed release means nothing was held, so only registering can fail for real.
            if sync(wanted) || !wanted {
                self.held = wanted;
                self.retry_at = None;
            } else {
                self.retry_at = Some(now + PANIC_RETRY);
            }
        }
        wanted && !self.held
    }
}

/// Registers or releases the panic keys. A failure must reach the UI: someone relying on a
/// safety control has to know it is not there.
fn sync_panic_hotkey(app: &AppHandle, wanted: bool) -> Result<(), GlobalShortcutError> {
    let shortcuts = app.global_shortcut();
    let keys = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
    if wanted {
        shortcuts.register(keys)
    } else {
        shortcuts.unregister(keys)
    }
}

pub fn emit_shell_info<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(SHELL_EVENT, app.state::<Shell>().info());
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Shows or hides the widget, from Settings, the tray or Alt+F4.
pub fn set_widget_visible(app: &AppHandle, visible: bool) {
    let shell = app.state::<Shell>();
    shell.widget().visible = visible;
    if let Some(window) = app.get_webview_window(WIDGET) {
        if visible {
            // Monitors may have changed since it was last shown.
            let position = shell.widget().position;
            let _ = widget::place(&window, position);
            let _ = window.show();
        } else {
            let _ = window.hide();
        }
    }
    app.state::<Tray>().set_widget_checked(visible);
    emit_shell_info(app);
}

/// The one way to quit. The engine leaves this to the shell, so the refusal is here.
pub fn quit<R: Runtime>(app: &AppHandle<R>) {
    if !app.state::<Shell>().quit_blocked() {
        app.exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_keys_retry_until_registered_and_stop_trying_between_retries() {
        let start = Instant::now();
        let mut keys = PanicKeys::default();
        assert!(keys.update(true, start, |_| false), "reported as missing");
        assert!(
            keys.update(true, start + Duration::from_millis(50), |_| panic!(
                "too soon"
            )),
            "still missing"
        );
        assert!(
            !keys.update(true, start + PANIC_RETRY, |_| true),
            "recovered"
        );
    }

    #[test]
    fn panic_keys_report_nothing_when_not_wanted() {
        let start = Instant::now();
        let mut keys = PanicKeys::default();
        assert!(keys.update(true, start, |_| false));
        assert!(!keys.update(false, start, |_| panic!("nothing to release")));
    }

    #[test]
    fn turning_panic_keys_off_and_on_retries_at_once() {
        let start = Instant::now();
        let mut keys = PanicKeys::default();
        keys.update(true, start, |_| false);
        keys.update(false, start, |_| panic!("nothing to release"));
        assert!(!keys.update(true, start, |_| true), "no leftover wait");
    }

    #[test]
    fn panic_keys_are_released_when_turned_off() {
        let start = Instant::now();
        let mut keys = PanicKeys::default();
        keys.update(true, start, |_| true);
        let mut released = false;
        assert!(!keys.update(false, start, |wanted| {
            released = !wanted;
            true
        }));
        assert!(released);
    }
}
