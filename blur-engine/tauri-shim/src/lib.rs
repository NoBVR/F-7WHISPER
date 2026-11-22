//! Stand-in for the handful of `tauri` items that Blur's engine files use.
//!
//! F-7WHISPER runs Blur's engine without Tauri, so this crate is linked under
//! the name `tauri` (see blur-engine/Cargo.toml). It only provides what the
//! copied files and the blur-engine crate root actually touch:
//! `AppHandle`, `Manager::state`, `State`, `Emitter::emit`,
//! `AppHandle::run_on_main_thread`, `Error` and `Result`.
//!
//! Each `AppHandle` owns its own type-keyed managed-state map, so every F-7
//! run can hold its own `ClickerState`. There is no main thread to marshal to:
//! `run_on_main_thread` runs the closure inline on the calling thread.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, RwLock};

/// The subset of `tauri::Error` that this stand-in can produce.
/// `InvokeKey` exists because Blur's own error tests construct it.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    InvokeKey,
    /// The emitted payload could not be serialized.
    EventSerialize(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvokeKey => f.write_str("invoke key mismatch"),
            Error::EventSerialize(msg) => write!(f, "failed to serialize event payload: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

type EmitListener = Box<dyn Fn(&str, &serde_json::Value) + Send + Sync + 'static>;

struct Inner {
    states: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    emit_listener: RwLock<Option<EmitListener>>,
}

/// A cheap, cloneable handle. Clones share one state map and one emit listener.
#[derive(Clone)]
pub struct AppHandle {
    inner: Arc<Inner>,
}

impl Default for AppHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl AppHandle {
    /// A fresh handle with no managed state and no emit listener.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                states: RwLock::new(HashMap::new()),
                emit_listener: RwLock::new(None),
            }),
        }
    }

    /// Runs `f` inline on the calling thread (there is no UI main thread here).
    pub fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        f();
        Ok(())
    }

    /// Installs the callback that receives every event this handle emits,
    /// with its payload serialized to a `serde_json::Value`. It runs inline on
    /// the emitting thread and must not call back into `set_emit_listener`.
    pub fn set_emit_listener<F>(&self, listener: F)
    where
        F: Fn(&str, &serde_json::Value) + Send + Sync + 'static,
    {
        *self
            .inner
            .emit_listener
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(Box::new(listener));
    }
}

/// A managed value borrowed from an `AppHandle`; derefs to `&T`.
pub struct State<'r, T: Send + Sync + 'static> {
    value: Arc<T>,
    _marker: PhantomData<&'r T>,
}

impl<'r, T: Send + Sync + 'static> State<'r, T> {
    pub fn inner(&self) -> &T {
        &self.value
    }
}

impl<T: Send + Sync + 'static> Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: Send + Sync + 'static> Clone for State<'_, T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            _marker: PhantomData,
        }
    }
}

pub trait Manager {
    /// Adds `state` unless a value of that type is already managed.
    /// Returns true when it was added (tauri's contract).
    fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool;

    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>>;

    /// Like tauri, panics when `T` was never managed on this handle.
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
        self.try_state::<T>().unwrap_or_else(|| {
            panic!(
                "state() called before manage() for {}",
                std::any::type_name::<T>()
            )
        })
    }
}

impl Manager for AppHandle {
    fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool {
        let mut states = self
            .inner
            .states
            .write()
            .unwrap_or_else(|p| p.into_inner());
        let key = TypeId::of::<T>();
        if states.contains_key(&key) {
            return false;
        }
        states.insert(key, Arc::new(state));
        true
    }

    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>> {
        let states = self
            .inner
            .states
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let any = Arc::clone(states.get(&TypeId::of::<T>())?);
        drop(states);
        let value = any.downcast::<T>().ok()?;
        Some(State {
            value,
            _marker: PhantomData,
        })
    }
}

pub trait Emitter {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> Result<()>;
}

impl Emitter for AppHandle {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> Result<()> {
        let listener = self
            .inner
            .emit_listener
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let Some(listener) = listener.as_ref() else {
            return Ok(());
        };
        let value =
            serde_json::to_value(&payload).map_err(|e| Error::EventSerialize(e.to_string()))?;
        listener(event, &value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    struct Counter(AtomicI64);

    #[test]
    fn each_handle_has_its_own_state() {
        let a = AppHandle::new();
        let b = AppHandle::new();
        assert!(a.manage(Counter(AtomicI64::new(1))));
        assert!(!a.manage(Counter(AtomicI64::new(9))));
        assert!(b.manage(Counter(AtomicI64::new(2))));
        a.state::<Counter>().0.store(5, Ordering::SeqCst);
        assert_eq!(a.clone().state::<Counter>().0.load(Ordering::SeqCst), 5);
        assert_eq!(b.state::<Counter>().0.load(Ordering::SeqCst), 2);
        assert!(a.try_state::<String>().is_none());
    }

    #[test]
    fn emit_reaches_this_handles_listener_inline() {
        let a = AppHandle::new();
        let seen = Arc::new(AtomicI64::new(0));
        let sink = Arc::clone(&seen);
        a.set_emit_listener(move |event, value| {
            assert_eq!(event, "e");
            sink.store(value["n"].as_i64().unwrap(), Ordering::SeqCst);
        });
        let h = a.clone();
        a.run_on_main_thread(move || {
            h.emit("e", serde_json::json!({ "n": 7 })).unwrap();
        })
        .unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 7);
        AppHandle::new().emit("e", 1).unwrap();
    }
}
