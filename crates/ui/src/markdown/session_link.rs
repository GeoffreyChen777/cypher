//! In-app session links: `cypher://session/<chat id>` in rendered markdown
//! (an agent bot cites the sessions an answer draws on this way). A click
//! opens the session in this window instead of handing the URL to the OS.
//!
//! The markdown layer knows nothing of the shell, so a click parks the id in
//! [`PendingSessionLink`] and dispatches [`OpenSessionLink`]; the shell's
//! handler takes the id and opens the tab.

use gpui::{App, Global, Window, actions};

pub const SESSION_LINK_PREFIX: &str = "cypher://session/";

actions!(markdown, [OpenSessionLink]);

#[derive(Default)]
struct PendingSessionLink(Option<String>);

impl Global for PendingSessionLink {}

/// The chat id a session link names, when `url` is one.
pub fn session_link_id(url: &str) -> Option<&str> {
    let id = url.strip_prefix(SESSION_LINK_PREFIX)?.trim_end_matches('/');
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then_some(id)
}

/// Follow a clicked link: a session link opens in-app, anything else in the
/// OS.
pub fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    match session_link_id(url) {
        Some(id) => {
            cx.set_global(PendingSessionLink(Some(id.to_owned())));
            window.dispatch_action(Box::new(OpenSessionLink), cx);
        }
        None if url.starts_with(SESSION_LINK_PREFIX) => {}
        None => cx.open_url(url),
    }
}

/// The session a just-dispatched [`OpenSessionLink`] is for.
pub fn take_pending(cx: &mut App) -> Option<String> {
    cx.try_global::<PendingSessionLink>()?;
    cx.global_mut::<PendingSessionLink>().0.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_well_formed_session_links_are_in_app() {
        assert_eq!(session_link_id("cypher://session/ab-12_c"), Some("ab-12_c"));
        assert_eq!(session_link_id("cypher://session/ab12/"), Some("ab12"));
        assert_eq!(session_link_id("cypher://session/"), None);
        assert_eq!(session_link_id("cypher://session/../x"), None);
        assert_eq!(session_link_id("https://example.com"), None);
    }
}
