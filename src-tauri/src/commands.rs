//! Everything the UI can ask for. Plain commands return at once. The `async` ones run on a
//! pool, so a slow device or registry cannot stall the UI thread.

use estimlock_engine::config::{Config, MAX_EFFECT_HZ};
use estimlock_engine::machine::{Command, EventKind, Snapshot};
use estimlock_engine::wasapi::{Endpoint, list_endpoints};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_autostart::ManagerExt;

use crate::shell::{self, MAIN, Shell, ShellInfo, WIDGET};

/// `EventKind` from the UI. The engine type only serializes.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Press {
    Begging,
    Came,
}

/// What a window needs to draw its first frame.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Init {
    snapshot: Snapshot,
    shell: ShellInfo,
    max_effect_hz: f64,
}

#[tauri::command]
pub fn get_init(shell: State<Shell>) -> Result<Init, String> {
    Ok(Init {
        snapshot: shell.snapshot().ok_or("stopped")?,
        shell: shell.info(),
        max_effect_hz: MAX_EFFECT_HZ,
    })
}

/// Called once a window has drawn. Windows start hidden, so none shows blank or stale.
#[tauri::command]
pub fn window_ready(window: WebviewWindow, shell: State<Shell>) -> Result<(), String> {
    let show = match window.label() {
        MAIN => !shell.start_hidden,
        WIDGET => shell.widget_visible(),
        _ => false,
    };
    if show {
        window.show().map_err(|error| error.to_string())?;
    }
    if show && window.label() == MAIN {
        let _ = window.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub fn set_on(shell: State<Shell>, on: bool) {
    shell.send(Command::SetOn(on));
}

#[tauri::command]
pub fn lock(shell: State<Shell>) {
    shell.send(Command::Lock);
}

#[tauri::command]
pub fn begin_unlock(shell: State<Shell>) {
    shell.send(Command::BeginUnlock);
}

#[tauri::command]
pub fn cancel_unlock(shell: State<Shell>) {
    shell.send(Command::CancelUnlock);
}

#[tauri::command]
pub fn toggle_ramp(shell: State<Shell>) {
    shell.send(Command::ToggleRamp);
}

#[tauri::command]
pub fn press(shell: State<Shell>, kind: Press) {
    shell.send(Command::Press(match kind {
        Press::Begging => EventKind::Begging,
        Press::Came => EventKind::Came,
    }));
}

#[tauri::command]
pub fn set_level(shell: State<Shell>, level: f64) {
    shell.send(Command::SetLevel(level));
}

#[tauri::command]
pub fn update_config(shell: State<Shell>, config: Config) {
    shell.send(Command::UpdateConfig(Box::new(config)));
}

#[tauri::command]
pub async fn list_devices() -> Result<Vec<Endpoint>, String> {
    tauri::async_runtime::spawn_blocking(list_endpoints)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_widget_visible(app: AppHandle, visible: bool) {
    shell::set_widget_visible(&app, visible);
}

#[tauri::command(async)]
pub fn set_start_with_windows(app: AppHandle, enabled: bool) -> Result<(), String> {
    let shell = app.state::<Shell>();
    if shell.is_locked() {
        return Err("locked".into());
    }
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|error| error.to_string())?;
    shell.note_start_with_windows(enabled);
    shell::emit_shell_info(&app);
    Ok(())
}
