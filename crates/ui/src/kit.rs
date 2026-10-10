//! The UI kit: the theme and the app-agnostic primitives every surface builds
//! on — icons, popovers, motion, frost, loaders, sounds, shadows, edge fades,
//! the syntax highlight cache and atomic file writes. Nothing here knows
//! about app state, settings or any surface; modules outside the kit depend
//! on it, never the other way round.

pub mod edge_fade;
pub mod frost;
pub mod fs_util;
pub mod icons;
pub mod loaders;
pub mod motion;
pub mod popover;
pub mod soft_shadow;
pub mod sound;
pub mod syntax_cache;
pub mod theme;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock one of the crate's `std` mutexes, ignoring poisoning. They guard
/// short critical sections over caches and paint state; a panic that
/// poisoned one leaves data that is still safe to read, and panicking on
/// every later frame would only turn one failure into a dead window.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
