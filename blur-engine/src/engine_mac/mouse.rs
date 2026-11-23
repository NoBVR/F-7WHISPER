// Changed for macOS by F-7WHISPER on 2026-11-22, from Blur AutoClicker v3.10.2's
// src-tauri/src/engine/mouse.rs (GPL-3.0, see LICENSE-BLUR-GPL-3.0). Diff it against
// ../engine/mouse.rs to see every change.

use super::cycle::{execute_click_cycle, ClickCycleKind, ClickCyclePlan};
use super::worker::{sleep_interruptible, RunControl};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

use super::{SendResult, AUTOCLICKER_EXTRA_INFO};
// macOS: keep Blur's button flags at the Quartz boundary.
use core_graphics::{
    display::CGDisplay,
    event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
const MOUSEEVENTF_MOVE: u32 = 0x01;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x02;
const MOUSEEVENTF_LEFTUP: u32 = 0x04;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x08;
const MOUSEEVENTF_RIGHTUP: u32 = 0x10;
const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x20;
const MOUSEEVENTF_MIDDLEUP: u32 = 0x40;
const VK_LBUTTON: u16 = 1;
const VK_RBUTTON: u16 = 2;
const VK_MBUTTON: u16 = 4;

pub enum INPUT {
    Move(CGPoint),
    Button(u32),
}

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

// macOS: a posted move reaches the cursor asynchronously, so a button posted right
// after it would still read the old position. Each Blur run has its own thread, so
// buttons use that run's own last move while it is fresh.
thread_local! {
    static LAST_MOVE: std::cell::Cell<Option<(CGPoint, Instant)>> = const { std::cell::Cell::new(None) };
}
const MOVE_FRESH: Duration = Duration::from_millis(100);

fn post(inputs: &[INPUT]) -> u32 {
    use CGEventType::*;
    use CGMouseButton::{Center, Left, Right};
    let Some(source) = source() else { return 0 };
    let mut sent = 0;
    for input in inputs {
        let (kind, button, point) = match *input {
            INPUT::Move(point) => {
                LAST_MOVE.with(|last| last.set(Some((point, Instant::now()))));
                (MouseMoved, Left, point)
            }
            INPUT::Button(flags) => {
                let (kind, button) = match flags {
                    MOUSEEVENTF_LEFTDOWN => (LeftMouseDown, Left),
                    MOUSEEVENTF_LEFTUP => (LeftMouseUp, Left),
                    MOUSEEVENTF_RIGHTDOWN => (RightMouseDown, Right),
                    MOUSEEVENTF_RIGHTUP => (RightMouseUp, Right),
                    MOUSEEVENTF_MIDDLEDOWN => (OtherMouseDown, Center),
                    MOUSEEVENTF_MIDDLEUP => (OtherMouseUp, Center),
                    _ => break,
                };
                let point = match LAST_MOVE.with(|last| last.get()) {
                    Some((moved, at)) if at.elapsed() < MOVE_FRESH => moved,
                    _ => {
                        let Ok(cursor) = CGEvent::new(source.clone()) else { break };
                        cursor.location()
                    }
                };
                (kind, button, point)
            }
        };
        let Ok(event) = CGEvent::new_mouse_event(source.clone(), kind, point, button) else { break };
        if matches!(input, INPUT::Button(_)) {
            event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, button as i64);
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
        }
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, AUTOCLICKER_EXTRA_INFO as i64);
        event.post(CGEventTapLocation::HID);
        sent += 1;
    }
    sent
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualScreenRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl VirtualScreenRect {
    #[inline]
    pub fn new(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    #[inline]
    pub fn right(self) -> i32 {
        self.left + self.width
    }

    #[inline]
    pub fn bottom(self) -> i32 {
        self.top + self.height
    }

    #[inline]
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right() && y >= self.top && y < self.bottom()
    }

    fn normalize_x(&self, pixel_x: i32) -> i32 {
        let relative_x = pixel_x as f64 - self.left as f64;
        let ratio = relative_x / self.width as f64;
        (ratio * 65535.0).round() as i32
    }
    fn normalize_y(&self, pixel_y: i32) -> i32 {
        let relative_y = pixel_y as f64 - self.top as f64;
        let ratio = relative_y / self.height as f64;
        (ratio * 65535.0).round() as i32
    }

    #[inline]
    pub fn offset_from(self, origin: VirtualScreenRect) -> Self {
        Self::new(
            self.left - origin.left,
            self.top - origin.top,
            self.width,
            self.height,
        )
    }
}

