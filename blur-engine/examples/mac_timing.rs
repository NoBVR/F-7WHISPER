#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    use std::hint::black_box;
    use std::thread;
    use std::time::{Duration, Instant};

    const F20: u16 = 90;

    fn report(label: &str, samples: &mut [Duration], extra: &str) {
        samples.sort_unstable();
        let n = samples.len();
        let us = |d: Duration| d.as_secs_f64() * 1_000_000.0;
        let percentile = |p: usize| us(samples[(n * p).div_ceil(100) - 1]);
        println!(
            "{label} n={n} mean_us={:.3} p50_us={:.3} p99_us={:.3} max_us={:.3}{extra}",
            us(samples.iter().sum()) / n as f64,
            percentile(50),
            percentile(99),
            us(samples[n - 1]),
        );
    }

    fn measure<T>(label: &str, n: usize, mut call: impl FnMut() -> T) {
        let mut samples = Vec::with_capacity(n);
        for _ in 0..n {
            let start = Instant::now();
            let result = call();
            samples.push(start.elapsed());
            black_box(result);
        }
        report(label, &mut samples, "");
    }

    fn sleeps(mode: &str) {
        for ms in [1, 3] {
            let delay = Duration::from_millis(ms);
            measure(&format!("{mode} sleep({ms}ms)"), 500, || {
                thread::sleep(delay)
            });
        }
    }

    fn source() -> CGEventSource {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("create HID source")
    }

    fn key(src: &CGEventSource, down: bool) -> CGEvent {
        CGEvent::new_keyboard_event(src.clone(), F20, down).expect("create F20 event")
    }

    fn post_pair(pair: &[CGEvent; 2]) {
        for event in pair {
            event.post(CGEventTapLocation::HID);
        }
    }

    fn post_fresh_pair(src: &CGEventSource) {
        post_pair(&[key(src, true), key(src, false)]);
    }

    fn event_costs() {
        const N: usize = 3000;
        measure("create HID source", N, source);
        let src = source();
        let cursor = || {
            CGEvent::new(src.clone())
                .expect("create cursor event")
                .location()
        };
        measure("create event + read cursor", N, cursor);
        measure("create F20 down", N, || key(&src, true));

        let pair = [key(&src, true), key(&src, false)];
        measure("post prebuilt F20 pair", N, || post_pair(&pair));
        measure("build + post F20 pair", N, || post_fresh_pair(&src));

        let point = cursor();
        let pair = [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp].map(|kind| {
            CGEvent::new_mouse_event(src.clone(), kind, point, CGMouseButton::Left)
                .expect("create left button event")
        });
        measure("post prebuilt left mouse pair", N, || post_pair(&pair));
    }

    fn batches(mode: &str) {
        const ROUNDS: usize = 1000;
        const PAIRS: usize = 3;
        let period = Duration::from_millis(3);
        let mut samples = Vec::with_capacity(ROUNDS);
        let mut overruns = 0;
        let start = Instant::now();
        let mut deadline = start;
        for _ in 0..ROUNDS {
            let round_start = Instant::now();
            deadline += period;
            // Match run_batch's catch-up limit before starting the posts.
            if round_start.saturating_duration_since(deadline) > period {
                deadline = round_start;
            }
            {
                let src = source();
                for _ in 0..PAIRS {
                    post_fresh_pair(&src);
                }
            }
            let posted = Instant::now();
            if posted > deadline {
                overruns += 1;
            } else {
                thread::sleep(deadline - posted);
            }
            samples.push(round_start.elapsed());
        }
        let rate = (ROUNDS * PAIRS) as f64 / start.elapsed().as_secs_f64();
        report(
            &format!("{mode} 3 F20 pairs/3ms round"),
            &mut samples,
            &format!(" pairs/s={rate:.1} pre_sleep_overruns={overruns}/{ROUNDS}"),
        );
    }

    #[allow(deprecated)] // Use the Mach bindings supplied by the engine's libc dependency.
    fn set_time_constraint() -> libc::kern_return_t {
        extern "C" {
            fn mach_port_deallocate(
                task: libc::mach_port_t,
                name: libc::mach_port_t,
            ) -> libc::kern_return_t;
        }
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        assert_eq!(
            unsafe { libc::mach_timebase_info(&mut tb) },
            libc::KERN_SUCCESS
        );
        let ticks =
            |ns: u64| u32::try_from(ns * u64::from(tb.denom) / u64::from(tb.numer)).unwrap();
        let mut policy = libc::thread_time_constraint_policy_data_t {
            period: ticks(1_000_000),
            computation: ticks(500_000),
            constraint: ticks(1_000_000),
            preemptible: 1,
        };
        unsafe {
            let port = libc::mach_thread_self();
            let kr = libc::thread_policy_set(
                port,
                libc::THREAD_TIME_CONSTRAINT_POLICY as _,
                (&raw mut policy).cast(),
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            );
            mach_port_deallocate(libc::mach_task_self(), port);
            kr
        }
    }

    pub fn run() {
        thread::spawn(|| sleeps("plain")).join().unwrap();
        thread::spawn(|| {
            let info = NSProcessInfo::processInfo();
            let activity = info.beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiated | NSActivityOptions::LatencyCritical,
                &NSString::from_str("Running a Blur input row"),
            );
            let qos = unsafe {
                libc::pthread_set_qos_class_self_np(
                    libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                    0,
                )
            };
            let mode = format!("worker(qos_rc={qos})");
            sleeps(&mode);
            batches(&mode);
            unsafe { info.endActivity(&activity) };
        })
        .join()
        .unwrap();
        thread::spawn(|| {
            let kr = set_time_constraint();
            let mode = format!("mach(policy_set_ok={},kr={kr})", kr == libc::KERN_SUCCESS);
            sleeps(&mode);
            batches(&mode);
        })
        .join()
        .unwrap();
        event_costs();
    }
}
