//! A workspace chat's file roots ([`crate::files::roots`]) as a session
//! context holds them: fetched from the chat's host when its tile shows a
//! workspace's own chat, listed again when the grant changes, and the root
//! the owner last picked (in a surface's picker, or by opening a reference
//! there). Files and Git read the shown root from here, so both follow the
//! same pick.

use std::borrow::Cow;

use cypher_proto::ChatFileRoot;
use cypher_rpc::RpcError;

use super::*;
use crate::files::roots::{self, RootId};

/// The roots of the selected chat, when it is a workspace's own chat.
#[derive(Default)]
pub(super) struct ChatRoots {
    /// What the list was fetched for ([`roots::list_key`]).
    key: Option<String>,
    /// `None` while the list is on its way.
    list: Option<Vec<ChatFileRoot>>,
    /// The chat the pick belongs to, and the pick (`None`: nothing picked).
    picked: Option<(String, RootId)>,
    task: Option<Task<()>>,
}

/// The selected chat's roots as a surface sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRoots<'a> {
    /// A session, a hidden thread, or nothing selected: no roots to choose
    /// from (a session's Files and Git use its checkout as before).
    None,
    /// A workspace's own chat whose list is on its way.
    Loading,
    /// A workspace's own chat: its folder first, then the granted projects.
    Ready(&'a [ChatFileRoot]),
}

impl AppState {
    /// Whether the selected chat is a workspace's own chat.
    pub fn workspace_chat_selected(&self) -> bool {
        self.selected_chat_row()
            .is_some_and(|chat| roots::is_workspace_chat(chat, &self.bots))
    }