pub fn current_cursor_position() -> Option<(i32, i32)> {
    // macOS: the host also stores points in Quartz global coordinates.
    let point = CGEvent::new(source()?).ok()?.location();
    Some((point.x.round() as i32, point.y.round() as i32))
}

pub fn current_virtual_screen_rect() -> Option<VirtualScreenRect> {
    // macOS: include displays above and to the left of the main display.
    current_monitor_rects()?.into_iter().reduce(|a, b| {
        let left = a.left.min(b.left);
        let top = a.top.min(b.top);
        VirtualScreenRect::new(left, top, a.right().max(b.right()) - left,
            a.bottom().max(b.bottom()) - top)
    })
}

// macOS: CGDisplay::active_displays wraps CGGetActiveDisplayList.
pub fn current_monitor_rects() -> Option<Vec<VirtualScreenRect>> {
    let mut monitors: Vec<_> = CGDisplay::active_displays().ok()?.into_iter().map(|id| {
        let bounds = CGDisplay::new(id).bounds();
        VirtualScreenRect::new(bounds.origin.x as i32, bounds.origin.y as i32,
            bounds.size.width as i32, bounds.size.height as i32)
    }).filter(|r| r.width > 0 && r.height > 0).collect();
    if monitors.is_empty() {
        return None;
    }
    monitors.sort_by_key(|monitor: &VirtualScreenRect| (monitor.top, monitor.left));
    Some(monitors)
}

#[inline]
pub fn get_cursor_pos() -> (i32, i32) {
    current_cursor_position().unwrap_or((0, 0))
}

#[inline]
pub fn move_mouse(target_x: i32, target_y: i32) {
    // macOS: post the move in global points.
    post(&[make_movement(target_x, target_y)]);
}

#[inline]
fn move_mouse_with_rect(target_x: i32, target_y: i32, screen_rect: Option<VirtualScreenRect>) {
    // macOS: smoothing supplies global points as well.
    if screen_rect.is_some() {
        move_mouse(target_x, target_y);
    }
}

#[inline]
pub fn make_movement(end_x: i32, end_y: i32) -> INPUT {
    // macOS: retain the point for the MouseMoved event.
    INPUT::Move(CGPoint::new(end_x.into(), end_y.into()))
}

fn released_button_vk(flags: u32) -> Option<i32> {
    if flags & MOUSEEVENTF_LEFTUP != 0 {
        Some(VK_LBUTTON as i32)
    } else if flags & MOUSEEVENTF_RIGHTUP != 0 {
        Some(VK_RBUTTON as i32)
    } else if flags & MOUSEEVENTF_MIDDLEUP != 0 {
        Some(VK_MBUTTON as i32)
    } else {
        None
    }
}

fn note_released_button(flags: u32) {
    if let Some(vk) = released_button_vk(flags) {
        crate::hotkeys::note_injected_key(vk);
    }
}

#[inline]
pub fn make_input(flags: u32, time: u32) -> INPUT {
    // macOS: Quartz timestamps the posted button event.
    INPUT::Button(flags)
}

#[inline]
pub fn send_mouse_event(flags: u32) -> bool {
    let input = make_input(flags, 0);
    // macOS: count successfully constructed and posted events.
    let sent = post(&[input]);
    if sent == 1 {
        note_released_button(flags);
    }
    sent == 1
}

static PARTIAL_MOUSE_INSERT_WARNED: AtomicBool = AtomicBool::new(false);

pub fn send_batch(down: u32, up: u32, n: usize) -> bool {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(n * 2);
    for _ in 0..n {
        inputs.push(make_input(down, 0));
        inputs.push(make_input(up, 0));
    }
    let expected = inputs.len() as u32;
    // macOS: submit the down/up sequence in order.
    let sent = post(&inputs);
    let accepted = sent == expected;
    if sent > 0 {
        note_released_button(up);
    }
    if accepted {
        PARTIAL_MOUSE_INSERT_WARNED.store(false, Ordering::Relaxed);
    } else if !PARTIAL_MOUSE_INSERT_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("[engine] posted {sent} of {expected} mouse input events");
    }
    accepted
}

pub fn send_clicks(
    down: u32,
    up: u32,
    count: usize,
    plan: ClickCyclePlan,
    control: &RunControl,
    should_abort: &dyn Fn() -> bool,
) -> SendResult {
    let result = send_clicks_inner(down, up, count, plan, control, should_abort);
    if result.is_blocked() {
        let _ = send_mouse_event(up);
    }
    result
}

