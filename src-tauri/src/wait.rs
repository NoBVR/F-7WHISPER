use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::Instant,
};

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        Media::{timeBeginPeriod, timeEndPeriod},
        System::Threading::{
            CreateEventW, CreateWaitableTimerExW, SetEvent, SetWaitableTimer,
            WaitForMultipleObjects, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
            TIMER_ALL_ACCESS,
        },
    },
};

#[cfg(windows)]
struct Handle(HANDLE);

// These handles refer only to kernel synchronization objects. Drop owns the close.
#[cfg(windows)]
unsafe impl Send for Handle {}
#[cfg(windows)]
unsafe impl Sync for Handle {}

#[cfg(windows)]
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.0); }
    }
}

pub struct Wake {
    changed: Condvar,
    #[cfg(windows)]
    event: Option<Handle>,
}

impl Wake {
    pub fn new() -> Self {
        Self {
            changed: Condvar::new(),
            #[cfg(windows)]
            event: unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.ok().map(Handle),
        }
    }

    pub fn notify(&self) {
        #[cfg(windows)]
        if let Some(event) = &self.event {
            unsafe { let _ = SetEvent(event.0); }
        }
        self.changed.notify_one();
    }
}

pub struct Timer {
    #[cfg(windows)]
    high_resolution: Option<Handle>,
    #[cfg(windows)]
    period_set: bool,
}

impl Timer {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            high_resolution: unsafe {
                CreateWaitableTimerExW(
                    None,
                    PCWSTR::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )
            }.ok().map(Handle),
            #[cfg(windows)]
            period_set: false,
        }
    }

    pub fn active(&mut self, active: bool) {
        #[cfg(windows)]
        if active && !self.period_set {
            self.period_set = unsafe { timeBeginPeriod(1) } == 0;
        } else if !active && self.period_set {
            unsafe { timeEndPeriod(1); }
            self.period_set = false;
        }
        #[cfg(not(windows))]
        let _ = active;
    }

    pub fn wait<'a, T>(
        &mut self,
        wake: &Wake,
        _mutex: &'a Mutex<T>,
        guard: MutexGuard<'a, T>,
        due: Option<Instant>,
    ) -> MutexGuard<'a, T> {
        let Some(due) = due else {
            return wake.changed.wait(guard).unwrap();
        };

        let remaining = due.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return guard;
        }

        #[cfg(windows)]
        if let (Some(timer), Some(event)) = (&self.high_resolution, &wake.event) {
            let ticks = remaining.as_nanos().div_ceil(100).min(i64::MAX as u128) as i64;
            let relative = -ticks.max(1);
            if unsafe { SetWaitableTimer(timer.0, &relative, 0, None, None, false) }.is_ok() {
                let handles = [event.0, timer.0];
                drop(guard);
                if unsafe { WaitForMultipleObjects(&handles, false, INFINITE) } == WAIT_FAILED {
                    self.high_resolution = None;
                }
                return _mutex.lock().unwrap();
            }
            self.high_resolution = None;
        }

        wake.changed.wait_timeout(guard, remaining).unwrap().0
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.active(false);
    }
}
