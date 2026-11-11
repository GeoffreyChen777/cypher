//! The one-time "What's new" card after a large update: a few slides, each a
//! picture over a title and a line of copy. The shell renders it
//! (`shell/whats_new.rs`); this module holds what it says.
//!
//! - Show it again after a later update: rewrite [`SLIDES`] and bump
//!   [`UPDATE_INTRO_VERSION`]. A `ui-settings.json` that recorded a lower
//!   number (`seenUpdateIntro`) shows the card once; new installs skip it.
//! - Add a screenshot: put the file in `crates/ui/assets/whats-new/` and set
//!   the slide's `image` to `Some(include_bytes!("../assets/whats-new/…"))`.
//!   PNG, JPEG or WebP at 1040×650 (the picture's 520×325 at 2x). Each file
//!   ships inside the binary: a palette PNG of 32 colours keeps a window
//!   shot near 40 KB. A slide without one shows its icon instead.
//! - Preview: `CYPHER_OPEN_DIALOG=whats-new` opens it on every launch.

use gpui::SharedString;

use crate::kit::icons;

/// The card's edition. Bump it with new [`SLIDES`] to show the card again.
pub const UPDATE_INTRO_VERSION: u32 = 1;

/// The asset path prefix [`asset`] serves (through `kit::icons::Assets`).
const ASSET_PREFIX: &str = "whats-new/";

pub struct Slide {
    pub title: &'static str,
    pub body: &'static str,
    /// Shown in place of the picture while [`Self::image`] is `None`.
    pub icon: &'static str,
    /// An embedded screenshot (`include_bytes!`).
    pub image: Option<&'static [u8]>,
}

pub const SLIDES: &[Slide] = &[
    Slide {
        title: "Workspaces",
        body: "Give a workspace a goal: its agent plans the work, runs it in sessions and \
               reports back. Switch to Workspaces from the logo at the top of the sidebar.",
        icon: icons::BOT,
        image: Some(include_bytes!("../assets/whats-new/workspaces.png")),
    },
    Slide {
        title: "Needs you",
        body: "One inbox for the decisions waiting on you: plans to approve, questions to \
               answer and notes held for review.",
        icon: icons::BELL,
        image: Some(include_bytes!("../assets/whats-new/needs-you.png")),
    },
    Slide {
        title: "Routines",
        body: "Prompts that run on a schedule, with the model you choose. Each result \
               arrives in the workspace chat as a card.",
        icon: icons::CLOCK,
        image: Some(include_bytes!("../assets/whats-new/routines.png")),
    },
    Slide {
        title: "Memory",
        body: "Agents keep what they learn about you, each project and each workspace as \
               files you can read and edit in Settings → Memory. A nightly pass tidies them.",
        icon: icons::BOOK,
        image: Some(include_bytes!("../assets/whats-new/memory.png")),
    },
    Slide {
        title: "Connections",
        body: "Invite a teammate with a code, or paste theirs. Their workspace can then ask \
               yours questions, read-only and within what you allow.",
        icon: icons::USERS,
        image: Some(include_bytes!("../assets/whats-new/connections.png")),
    },
    Slide {
        title: "Shortcuts",
        body: "Ask a workspace's agent to turn a workflow into a button at the bottom of \
               the sidebar. One click runs it again.",
        icon: icons::BOLT,
        image: Some(include_bytes!("../assets/whats-new/shortcuts.png")),
    },
];

/// Whether the card opens at boot: an edition this user has not seen, unless
/// first-run setup is still ahead (a new install has nothing to compare).
pub fn should_show(seen: u32, setup_completed: bool, debug: bool) -> bool {
    debug || (setup_completed && seen < UPDATE_INTRO_VERSION)
}

/// The `img()` path of slide `index`'s screenshot, when it has one.
pub fn image_path(index: usize) -> Option<SharedString> {
    SLIDES.get(index)?.image?;
    Some(format!("{ASSET_PREFIX}{index}").into())
}

/// The bytes behind an [`image_path`].
pub fn asset(path: &str) -> Option<&'static [u8]> {
    let index = path.strip_prefix(ASSET_PREFIX)?.parse::<usize>().ok()?;
    SLIDES.get(index)?.image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_once_per_edition_and_never_before_setup() {
        assert!(should_show(0, true, false));
        assert!(!should_show(UPDATE_INTRO_VERSION, true, false));
        assert!(!should_show(0, false, false));
        assert!(should_show(UPDATE_INTRO_VERSION, true, true));
    }

    #[test]
    fn image_paths_resolve_only_for_slides_with_images() {
        for (index, slide) in SLIDES.iter().enumerate() {
            let path = image_path(index);
            assert_eq!(path.is_some(), slide.image.is_some());
            if let Some(path) = path {
                assert!(asset(&path).is_some());
            }
        }
        assert!(asset("whats-new/99").is_none());
        assert!(asset("icons/bot.svg").is_none());
    }
}
