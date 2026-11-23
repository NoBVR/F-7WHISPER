// Changed for macOS by F-7WHISPER on 2026-11-22, from Blur AutoClicker v3.10.2's
// src-tauri/src/engine/keyboard.rs (GPL-3.0, see LICENSE-BLUR-GPL-3.0). Diff it against
// ../engine/keyboard.rs to see every change.

use std::sync::atomic::{AtomicBool, Ordering};

use super::cycle::{execute_click_cycle, ClickCycleKind, ClickCyclePlan};
use super::worker::{sleep_interruptible, RunControl};
use super::SendResult;
use super::AUTOCLICKER_EXTRA_INFO;
// macOS: translate Blur's Windows VK at the Quartz boundary.
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use std::{cell::Cell, sync::Mutex};

const VK_SHIFT: u16 = 0x10;
const KEYEVENTF_KEYUP: u32 = 2;

pub struct INPUT {
    code: Option<u16>,
    down: bool,
}

fn keycode(vk: u16) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46,
        45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const FUNCTION: [u16; 20] = [
        122, 120, 99, 118, 96, 97, 98, 100, 101, 109,
        103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
    ];
    // Keypad digits, multiply, plus, separator (JIS comma), minus, decimal, divide.
    const KEYPAD: [u16; 16] = [82, 83, 84, 85, 86, 87, 88, 89, 91, 92, 67, 69, 95, 78, 65, 75];
    Some(match vk {
        0x41..=0x5a => LETTERS[(vk - 0x41) as usize],
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x70..=0x83 => FUNCTION[(vk - 0x70) as usize],
        0x60..=0x6f => KEYPAD[(vk - 0x60) as usize],
        0x25 => 123, 0x27 => 124, 0x28 => 125, 0x26 => 126,
        0x20 => 49, 0x0d => 36, 0x09 => 48, 0x1b => 53,
        0x08 => 51, 0x2e => 117, 0x2d => 114,
        0x24 => 115, 0x23 => 119, 0x21 => 116, 0x22 => 121,
        0x5b => 55, 0x5c => 54, 0x5d => 110,
        0x10 | 0xa0 => 56, 0xa1 => 60, 0x11 | 0xa2 => 59, 0xa3 => 62,
        0x12 | 0xa4 => 58, 0xa5 => 61, 0x14 => 57, 0x90 => 71,
        0xba => 41, 0xbb => 24, 0xbc => 43, 0xbd => 27, 0xbe => 47,
        0xbf => 44, 0xc0 => 50, 0xdb => 33, 0xdc => 42, 0xdd => 30,
        0xde => 39, 0xe2 => 10,
        _ => return None,
    })
}

// macOS: combine the HID flags with modifiers held by concurrent Blur rows.
const MODIFIERS: [(u16, CGEventFlags, u64); 9] = [
    (56, CGEventFlags::CGEventFlagShift, 0x0002),
    (60, CGEventFlags::CGEventFlagShift, 0x0004),
    (59, CGEventFlags::CGEventFlagControl, 0x0001),
    (62, CGEventFlags::CGEventFlagControl, 0x2000),
    (58, CGEventFlags::CGEventFlagAlternate, 0x0020),
    (61, CGEventFlags::CGEventFlagAlternate, 0x0040),
    (55, CGEventFlags::CGEventFlagCommand, 0x0008),
    (54, CGEventFlags::CGEventFlagCommand, 0x0010),
    (57, CGEventFlags::CGEventFlagAlphaShift, 0),
];

static HELD: Mutex<[usize; MODIFIERS.len()]> = Mutex::new([0; MODIFIERS.len()]);
thread_local! { static ROW_MODIFIERS: Cell<u16> = const { Cell::new(0) }; }

fn post(inputs: &[INPUT]) -> u32 {
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else { return 0 };
    let mut held = HELD.lock().unwrap_or_else(crate::error::poisoned_inner);
    let mut sent = 0;
    for input in inputs {
        let Some(code) = input.code else { break };
        let Ok(event) = CGEvent::new_keyboard_event(source.clone(), code, input.down) else { break };
        let mut bits = event.get_flags().bits();
        if let Some(i) = MODIFIERS.iter().position(|m| m.0 == code) {
            let (_, flag, side) = MODIFIERS[i];
            ROW_MODIFIERS.with(|owned| {
                let mask = 1 << i;
                if input.down && owned.get() & mask == 0 {
                    held[i] += 1;
                    owned.set(owned.get() | mask);
                } else if !input.down && owned.get() & mask != 0 {
                    held[i] -= 1;
                    owned.set(owned.get() & !mask);
                }
            });
            bits &= !(flag.bits() | side);
            for &(_, other, other_side) in &MODIFIERS {
                if other == flag && bits & other_side != 0 {
                    bits |= flag.bits();
                }
            }
            event.set_type(CGEventType::FlagsChanged);
        }
        for (i, &(_, flag, side)) in MODIFIERS.iter().enumerate() {
            if held[i] != 0 {
                bits |= flag.bits() | side;
            }
        }
        event.set_flags(CGEventFlags::from_bits_retain(bits));
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, AUTOCLICKER_EXTRA_INFO as i64);
        event.post(CGEventTapLocation::HID);
        sent += 1;
    }
    sent
}

