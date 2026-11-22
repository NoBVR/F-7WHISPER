use crate::{
    keys,
    settings::{Button, Point},
};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_graphics::{
    event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::{CGPoint, CGRect},
    window::{
        kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly, kCGWindowOwnerPID, CGWindowListCopyWindowInfo,
    },
};
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use std::collections::BTreeSet;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // CGEventSourceStateID is int32_t and CGMouseButton is uint32_t.
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
}

pub fn foreground_pid() -> Option<u32> {
    autoreleasepool(|_| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        u32::try_from(app.processIdentifier()).ok().filter(|id| *id != 0)
    })
}

pub fn process_name(id: u32) -> Option<String> {
    autoreleasepool(|_| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(id.try_into().ok()?)?;
        Some(app.localizedName()?.to_string())
    })
}

type WindowInfo = CFDictionary<CFString, CFType>;

fn windows_on_screen() -> Option<CFArray<WindowInfo>> {
    unsafe {
        let array = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        );
        if array.is_null() {
            return None;
        }
        // The Copy result owns an array of window description dictionaries.
        Some(CFArray::wrap_under_create_rule(array))
    }
}

fn number(info: &WindowInfo, key: CFStringRef) -> Option<i32> {
    info.find(key)?.downcast::<CFNumber>()?.to_i32()
}

pub fn apps() -> Vec<String> {
    let Some(windows) = windows_on_screen() else { return Vec::new() };
    let mut ids = BTreeSet::new();
    for info in windows.iter() {
        if number(&info, unsafe { kCGWindowLayer }) != Some(0) {
            continue;
        }
        if let Some(id) = number(&info, unsafe { kCGWindowOwnerPID }) {
            if id > 0 && id as u32 != std::process::id() {
                ids.insert(id);
            }
        }
    }
    autoreleasepool(|_| {
        ids.into_iter().filter_map(|id| {
            let app = NSRunningApplication::runningApplicationWithProcessIdentifier(id)?;
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
                return None;
            }
            Some(app.localizedName()?.to_string())
        }).collect()
    })
}

pub fn left_down() -> bool {
    unsafe {
        CGEventSourceButtonState(
            CGEventSourceStateID::HIDSystemState as i32,
            CGMouseButton::Left as u32,
        )
    }
}

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

fn cursor() -> Option<CGPoint> {
    Some(CGEvent::new(source()?).ok()?.location())
}

pub fn outside_point() -> Option<Point> {
    let p = cursor()?;
    let windows = windows_on_screen()?;
    for info in windows.iter() {
        if number(&info, unsafe { kCGWindowOwnerPID }) != Some(std::process::id() as i32) {
            continue;
        }
        let bounds = info.find(unsafe { kCGWindowBounds })?.downcast::<CFDictionary>()?;
        if CGRect::from_dict_representation(&bounds)?.contains(&p) {
            return None;
        }
    }
    Some(Point { x: p.x.round() as i32, y: p.y.round() as i32 })
}

pub fn click(button: Button, at: Option<Point>, down: bool) -> bool {
    if !keys::trusted() {
        return false;
    }
    let Some(source) = source() else { return false };
    let fixed = at.filter(|_| down);
    let p = match fixed {
        Some(p) => CGPoint::new(f64::from(p.x), f64::from(p.y)),
        None => {
            let Ok(event) = CGEvent::new(source.clone()) else { return false };
            event.location()
        }
    };
    let (native_button, press, release) = match button {
        Button::Left => (CGMouseButton::Left, CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
        Button::Right => (CGMouseButton::Right, CGEventType::RightMouseDown, CGEventType::RightMouseUp),
        Button::Middle => (CGMouseButton::Center, CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
    };
    let kind = if down { press } else { release };
    let Ok(event) = CGEvent::new_mouse_event(source.clone(), kind, p, native_button) else {
        return false;
    };
    if fixed.is_some() {
        let Ok(movement) = CGEvent::new_mouse_event(source, CGEventType::MouseMoved, p, native_button)
        else { return false };
        movement.post(CGEventTapLocation::HID);
    }
    event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
    event.post(CGEventTapLocation::HID);
    true
}
