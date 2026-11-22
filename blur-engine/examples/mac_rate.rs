#[cfg(target_os = "macos")]
fn main() {
    use blur_engine::{BlurButton, BlurInput, BlurRun};
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
    use core_graphics::event::{
        CallbackResult, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventType::{KeyDown, LeftMouseDown}, EventField,
    };
    use std::{cell::Cell, thread, time::{Duration, Instant}};

    let keys = Cell::new(0_u64);
    let buttons = Cell::new(0_u64);
    let measure = |listen: bool| {
        let wait = |duration: Duration| {
            if listen {
                CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, duration, false);
            } else {
                thread::sleep(duration);
            }
        };
        for (name, input) in [
            ("F20", BlurInput::Key { vk: 0x83 }),
            ("Left", BlurInput::Click { button: BlurButton::Left, at: None }),
        ] {
            let start = Instant::now();
            let run = BlurRun::start(input, 1.0).expect("start Blur worker");
            wait(Duration::from_secs(3));
            let clicks = run.clicks();
            let elapsed = start.elapsed().as_secs_f64();
            run.stop();
            println!("{name}: {clicks} clicks, {:.1}/s", clicks as f64 / elapsed);
        }
        if listen {
            wait(Duration::from_millis(100));
        }
    };
    let tap = CGEventTap::with_enabled(
        CGEventTapLocation::Session,
        CGEventTapPlacement::TailAppendEventTap,
        CGEventTapOptions::ListenOnly,
        vec![KeyDown, LeftMouseDown],
        |_, kind, event| {
            let counter = match kind {
                KeyDown => &keys,
                LeftMouseDown => &buttons,
                _ => return CallbackResult::Keep,
            };
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == 0x800D_A5A5 {
                counter.set(counter.get() + 1);
            }
            CallbackResult::Keep
        },
        || measure(true),
    );
    if tap.is_ok() {
        println!("tagged keyDown={}, leftMouseDown={}", keys.get(), buttons.get());
    } else {
        println!("tap unavailable");
        measure(false);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
