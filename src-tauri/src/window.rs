use crate::settings::Position;
use tauri::{Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

/// Content width in CSS px until the page's `fit` says otherwise.
pub const WIDTH: i64 = 800;

pub fn initial_height(rows: usize) -> i64 {
    // Title 40 + toolbar 56 + header 36 + rows (8 at most, then the table scrolls) + bottom gap 16.
    let body = if rows == 0 { 96 } else { (rows.min(8) as i64) * 48 };
    148 + body
}

pub fn position(window: &WebviewWindow) -> Option<Position> {
    if window.is_minimized().ok()? {
        return None;
    }
    let p = window.outer_position().ok()?;
    Some(Position { x: p.x, y: p.y })
}

fn contains(monitor: &Monitor, p: Position) -> bool {
    let origin = monitor.position();
    let size = monitor.size();
    let dx = i64::from(p.x) - i64::from(origin.x);
    let dy = i64::from(p.y) - i64::from(origin.y);
    (0..i64::from(size.width)).contains(&dx) && (0..i64::from(size.height)).contains(&dy)
}

fn move_to(window: &WebviewWindow, p: PhysicalPosition<i32>, monitor: &Monitor) {
    #[cfg(target_os = "macos")]
    {
        let _ = window.set_position(p.to_logical::<f64>(monitor.scale_factor()));
    }
    #[cfg(windows)]
    {
        let _ = monitor;
        let _ = window.set_position(p);
    }
}

pub fn initialize(window: &WebviewWindow, saved: Option<Position>, width: i64, height: i64) {
    frame(window);
    if let Some(p) = saved {
        let monitor = window.available_monitors().ok()
            .and_then(|list| list.into_iter().find(|m| contains(m, p)));
        if let Some(monitor) = monitor {
            move_to(window, PhysicalPosition::new(p.x, p.y), &monitor);
            fit_on(window, &monitor, p, width, height);
            return;
        }
    }
    fit(window, width, height);
    let _ = window.center();
    fit(window, width, height);
}

pub fn fit(window: &WebviewWindow, width: i64, height: i64) {
    let Some(p) = position(window) else { return };
    let monitor = window.current_monitor().ok().flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        fit_on(window, &monitor, p, width, height);
    }
}

fn fit_on(window: &WebviewWindow, monitor: &Monitor, p: Position, width: i64, height: i64) {
    let (Ok(outer), Ok(inner)) = (window.outer_size(), window.inner_size()) else { return };
    let work = monitor.work_area();
    if work.size.width == 0 || work.size.height == 0 {
        return;
    }

    let frame_w = outer.width.saturating_sub(inner.width);
    let frame_h = outer.height.saturating_sub(inner.height);
    let scale = monitor.scale_factor();
    let width = ((width.max(1) as f64 * scale).round() as u32)
        .min(work.size.width.saturating_sub(frame_w)).max(1);
    let height = ((height.max(1) as f64 * scale).round() as u32)
        .min(work.size.height.saturating_sub(frame_h)).max(1);
    let wanted = PhysicalSize::new(width, height);
    if inner != wanted && window.set_size(wanted).is_err() {
        return;
    }

    fn axis(old: i32, origin: i32, available: u32, used: u32) -> i32 {
        let lo = i64::from(origin);
        let hi = lo + i64::from(available.saturating_sub(used));
        i64::from(old).clamp(lo, hi).min(i64::from(i32::MAX)) as i32
    }

    let target = PhysicalPosition::new(
        axis(p.x, work.position.x, work.size.width, width.saturating_add(frame_w)),
        axis(p.y, work.position.y, work.size.height, height.saturating_add(frame_h)),
    );
    if window.outer_position().ok() != Some(target) {
        move_to(window, target, monitor);
    }
}

#[cfg(windows)]
pub fn frame(window: &WebviewWindow) {
    use std::mem::{size_of, size_of_val};
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_ROUND,
        },
    };
    let Ok(handle) = window.hwnd() else { return };
    let hwnd = HWND(handle.0.cast());
    let corner = DWMWCP_ROUND;
    let border: u32 = 0x002E_2E2E;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, std::ptr::from_ref(&corner).cast(),
            size_of_val(&corner) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd, DWMWA_BORDER_COLOR, std::ptr::from_ref(&border).cast(), size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn frame(_window: &WebviewWindow) {}