fn send_clicks_inner(
    down: u32,
    up: u32,
    count: usize,
    plan: ClickCyclePlan,
    control: &RunControl,
    should_abort: &dyn Fn() -> bool,
) -> SendResult {
    if count == 0 {
        return SendResult::Sent;
    }

    if should_abort() {
        return SendResult::Sent;
    }

    if plan.kind == ClickCycleKind::Single && plan.first_hold_ms == 0 {
        if count > 1 {
            return batch_result(send_batch(down, up, count));
        }
        if !control.is_active() {
            return SendResult::Sent;
        }
        return batch_result(send_batch(down, up, 1));
    }

    let blocked = std::cell::Cell::new(false);
    let is_active = || control.is_active() && !should_abort();
    let mut sleep_for = |duration| sleep_interruptible(duration, control, should_abort);

    for _ in 0..count {
        if should_abort() {
            break;
        }
        let mut press = || {
            if !send_mouse_event(down) {
                blocked.set(true);
            }
        };
        let mut release = || {
            if !send_mouse_event(up) {
                blocked.set(true);
            }
        };
        if !execute_click_cycle(plan, &mut press, &mut release, &mut sleep_for, &is_active) {
            break;
        }
    }

    if blocked.get() {
        SendResult::Blocked
    } else {
        SendResult::Sent
    }
}

#[inline]
fn batch_result(sent: bool) -> SendResult {
    if sent {
        SendResult::Sent
    } else {
        SendResult::Blocked
    }
}

#[inline]
pub fn get_button_flags(button: i32) -> (u32, u32) {
    match button {
        2 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        3 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
    }
}

#[inline]
pub fn ease_in_out_quad(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

#[inline]
pub fn cubic_bezier(t: f64, p0: f64, p1: f64, p2: f64, p3: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

struct SmoothMoveCtx<'a> {
    rng: &'a mut crate::engine::rng::SmallRng,
    allow_overshoot: bool,
    screen_rect: Option<VirtualScreenRect>,
}

fn smooth_move_inner(
    start_x: i32,
    start_y: i32,
    end_x: i32,
    end_y: i32,
    duration_ms: u64,
    ctx: &mut SmoothMoveCtx<'_>,
) {
    if duration_ms < 3 || (start_x == end_x && start_y == end_y) {
        move_mouse_with_rect(end_x, end_y, ctx.screen_rect);
        return;
    }

    let (start_x, start_y) = (start_x as f64, start_y as f64);
    let (target_x, target_y) = (end_x as f64, end_y as f64);
    let delta_x = target_x - start_x;
    let delta_y = target_y - start_y;
    let distance = delta_x.hypot(delta_y);

    if distance < 3.0 {
        move_mouse_with_rect(end_x, end_y, ctx.screen_rect);
        return;
    }

    let steps = if duration_ms <= 12 {
        (duration_ms / 3).clamp(1, 4) as usize
    } else {
        ((duration_ms / 8) as usize).clamp(4, 75)
    };

    let tick_duration = Duration::from_millis(duration_ms) / steps as u32;
    let start_time = Instant::now();

    let cp1_ratio = ctx.rng.next_f64() * 0.28 + 0.20;
    let cp2_ratio = ctx.rng.next_f64() * 0.24 + 0.55;

    let max_perp_offset = (distance * 0.29).min(76.0);

    let perp_x = -delta_y / distance;
    let perp_y = delta_x / distance;

    let offset_1 = (ctx.rng.next_f64() * 0.41 + 0.07)
        * max_perp_offset
        * (if ctx.rng.next_f64() >= 0.5 { 1.0 } else { -1.0 });
    let offset_2 = (ctx.rng.next_f64() * 0.41 + 0.07)
        * max_perp_offset
        * (if ctx.rng.next_f64() >= 0.5 { 1.0 } else { -1.0 });

    let control_1x = start_x + delta_x * cp1_ratio + perp_x * offset_1;
    let control_1y = start_y + delta_y * cp1_ratio + perp_y * offset_1;
    let control_2x = start_x + delta_x * cp2_ratio + perp_x * offset_2;
    let control_2y = start_y + delta_y * cp2_ratio + perp_y * offset_2;

    let mid_wobble = ctx.rng.next_f64() < 0.37 && duration_ms > 22;
    let wobble_step = if mid_wobble { steps / 2 } else { 0 };

    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let ease = ease_in_out_quad(t);

        let mut current_x = cubic_bezier(ease, start_x, control_1x, control_2x, target_x);
        let mut current_y = cubic_bezier(ease, start_y, control_1y, control_2y, target_y);

        if mid_wobble && i == wobble_step {
            let wobble = ctx.rng.next_f64() * 1.7 + 0.7;
            let sign = if ctx.rng.next_f64() >= 0.5 { 1.0 } else { -1.0 };
            current_x += perp_x * wobble * sign;
            current_y += perp_y * wobble * sign;
        }

        move_mouse_with_rect(current_x as i32, current_y as i32, ctx.screen_rect);

        if i < steps {
            let elapsed = start_time.elapsed();
            let expected = tick_duration * (i + 1) as u32;

            if expected > elapsed {
                std::thread::sleep(expected - elapsed);
            }
        }
    }

    if ctx.allow_overshoot && duration_ms > 16 && ctx.rng.next_f64() < 0.47 {
        let overshoot_amount = ctx.rng.next_f64() * 6.3 + 2.2;
        let dir_x = delta_x / distance;
        let dir_y = delta_y / distance;

        let over_x = (target_x + dir_x * overshoot_amount) as i32;
        let over_y = (target_y + dir_y * overshoot_amount) as i32;

        let correction_ms = (duration_ms as f64 * 0.19).max(4.0) as u64;

        ctx.allow_overshoot = false;
        smooth_move_inner(end_x, end_y, over_x, over_y, correction_ms, ctx);
        smooth_move_inner(
            over_x,
            over_y,
            end_x,
            end_y,
            (correction_ms * 2 / 3).max(3),
            ctx,
        );
    }
}

