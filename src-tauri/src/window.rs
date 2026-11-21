use crate::settings::Bounds;
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};

pub fn normal_bounds(window: &WebviewWindow) -> Option<Bounds> {
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    if size.width == 0 || size.height == 0 {
        return None;
    }
    Some(Bounds {
        x: position.x,
        y: position.y,
        width: size.width.try_into().ok()?,
        height: size.height.try_into().ok()?,
        maximized: false,
    })
}

pub fn restore(window: &WebviewWindow, saved: Bounds) -> Option<Bounds> {
    if saved.width <= 0 || saved.height <= 0 {
        return None;
    }
    let right = i64::from(saved.x.checked_add(saved.width)?);
    let bottom = i64::from(saved.y.checked_add(saved.height)?);
    let monitors = window.available_monitors().ok()?;
    let (area, monitor) = monitors.iter().map(|monitor| {
        let work = monitor.work_area();
        let x = i64::from(work.position.x);
        let y = i64::from(work.position.y);
        let width = right.min(x + i64::from(work.size.width)) - x.max(i64::from(saved.x));
        let height = bottom.min(y + i64::from(work.size.height)) - y.max(i64::from(saved.y));
        (width.max(0) * height.max(0), monitor)
    }).max_by_key(|(area, _)| *area)?;
    if area == 0 {
        return None;
    }

    let position = PhysicalPosition::new(saved.x, saved.y);
    window.set_position(position).ok()?;
    let outer = window.outer_size().ok()?;
    let inner = window.inner_size().ok()?;
    let scale = monitor.scale_factor();
    // Settings contain outer pixels. Tauri set_size takes client dimensions.
    let width = (saved.width as u32)
        .saturating_sub(outer.width.saturating_sub(inner.width))
        .max((560.0 * scale).round() as u32);
    let height = (saved.height as u32)
        .saturating_sub(outer.height.saturating_sub(inner.height))
        .max((260.0 * scale).round() as u32);
    window.set_size(PhysicalSize::new(width, height)).ok()?;
    window.set_position(position).ok()?;
    Some(saved)
}

pub fn resize(window: &WebviewWindow, edge: Option<&str>) {
    #[cfg(windows)]
    {
        if window.is_maximized().unwrap_or(true) {
            return;
        }
        let direction = match edge {
            Some("n") => "North",
            Some("s") => "South",
            Some("e") => "East",
            Some("w") => "West",
            Some("ne") => "NorthEast",
            Some("nw") => "NorthWest",
            Some("se") => "SouthEast",
            Some("sw") => "SouthWest",
            _ => return,
        };
        // Tauri exposes this runtime enum through the method's argument type.
        if let Ok(direction) = serde_json::from_value(serde_json::Value::from(direction)) {
            let _ = window.as_ref().window().start_resize_dragging(direction);
        }
    }
    #[cfg(target_os = "macos")]
    let _ = (window, edge);
}
