//! The tray icon and its menu.

use std::sync::{Mutex, PoisonError};

use estimlock_engine::machine::{DeviceStatus, Snapshot};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::shell::{self, Shell};

const SHOW: &str = "show";
const WIDGET: &str = "widget";
const QUIT: &str = "quit";

pub struct Tray {
    icon: TrayIcon,
    widget: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
    shown: Mutex<Shown>,
}

/// What the tray last displayed, so unchanged ticks cost nothing. The default matches a
/// fresh tray: empty tooltip, Quit enabled.
#[derive(Default, PartialEq)]
struct Shown {
    tooltip: String,
    quit_blocked: bool,
}

fn tooltip(snapshot: &Snapshot, engine_stopped: bool) -> String {
    if engine_stopped {
        return "estimlock engine stopped".to_owned();
    }
    let level = match snapshot.device {
        DeviceStatus::NotSelected | DeviceStatus::Lost { .. } => String::new(),
        _ => format!(" {:.0}%", snapshot.output * 100.0),
    };
    let power = if snapshot.on { "On" } else { "Off" };
    let lock = if snapshot.locked { ", Locked" } else { "" };
    format!("estimlock{level}, {power}{lock}")
}

impl Tray {
    pub fn build(app: &AppHandle, widget_visible: bool) -> tauri::Result<Self> {
        let show = MenuItem::with_id(app, SHOW, "Show", true, None::<&str>)?;
        let widget =
            CheckMenuItem::with_id(app, WIDGET, "Widget", true, widget_visible, None::<&str>)?;
        let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
        let separator = PredefinedMenuItem::separator(app)?;
        let menu = Menu::with_items(app, &[&show, &widget, &separator, &quit])?;

        let mut builder = TrayIconBuilder::with_id("main")
            .tooltip("estimlock")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                SHOW => shell::show_main(app),
                WIDGET => {
                    let visible = app.state::<Shell>().widget_visible();
                    shell::set_widget_visible(app, !visible);
                }
                // Refused while locked, whatever state the menu item is in.
                QUIT => shell::quit(app),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    shell::show_main(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        Ok(Self {
            icon: builder.build(app)?,
            widget,
            quit,
            shown: Mutex::default(),
        })
    }

    /// Brings the tooltip and Quit item up to date.
    pub fn sync(&self, snapshot: &Snapshot, engine_stopped: bool, quit_blocked: bool) {
        let next = Shown {
            tooltip: tooltip(snapshot, engine_stopped),
            quit_blocked,
        };
        let mut shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        if shown.tooltip != next.tooltip {
            let _ = self.icon.set_tooltip(Some(&next.tooltip));
        }
        if shown.quit_blocked != next.quit_blocked {
            let _ = self.quit.set_enabled(!next.quit_blocked);
        }
        *shown = next;
    }

    pub fn set_widget_checked(&self, checked: bool) {
        let _ = self.widget.set_checked(checked);
    }
}

#[cfg(test)]
mod tests {
    use estimlock_engine::config::PersistedState;
    use estimlock_engine::machine::Machine;

    use super::*;

    fn snapshot(locked: bool, on: bool, device: DeviceStatus) -> Snapshot {
        let machine = Machine::new(PersistedState {
            locked,
            on,
            level: 0.42,
            base: 0.42,
            ..Default::default()
        });
        machine.snapshot(0.0, device)
    }

    #[test]
    fn tooltip_says_on_or_off_and_locked() {
        assert_eq!(
            tooltip(&snapshot(true, true, DeviceStatus::Connected), false),
            "estimlock 42%, On, Locked"
        );
        assert_eq!(
            tooltip(&snapshot(false, false, DeviceStatus::NotSelected), false),
            "estimlock, Off"
        );
    }

    #[test]
    fn tooltip_does_not_show_a_frozen_level() {
        assert_eq!(
            tooltip(&snapshot(true, true, DeviceStatus::Connected), true),
            "estimlock engine stopped"
        );
    }
}