pub fn smooth_move(
    start_x: i32,
    start_y: i32,
    end_x: i32,
    end_y: i32,
    duration_ms: u64,
    rng: &mut crate::engine::rng::SmallRng,
) {
    let screen_rect = current_virtual_screen_rect();
    let mut ctx = SmoothMoveCtx {
        rng,
        allow_overshoot: true,
        screen_rect,
    };
    smooth_move_inner(start_x, start_y, end_x, end_y, duration_ms, &mut ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_button_vk_maps_up_flags_only() {
        // macOS: use the same flag and VK values from this backend.
        use super::{
            MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
            MOUSEEVENTF_RIGHTUP, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
        };

        assert_eq!(
            released_button_vk(MOUSEEVENTF_LEFTUP),
            Some(VK_LBUTTON as i32)
        );
        assert_eq!(
            released_button_vk(MOUSEEVENTF_RIGHTUP),
            Some(VK_RBUTTON as i32)
        );
        assert_eq!(
            released_button_vk(MOUSEEVENTF_MIDDLEUP),
            Some(VK_MBUTTON as i32)
        );
        assert_eq!(released_button_vk(MOUSEEVENTF_LEFTDOWN), None);
        assert_eq!(released_button_vk(MOUSEEVENTF_MOVE), None);
    }

    #[test]
    fn contains_includes_left_and_top_edges_but_excludes_right_and_bottom() {
        let rects = [
            VirtualScreenRect::new(0, 0, 1920, 1080),
            VirtualScreenRect::new(-1920, -200, 1280, 1024),
        ];

        for rect in rects {
            assert!(rect.contains(rect.left, rect.top));
            assert!(rect.contains(rect.left + 1, rect.top + 1));
            assert!(rect.contains(rect.right() - 1, rect.top));
            assert!(rect.contains(rect.left, rect.bottom() - 1));
            assert!(!rect.contains(rect.right(), rect.top));
            assert!(!rect.contains(rect.left, rect.bottom()));
            assert!(!rect.contains(rect.right(), rect.bottom()));
            assert!(!rect.contains(rect.left - 1, rect.top));
            assert!(!rect.contains(rect.left, rect.top - 1));
        }
    }

    #[test]
    fn button_flags_map_right_and_middle_and_default_to_left() {
        assert_eq!(
            get_button_flags(1),
            (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        );
        assert_eq!(
            get_button_flags(2),
            (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
        );
        assert_eq!(
            get_button_flags(3),
            (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP)
        );
        assert_eq!(
            get_button_flags(0),
            (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        );
        assert_eq!(
            get_button_flags(99),
            (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        );
    }
}