#[inline]
pub fn make_keyboard_input(vk: u16, flags: u32) -> INPUT {
    // macOS: an unmapped VK stops the batch at this input.
    INPUT { code: keycode(vk), down: flags & KEYEVENTF_KEYUP == 0 }
}

#[inline]
pub fn send_key_event(vk: u16, flags: u32) -> bool {
    let input = make_keyboard_input(vk, flags);
    // macOS: post at the HID tap.
    let sent = post(&[input]);
    if sent == 1 && flags & KEYEVENTF_KEYUP != 0 {
        crate::hotkeys::note_injected_key(vk as i32);
    }
    sent == 1
}

pub fn is_alphabetic_vk(vk: u16) -> bool {
    (b'A' as u16..=b'Z' as u16).contains(&vk)
}

fn caps_lock_enabled() -> bool {
    // macOS: AlphaShift tracks Caps Lock.
    super::hid_flags().contains(CGEventFlags::CGEventFlagAlphaShift)
}

fn should_hold_shift_for_case(vk: u16, uppercase: bool) -> bool {
    shift_needed_for_case(vk, uppercase, caps_lock_enabled())
}

fn shift_needed_for_case(vk: u16, uppercase: bool, caps_lock_on: bool) -> bool {
    is_alphabetic_vk(vk) && (caps_lock_on != uppercase)
}

