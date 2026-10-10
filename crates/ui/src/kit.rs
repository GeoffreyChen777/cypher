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
