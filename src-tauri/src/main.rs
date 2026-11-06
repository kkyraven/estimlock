#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod shell;
mod store;
mod tray;
mod widget;

use std::path::PathBuf;

use estimlock_engine::machine::Command;
use estimlock_engine::wasapi::spawn_runner;
use tauri::{App, AppHandle, Manager, RunEvent, WebviewWindowBuilder, Window, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::ShortcutState;

use shell::{MAIN, Shell, WIDGET};
use store::Store;
use tray::Tray;

/// Passed by the Start with Windows entry, so a login does not open the window.
const HIDDEN_ARG: &str = "--hidden";

/// Points a debug build at another state folder, so test runs leave the real config alone.
/// Never in release: a fresh folder has no lock in it, so it would be a way past one.
const DATA_DIR_VAR: &str = "ESTIMLOCK_DATA_DIR";

fn data_dir(app: &App) -> tauri::Result<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os(DATA_DIR_VAR)
    {
        return Ok(dir.into());
    }
    app.path().app_data_dir()
}

fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let (store, saved) = Store::open(&data_dir(app)?);
    let widget_state = saved.widget;
    // Here and not in `main`: a second launch has already exited by now, without touching
    // the device. Two engines would fight over it.
    let runner = spawn_runner(saved.engine);
    let start_with_windows = app.autolaunch().is_enabled().unwrap_or(false);
    let start_hidden = std::env::args().any(|arg| arg == HIDDEN_ARG);
    app.manage(Shell::new(
        runner,
        store,
        widget_state,
        start_with_windows,
        start_hidden,
    ));

    for config in &app.config().app.windows {
        let window = WebviewWindowBuilder::from_config(app.handle(), config)?.build()?;
        match window.label() {
            // Here, not in the config: the config's `center` misses on a scaled display.
            MAIN => window.center()?,
            _ => widget::place(&window, widget_state.position)?,
        }
    }
    app.manage(Tray::build(app.handle(), widget_state.visible)?);
    shell::spawn_pump(app.handle().clone());
    Ok(())
}

fn on_window_event(window: &Window, event: &WindowEvent) {
    match event {
        // Closing never ends the app, it only hides. Quit is in the tray.
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if window.label() == MAIN {
                let _ = window.hide();
            } else {
                shell::set_widget_visible(window.app_handle(), false);
            }
        }
        WindowEvent::Moved(position) if window.label() == WIDGET => {
            let shell = window.state::<Shell>();
            shell.note_widget_position(position.x, position.y);
        }
        _ => {}
    }
}

fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Backstop for `shell::quit`. Windows ending the session skips this and goes straight
        // to `Exit`, so a lock can never block shutdown or logoff.
        RunEvent::ExitRequested { api, .. } if app.state::<Shell>().quit_blocked() => {
            api.prevent_exit();
        }
        RunEvent::Exit => app.state::<Shell>().stop(),
        _ => {}
    }
}

fn main() {
    tauri::Builder::default()
        // First, so a second launch shows this instance and exits before anything else runs.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            shell::show_main(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        app.state::<Shell>().send(Command::Panic);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::get_init,
            commands::window_ready,
            commands::set_on,
            commands::lock,
            commands::begin_unlock,
            commands::cancel_unlock,
            commands::toggle_ramp,
            commands::press,
            commands::set_level,
            commands::update_config,
            commands::list_devices,
            commands::set_widget_visible,
            commands::set_start_with_windows,
        ])
        .setup(setup)
        .on_window_event(on_window_event)
        .build(tauri::generate_context!())
        .expect("failed to start estimlock")
        .run(on_run_event);
}
