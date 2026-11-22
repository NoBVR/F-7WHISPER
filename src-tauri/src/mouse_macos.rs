use super::{modifiers, Tracker};
use crate::{desktop, keys};
use core_foundation::{
    base::{kCFAllocatorDefault, TCFType},
    mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef},
    runloop::{
        kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext,
        CFRunLoopSourceCreate, CFRunLoopSourceSignal, CFRunLoopWakeUp,
    },
};
use core_graphics::{
    event::{
        CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventTapProxy, CGEventType, EventField,
    },
    sys::CGEventRef,
};
use std::{cell::Cell, ffi::c_void, ptr, sync::mpsc::Sender, thread::JoinHandle};

type TapCallback =
    unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

// Use the raw callback so a tap-disabled notification can be handled before
// interpreting its event pointer. Quartz owns the incoming event.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
}

struct Context {
    tracker: Tracker,
    port: Cell<CFMachPortRef>,
    pid: i64,
}

unsafe extern "C" fn callback(
    _: CGEventTapProxy,
    kind: u32,
    event: CGEventRef,
    info: *mut c_void,
) -> CGEventRef {
    let context = &*info.cast::<Context>();

    if kind == CGEventType::TapDisabledByTimeout as u32
        || kind == CGEventType::TapDisabledByUserInput as u32
    {
        CGEventTapEnable(context.port.get(), true);
        // A physical release may have occurred during the interruption.
        for side in 0..2 {
            if context.tracker.held[side].get() != 0 && !desktop::button_down(side as u32 + 3) {
                context.tracker.edge(side, 0, false);
            }
        }
        return event;
    }

    let down = kind == CGEventType::OtherMouseDown as u32;
    if (!down && kind != CGEventType::OtherMouseUp as u32) || event.is_null() {
        return event;
    }
    if CGEventGetIntegerValueField(event, EventField::EVENT_SOURCE_UNIX_PROCESS_ID) == context.pid {
        return event;
    }
    let side = match CGEventGetIntegerValueField(event, EventField::MOUSE_EVENT_BUTTON_NUMBER) {
        3 => 0,
        4 => 1,
        _ => return event,
    };
    let mods = if down {
        let flags = CGEventFlags::from_bits_retain(CGEventGetFlags(event));
        modifiers(
            flags.contains(CGEventFlags::CGEventFlagControl),
            flags.contains(CGEventFlags::CGEventFlagAlternate),
            flags.contains(CGEventFlags::CGEventFlagShift),
            flags.contains(CGEventFlags::CGEventFlagCommand),
        )
    } else {
        0
    };
    if context.tracker.edge(side, mods, down) {
        ptr::null_mut()
    } else {
        event
    }
}

struct Tap(CFMachPort);

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe { CFMachPortInvalidate(self.0.as_concrete_TypeRef()) };
    }
}

// CFRunLoopSourceSignal and CFRunLoopWakeUp support cross-thread signaling.
// This handle owns references to both objects. The worker owns the callback.
pub(super) struct Stop {
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
}

unsafe impl Send for Stop {}

impl Stop {
    pub(super) fn signal(&self, _: &JoinHandle<()>) {
        unsafe {
            CFRunLoopSourceSignal(self.source.as_concrete_TypeRef());
            CFRunLoopWakeUp(self.run_loop.as_concrete_TypeRef());
        }
    }
}

extern "C" fn stop_current(_: *mut c_void) {
    CFRunLoop::get_current().stop();
}

fn stop_source() -> Option<CFRunLoopSource> {
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: stop_current,
    };
    unsafe {
        let raw = CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut context);
        if raw.is_null() {
            return None;
        }
        Some(CFRunLoopSource::wrap_under_create_rule(raw))
    }
}

pub(super) fn run(tracker: Tracker, ready: Sender<Stop>) {
    if !keys::trusted() {
        return;
    }
    let Some(quit) = stop_source() else { return };
    let context = Context {
        tracker,
        port: Cell::new(ptr::null_mut()),
        pid: i64::from(std::process::id()),
    };
    let mask = (1u64 << CGEventType::OtherMouseDown as u32)
        | (1u64 << CGEventType::OtherMouseUp as u32);

    // The context stays at this address until after the source is removed
    // and the tap is invalidated. Only this thread services the tap.
    let raw = unsafe {
        CGEventTapCreate(
            CGEventTapLocation::Session as u32,
            CGEventTapPlacement::HeadInsertEventTap as u32,
            CGEventTapOptions::Default as u32,
            mask,
            callback,
            ptr::from_ref(&context).cast_mut().cast(),
        )
    };
    if raw.is_null() {
        return;
    }
    context.port.set(raw);
    let tap = Tap(unsafe { CFMachPort::wrap_under_create_rule(raw) });
    let Ok(source) = tap.0.create_runloop_source(0) else { return };

    let run_loop = CFRunLoop::get_current();
    let mode = unsafe { kCFRunLoopDefaultMode };
    run_loop.add_source(&source, mode);
    run_loop.add_source(&quit, mode);
    unsafe { CGEventTapEnable(raw, true) };

    // A signaled source stays pending even when shutdown races startup.
    if ready.send(Stop { source: quit.clone(), run_loop: run_loop.clone() }).is_ok() {
        CFRunLoop::run_current();
    }
    run_loop.remove_source(&quit, mode);
    run_loop.remove_source(&source, mode);
}
