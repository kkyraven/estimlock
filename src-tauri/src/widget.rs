//! Widget placement. A saved position can point at a monitor that has since gone.

use tauri::{PhysicalPosition, Runtime, WebviewWindow};

/// Gap from the screen edge for the default position.
const MARGIN: i32 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

impl From<&tauri::PhysicalRect<i32, u32>> for Rect {
    fn from(rect: &tauri::PhysicalRect<i32, u32>) -> Self {
        Self {
            x: rect.position.x,
            y: rect.position.y,
            width: rect.size.width as i32,
            height: rect.size.height as i32,
        }
    }
}

/// Whether the middle of the top edge is on a monitor. That edge is the drag handle, so a
/// widget that passes can always be grabbed, even if part of it hangs off the screen.
fn is_reachable(position: (i32, i32), width: i32, monitors: &[Rect]) -> bool {
    let (x, y) = (position.0 + width / 2, position.1 + 1);
    monitors.iter().any(|monitor| monitor.contains(x, y))
}

fn bottom_right(work_area: Rect, width: i32, height: i32) -> (i32, i32) {
    (
        work_area.x + work_area.width - width - MARGIN,
        work_area.y + work_area.height - height - MARGIN,
    )
}

/// Moves the widget to `saved` if that is reachable, otherwise to the corner of the primary
/// monitor.
pub fn place<R: Runtime>(
    window: &WebviewWindow<R>,
    saved: Option<(i32, i32)>,
) -> tauri::Result<()> {
    let size = window.outer_size()?;
    let (width, height) = (size.width as i32, size.height as i32);
    let monitors: Vec<Rect> = window
        .available_monitors()?
        .iter()
        .map(|monitor| Rect::from(monitor.work_area()))
        .collect();

    let position = saved
        .filter(|position| is_reachable(*position, width, &monitors))
        .or_else(|| {
            let primary = window.primary_monitor().ok()??;
            Some(bottom_right(primary.work_area().into(), width, height))
        });
    match position {
        Some((x, y)) => window.set_position(PhysicalPosition::new(x, y)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect {
        x: 0,
        y: 0,
        width: 2560,
        height: 1440,
    };
    const RIGHT: Rect = Rect {
        x: 2560,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn a_position_on_a_removed_monitor_is_not_reachable() {
        assert!(is_reachable((3000, 100), 248, &[LEFT, RIGHT]));
        assert!(!is_reachable((3000, 100), 248, &[LEFT]));
    }

    #[test]
    fn hanging_off_an_edge_is_fine_while_the_handle_is_on_screen() {
        assert!(is_reachable((-100, 100), 248, &[LEFT]));
        assert!(!is_reachable((-200, 100), 248, &[LEFT]));
        assert!(!is_reachable((100, -40), 248, &[LEFT]));
    }

    #[test]
    fn default_position_is_inside_the_work_area() {
        let (x, y) = bottom_right(LEFT, 248, 88);
        assert!(is_reachable((x, y), 248, &[LEFT]));
        assert_eq!((x + 248 + MARGIN, y + 88 + MARGIN), (2560, 1440));
    }
}