    pub fn file_roots(&self) -> FileRoots<'_> {
        if !self.workspace_chat_selected() {
            return FileRoots::None;
        }
        match &self.chat_roots.list {
            Some(list) if self.chat_roots.key == self.roots_key() => FileRoots::Ready(list),
            _ => FileRoots::Loading,
        }
    }

    fn roots_pick(&self) -> Option<&RootId> {
        let chat = self.selected_chat.as_deref()?;
        self.chat_roots
            .picked
            .as_ref()
            .filter(|(picked_for, _)| picked_for == chat)
            .map(|(_, pick)| pick)
    }

    /// The root the Files surface shows (see [`roots::pick`]).
    pub fn files_root(&self) -> Option<&ChatFileRoot> {
        match self.file_roots() {
            FileRoots::Ready(list) => roots::pick(list, self.roots_pick(), |_| true),
            _ => None,
        }
    }

    /// The root the Git surfaces show: the pick when it is a checkout, else
    /// the first granted checkout.
    pub fn git_root(&self) -> Option<&ChatFileRoot> {
        match self.file_roots() {
            FileRoots::Ready(list) => {
                roots::pick(list, self.roots_pick(), |root| self.root_has_git(root))
            }
            _ => None,
        }
    }

    /// The roots Git may show: the checkouts among them.
    pub fn git_roots(&self) -> Vec<&ChatFileRoot> {
        match self.file_roots() {
            FileRoots::Ready(list) => list.iter().filter(|root| self.root_has_git(root)).collect(),
            _ => Vec::new(),
        }
    }

    /// A known checkout, or a project its host found git in.
    pub fn root_has_git(&self, root: &ChatFileRoot) -> bool {
        root.checkout_id.is_some()
            || root
                .space_id
                .as_deref()
                .and_then(|id| self.space_row(id))
                .is_some_and(|space| space.git_detected)
    }

    /// Show `root` in the selected workspace chat's Files and Git.
    pub fn pick_file_root(&mut self, root: RootId, cx: &mut Context<Self>) {
        let Some(chat) = self.selected_chat.clone() else {
            return;
        };
        let next = Some((chat, root));
        if self.chat_roots.picked != next {
            self.chat_roots.picked = next;
            cx.notify();
        }
    }

    /// The selected chat as the Git surfaces see it: a session as it is; a
    /// workspace's own chat at its Git root ([`Self::chat_at_root`]), or
    /// `None` while it has no checkout to show.
    pub fn git_chat_row(&self) -> Option<Cow<'_, Chat>> {
        let chat = self.selected_chat_row()?;
        if !roots::is_workspace_chat(chat, &self.bots) {
            return Some(Cow::Borrowed(chat));
        }
        self.git_root()
            .map(|root| Cow::Owned(self.chat_at_root(chat, root)))
    }

    /// `chat` re-aimed at `root`: its folder, checkout and project, so the
    /// diff stream resolves the project's checkout (by checkout id, else by
    /// folder) and the path-addressed git calls go there. The branch is the
    /// checkout's, which the chat row does not know.
    pub fn chat_at_root(&self, chat: &Chat, root: &ChatFileRoot) -> Chat {
        Chat {
            cwd: Some(root.path.clone()),
            checkout_id: root.checkout_id.clone().or_else(|| {
                root.space_id
                    .as_deref()
                    .and_then(|id| self.space_row(id))
                    .and_then(|space| space.checkout_id.clone())
            }),
            space_id: root.space_id.clone(),
            branch: None,
            ..chat.clone()
        }
    }

    fn roots_key(&self) -> Option<String> {
        let chat = self.selected_chat_row()?;
        roots::list_key(chat, &self.bots, &self.spaces)
    }

    /// Fetch the selected workspace chat's roots when what decides them
    /// changed (session contexts only: a tile's surfaces read them). An
    /// older host engine has no `ListChatFileRoots`: the workspace's own
    /// folder stands alone. Other failures do the same until the grant
    /// changes or the owner refreshes.
    pub(super) fn sync_file_roots(&mut self, cx: &mut Context<Self>) {
        if self.parent.is_none() {
            return;
        }
        let key = self.roots_key();
        if key == self.chat_roots.key {
            return;
        }
        self.chat_roots.key = key.clone();
        self.chat_roots.list = None;
        self.chat_roots.task = None;
        let (Some(key), Some(engine), Some(chat)) =
            (key, self.engine.clone(), self.selected_chat_row().cloned())
        else {
            return;
        };
        let name = self
            .bots
            .iter()
            .find(|bot| bot.chat_id == chat.id)
            .map(|bot| bot.name.clone())
            .unwrap_or_default();
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), chat.id.clone().into());
        if self.local_device_id.as_deref() != Some(chat.device_id.as_str()) {
            params.insert("targetDeviceId".into(), chat.device_id.clone().into());
        }
        self.chat_roots.task = Some(cx.spawn(async move |this, cx| {
            let result = crate::files::call_with_retry(
                &engine,
                methods::LIST_CHAT_FILE_ROOTS,
                serde_json::Value::Object(params),
                cx,
            )
            .await
            .and_then(|value| {
                serde_json::from_value::<Vec<ChatFileRoot>>(value)
                    .map_err(|e| RpcError::Failed(e.to_string()))
            });
            this.update(cx, |this, cx| {
                if this.chat_roots.key.as_deref() != Some(key.as_str()) {
                    return;
                }
                this.chat_roots.list = Some(listed_roots(result, &chat, &name));
                this.chat_roots.task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Tests: pick a root (`None`: the workspace's folder).
    #[cfg(test)]
    pub fn chat_roots_pick_for_test(&mut self, space_id: Option<&str>) {
        self.chat_roots.picked = self
            .selected_chat
            .clone()
            .map(|chat| (chat, space_id.map(str::to_owned)));
    }

    /// Tests: the selected chat's roots as if listed.
    #[cfg(test)]
    pub fn set_file_roots_for_test(&mut self, list: Vec<ChatFileRoot>) {
        self.chat_roots.key = self.roots_key();
        self.chat_roots.list = Some(list);
        self.chat_roots.task = None;
    }
}

/// The roots a `ListChatFileRoots` reply leaves: the list as answered; the
/// workspace's own folder alone when the host's engine predates the call
/// (or the call failed, or listed nothing).
fn listed_roots(
    reply: Result<Vec<ChatFileRoot>, RpcError>,
    chat: &Chat,
    name: &str,
) -> Vec<ChatFileRoot> {
    match reply {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => roots::fallback_roots(chat, name),
        Err(err) => {
            if !matches!(err, RpcError::UnknownMethod(_)) {
                tracing::debug!(error = %err, "file roots: list failed");
            }
            roots::fallback_roots(chat, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_engine_leaves_the_workspace_folder() {
        let chat = Chat {
            cwd: Some("/data/bots/b".into()),
            ..crate::test_fixtures::chat()
        };
        let own = |path: &str| ChatFileRoot {
            space_id: None,
            name: "Billing".into(),
            path: path.into(),
            checkout_id: None,
        };
        let old = listed_roots(
            Err(RpcError::UnknownMethod("ListChatFileRoots".into())),
            &chat,
            "Billing",
        );
        assert_eq!(old, vec![own("/data/bots/b")]);
        assert_eq!(listed_roots(Ok(Vec::new()), &chat, "Billing"), old);
        let listed = vec![
            own("/private/data/bots/b"),
            ChatFileRoot {
                space_id: Some("api".into()),
                name: "api".into(),
                path: "/src/api".into(),
                checkout_id: None,
            },
        ];
        assert_eq!(listed_roots(Ok(listed.clone()), &chat, "Billing"), listed);
    }
}