fn push_key_press(inputs: &mut Vec<INPUT>, vk: u16, use_shift: bool) {
    if use_shift {
        inputs.push(make_keyboard_input(VK_SHIFT, 0));
    }

    inputs.push(make_keyboard_input(vk, 0));
    inputs.push(make_keyboard_input(vk, KEYEVENTF_KEYUP));

    if use_shift {
        inputs.push(make_keyboard_input(VK_SHIFT, KEYEVENTF_KEYUP));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyDown {
    pub use_shift: bool,
    pub shift_down: bool,
    pub key_down: bool,
}

impl KeyDown {
    pub(crate) fn accepted(self) -> bool {
        self.key_down
    }

    pub(crate) fn held(self) -> bool {
        self.shift_down || self.key_down
    }

    pub(crate) fn latch(&mut self, press: KeyDown) {
        if press.shift_down {
            self.use_shift = press.use_shift;
        }
        self.shift_down |= press.shift_down;
        self.key_down |= press.key_down;
    }
}

pub(crate) fn send_key_down(vk: u16, uppercase: bool) -> KeyDown {
    let use_shift = should_hold_shift_for_case(vk, uppercase);
    if use_shift && !send_key_event(VK_SHIFT, 0) {
        return KeyDown {
            use_shift,
            shift_down: false,
            key_down: false,
        };
    }
    KeyDown {
        use_shift,
        shift_down: use_shift,
        key_down: send_key_event(vk, 0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyUpPlan {
    key: bool,
    shift: bool,
}

fn key_up_plan(down: KeyDown) -> KeyUpPlan {
    KeyUpPlan {
        key: down.key_down,
        shift: down.use_shift && down.shift_down,
    }
}

pub(crate) fn send_key_up(vk: u16, down: KeyDown) -> bool {
    let plan = key_up_plan(down);
    let key_ok = !plan.key || send_key_event(vk, KEYEVENTF_KEYUP);
    let shift_ok = !plan.shift || send_key_event(VK_SHIFT, KEYEVENTF_KEYUP);
    key_ok && shift_ok
}

pub(crate) fn after_release(down: KeyDown, released: bool) -> KeyDown {
    if released {
        KeyDown::default()
    } else {
        down
    }
}

static PARTIAL_KEYBOARD_INSERT_WARNED: AtomicBool = AtomicBool::new(false);

pub fn send_key_batch(vk: u16, n: usize, uppercase: bool) -> bool {
    let use_shift = should_hold_shift_for_case(vk, uppercase);
    let inputs_per_press = if use_shift { 4 } else { 2 };
    let mut inputs: Vec<INPUT> = Vec::with_capacity(n * inputs_per_press);
    for _ in 0..n {
        push_key_press(&mut inputs, vk, use_shift);
    }
    debug_assert_eq!(inputs.len(), n * inputs_per_press);
    let expected = inputs.len() as u32;
    // macOS: serialize the batch with other Blur keyboard workers.
    let sent = post(&inputs);
    let accepted = sent == expected;
    if sent > 0 {
        crate::hotkeys::note_injected_key(vk as i32);
        if use_shift {
            crate::hotkeys::note_injected_key(VK_SHIFT as i32);
        }
    }
    if accepted {
        PARTIAL_KEYBOARD_INSERT_WARNED.store(false, Ordering::Relaxed);
    } else if !PARTIAL_KEYBOARD_INSERT_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("[engine] posted {sent} of {expected} keyboard input events");
    }
    accepted
}

fn batch_residual(vk: u16, uppercase: bool) -> KeyDown {
    let use_shift = should_hold_shift_for_case(vk, uppercase);
    KeyDown {
        use_shift,
        shift_down: use_shift,
        key_down: true,
    }
}

pub fn send_key_presses(
    vk: u16,
    count: usize,
    uppercase: bool,
    plan: ClickCyclePlan,
    control: &RunControl,
    should_abort: &dyn Fn() -> bool,
) -> SendResult {
    let (result, residual) =
        send_key_presses_inner(vk, count, uppercase, plan, control, should_abort);
    if result.is_blocked() && residual.held() {
        let _ = send_key_up(vk, residual);
    }
    result
}

fn send_key_presses_inner(
    vk: u16,
    count: usize,
    uppercase: bool,
    plan: ClickCyclePlan,
    control: &RunControl,
    should_abort: &dyn Fn() -> bool,
) -> (SendResult, KeyDown) {
    if count == 0 {
        return (SendResult::Sent, KeyDown::default());
    }

    if should_abort() {
        return (SendResult::Sent, KeyDown::default());
    }

    if plan.kind == ClickCycleKind::Single && plan.first_hold_ms == 0 {
        let residual = batch_residual(vk, uppercase);
        if count > 1 {
            return (batch_result(send_key_batch(vk, count, uppercase)), residual);
        }
        if !control.is_active() {
            return (SendResult::Sent, KeyDown::default());
        }
        return (batch_result(send_key_batch(vk, count, uppercase)), residual);
    }

    let blocked = std::cell::Cell::new(false);
    let down = std::cell::Cell::new(KeyDown::default());
    let is_active = || control.is_active() && !should_abort();
    let mut sleep_for = |duration| sleep_interruptible(duration, control, should_abort);

    for _ in 0..count {
        if should_abort() {
            break;
        }
        let mut press = || {
            let press = send_key_down(vk, uppercase);
            let mut held = down.get();
            held.latch(press);
            down.set(held);
            if !press.accepted() {
                blocked.set(true);
            }
        };
        let mut release = || {
            let held = down.get();
            if !held.held() {
                return;
            }
            let released = send_key_up(vk, held);
            down.set(after_release(held, released));
            if !released {
                blocked.set(true);
            }
        };
        if !execute_click_cycle(plan, &mut press, &mut release, &mut sleep_for, &is_active) {
            break;
        }
    }

    let result = if blocked.get() {
        SendResult::Blocked
    } else {
        SendResult::Sent
    };
    (result, down.get())
}

#[inline]
fn batch_result(sent: bool) -> SendResult {
    if sent {
        SendResult::Sent
    } else {
        SendResult::Blocked
    }
}

#[cfg(test)]
mod tests {
    use super::{after_release, key_up_plan, shift_needed_for_case, KeyDown, KeyUpPlan};

    fn down(use_shift: bool, shift_down: bool, key_down: bool) -> KeyDown {
        KeyDown {
            use_shift,
            shift_down,
            key_down,
        }
    }

    #[test]
    fn a_partial_press_still_releases_the_shift_it_pressed() {
        let partial = down(true, true, false);

        assert!(!partial.accepted());
        assert!(partial.held());
        assert_eq!(
            key_up_plan(partial),
            KeyUpPlan {
                key: false,
                shift: true
            }
        );
    }

    #[test]
    fn a_rejected_shift_press_leaves_nothing_to_release() {
        let rejected = down(true, false, false);

        assert!(!rejected.held());
        assert_eq!(
            key_up_plan(rejected),
            KeyUpPlan {
                key: false,
                shift: false
            }
        );
    }

    #[test]
    fn a_failed_repeat_press_keeps_the_earlier_press_releasable() {
        let mut held = KeyDown::default();
        held.latch(down(true, true, true));
        held.latch(down(true, false, false));

        assert_eq!(
            key_up_plan(held),
            KeyUpPlan {
                key: true,
                shift: true
            }
        );
    }

    #[test]
    fn a_failed_release_keeps_the_key_latched_for_a_retry() {
        let held = down(true, true, true);

        assert_eq!(after_release(held, false), held);
        assert_eq!(after_release(held, true), KeyDown::default());
    }

    #[test]
    fn a_blocked_batch_reports_the_press_it_must_release() {
        let residual = super::batch_residual(b'A' as u16, true);

        assert!(residual.held());
        assert!(key_up_plan(residual).key);
    }

    #[test]
    fn release_uses_the_shift_state_captured_at_press_time() {
        let vk = b'A' as u16;
        let mut held = KeyDown::default();
        held.latch(down(shift_needed_for_case(vk, true, false), true, true));

        assert!(!shift_needed_for_case(vk, true, true));
        assert_eq!(
            key_up_plan(held),
            KeyUpPlan {
                key: true,
                shift: true
            }
        );
    }
}
