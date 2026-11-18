//! Session notification sounds — the herdr approach (state-transition chimes
//! played through the platform's own audio CLI, zero Rust audio deps):
//!
//! - two short chimes embedded in the binary (`assets/sounds/*.wav`, synthesized
//!   in-repo — no external assets): **done** (run finished) and **request**
//!   (agent is asking a question). Done uses the selected descending Soft pair;
//!   Request uses the adjusted ascending pair. Regenerate with
//!   `scripts/generate-notification-sounds.py`;
//! - playback = write to a temp file, hand it to the system player on a
//!   background thread: `afplay` (macOS), PowerShell `Media.SoundPlayer`
//!   (Windows), first of `paplay`/`pw-play`/`aplay`/`ffplay`/`mpv` (Linux —
//!   WAV, so even bare ALSA `aplay` decodes it);
//! - `CYPHER_DISABLE_SOUND` env kill-switch + the `soundEnabled` ui-setting;
//! - failures are logged and swallowed — a missing player must never bother
//!   the session flow.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

static SOUND_DONE: &[u8] = include_bytes!("../../assets/sounds/done.wav");
static SOUND_REQUEST: &[u8] = include_bytes!("../../assets/sounds/request.wav");

/// Which notification chime to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// A run finished (Working → Idle).
    Done,
    /// The agent is waiting on a question (→ AwaitingInput).
    Request,
}

/// Play a chime on a background thread. Silently a no-op when disabled or no
/// player is available.
pub fn play(sound: Sound) {
    if cypher_env::var_os("DISABLE_SOUND").is_some() {
        return;
    }
    std::thread::spawn(move || {
        let data = match sound {
            Sound::Done => SOUND_DONE,
            Sound::Request => SOUND_REQUEST,
        };
        if let Err(err) = play_bytes(data) {
            tracing::debug!(?sound, error = %err, "notification sound playback failed");
        }
    });
}

fn play_bytes(data: &[u8]) -> Result<(), String> {
    // The system players want a file path; write the embedded bytes out.
    let tmp = temp_path();
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    let result = run_player(&tmp);
    let _ = std::fs::remove_file(&tmp);
    result
}

fn temp_path() -> PathBuf {
    let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("cypher-sound-{}-{id}.wav", std::process::id()))
}

#[cfg(target_os = "macos")]
fn run_player(path: &Path) -> Result<(), String> {
    run_checked("afplay", &[], path)
}

#[cfg(windows)]
fn run_player(path: &Path) -> Result<(), String> {
    // SoundPlayer handles WAV natively; PlaySync keeps the process alive for
    // the chime's duration.
    let script = format!(
        "(New-Object Media.SoundPlayer '{}').PlaySync()",
        path.display()
    );
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("powershell failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("powershell exited with {}", output.status))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_player(path: &Path) -> Result<(), String> {
    // WAV everywhere, so even bare ALSA aplay decodes it (herdr must exclude
    // aplay because it ships mp3s).
    let players: &[(&str, &[&str])] = &[
        ("paplay", &[]),
        ("pw-play", &[]),
        ("aplay", &["-q"]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ("mpv", &["--no-video", "--really-quiet"]),
    ];
    let mut errors = Vec::new();
    for (program, args) in players {
        match run_checked(program, args, path) {
            Ok(()) => return Ok(()),
            Err(err) => errors.push(err),
        }
    }
    Err(format!("no audio player available: {}", errors.join("; ")))
}

fn run_checked(program: &str, args: &[&str], path: &Path) -> Result<(), String> {
    // Bounded wait: a wedged audio daemon must not accumulate zombie threads.
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} exited with {status}")),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{program} timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program}: {err}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Transition mapping (pure — herdr's notification_sound_for_state_change)
// ---------------------------------------------------------------------------

use cypher_proto::{Chat, SessionStatus};

/// One baseline per user-facing conversation. Internal sessions must neither
/// notify on their own nor leave an edge to replay when chat metadata arrives
/// late (or a chat becomes visible again).
#[derive(Default)]
pub struct SessionChimes {
    previous: std::collections::HashMap<String, SessionStatus>,
}

impl SessionChimes {
    pub fn observe(
        &mut self,
        chat_id: &str,
        chat: Option<&Chat>,
        status: SessionStatus,
    ) -> Option<Sound> {
        if !chat.is_some_and(Chat::is_user_facing) {
            self.previous.remove(chat_id);
            return None;
        }
        let prev = self.previous.insert(chat_id.to_owned(), status)?;
        sound_for_transition(prev, status)
    }
}

/// Which chime (if any) a session-status transition deserves. Same-state
/// updates never chime; a question chimes; a completion chimes on the
/// Working→Idle edge. Call through [`SessionChimes`] to exclude internal chats.
pub fn sound_for_transition(prev: SessionStatus, new: SessionStatus) -> Option<Sound> {
    if prev == new {
        return None;
    }
    match new {
        SessionStatus::AwaitingInput => Some(Sound::Request),
        SessionStatus::Idle if prev == SessionStatus::Working => Some(Sound::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{BotChat, BotOrigin, BotThread};

    fn chat() -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": "chat",
            "deviceId": "device",
            "archived": false,
            "createdAt": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn child_chat() -> Chat {
        let mut chat = chat();
        chat.id = "child".into();
        chat.child = Some(
            serde_json::from_value(serde_json::json!({
                "parentChatId": "chat",
                "parentRunId": "run",
                "agent": "worker",
                "task": "background work",
                "mode": "async",
                "profile": { "systemPrompt": "" }
            }))
            .unwrap(),
        );
        chat
    }

    fn assert_silent(chat: &Chat) {
        use SessionStatus::*;
        let mut chimes = SessionChimes::default();
        for status in [Working, AwaitingInput, Working, Idle, Errored] {
            assert_eq!(chimes.observe(&chat.id, Some(chat), status), None);
        }
    }

    #[test]
    fn normal_sessions_and_workspace_owner_chats_still_notify_once() {
        use SessionStatus::*;
        let regular = chat();
        let mut workspace = chat();
        workspace.bot = Some(BotChat::owner("bot"));
        for chat in [regular, workspace] {
            let mut chimes = SessionChimes::default();
            for (status, expected) in [
                (Idle, None),
                (Working, None),
                (AwaitingInput, Some(Sound::Request)),
                (AwaitingInput, None),
                (Working, None),
                (Idle, Some(Sound::Done)),
                (Idle, None),
            ] {
                assert_eq!(chimes.observe(&chat.id, Some(&chat), status), expected);
            }
        }
    }

    #[test]
    fn children_memory_passes_and_archived_chats_never_notify() {
        assert_silent(&child_chat());
        let mut memory = chat();
        memory.memory = Some(cypher_proto::MemoryChat {
            store_id: "store".into(),
            scope: cypher_proto::MemoryScope::You,
            pass_id: "pass".into(),
        });
        assert_silent(&memory);
        let mut archived = chat();
        archived.archived = true;
        assert_silent(&archived);
    }

    #[test]
    fn all_internal_bot_threads_and_non_owner_origins_stay_silent() {
        for origin in [BotOrigin::Owner, BotOrigin::Routine, BotOrigin::Peer] {
            for thread in [
                BotThread::Main,
                BotThread::Work {
                    work_id: "work".into(),
                },
                BotThread::Routine {
                    routine_id: "routine".into(),
                },
                BotThread::RoutineSession {
                    routine_id: "project-routine".into(),
                },
                BotThread::Peer {
                    connection_id: "connection".into(),
                },
                BotThread::Unknown,
            ] {
                let marker = BotChat::thread("bot", origin, thread);
                if marker.is_owner_chat() {
                    continue;
                }
                let mut chat = chat();
                chat.bot = Some(marker);
                assert_silent(&chat);
            }
        }
    }

    #[test]
    fn missing_metadata_never_rings_or_seeds_a_late_completion() {
        use SessionStatus::*;
        let chat = chat();
        let mut chimes = SessionChimes::default();
        for status in [Working, AwaitingInput, Working, Idle, Working] {
            assert_eq!(chimes.observe(&chat.id, None, status), None);
        }
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Idle), None);
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Working), None);
        assert_eq!(
            chimes.observe(&chat.id, Some(&chat), Idle),
            Some(Sound::Done)
        );
        // Losing metadata also clears an existing baseline.
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Working), None);
        assert_eq!(chimes.observe(&chat.id, None, Working), None);
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Idle), None);
    }

    #[test]
    fn unarchiving_does_not_replay_a_suppressed_completion() {
        use SessionStatus::*;
        let mut chat = chat();
        let mut chimes = SessionChimes::default();
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Working), None);
        chat.archived = true;
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Working), None);
        chat.archived = false;
        assert_eq!(chimes.observe(&chat.id, Some(&chat), Idle), None);
    }

    #[test]
    fn child_input_notifies_only_when_forwarded_to_the_parent() {
        use SessionStatus::*;
        let parent = chat();
        let child = child_chat();
        let mut chimes = SessionChimes::default();
        assert_eq!(chimes.observe(&parent.id, Some(&parent), Working), None);
        assert_eq!(chimes.observe(&child.id, Some(&child), Working), None);
        assert_eq!(chimes.observe(&child.id, Some(&child), AwaitingInput), None);
        assert_eq!(
            chimes.observe(&parent.id, Some(&parent), AwaitingInput),
            Some(Sound::Request)
        );
        assert_eq!(chimes.observe(&child.id, Some(&child), Working), None);
        assert_eq!(chimes.observe(&child.id, Some(&child), Idle), None);
        assert_eq!(chimes.observe(&parent.id, Some(&parent), Working), None);
        assert_eq!(
            chimes.observe(&parent.id, Some(&parent), Idle),
            Some(Sound::Done)
        );
    }

    #[test]
    fn transition_mapping_matches_herdr_semantics() {
        use SessionStatus::*;
        // A question always chimes, wherever it came from.
        assert_eq!(
            sound_for_transition(Working, AwaitingInput),
            Some(Sound::Request)
        );
        assert_eq!(
            sound_for_transition(Idle, AwaitingInput),
            Some(Sound::Request)
        );
        // Completion = the Working→Idle edge only.
        assert_eq!(sound_for_transition(Working, Idle), Some(Sound::Done));
        assert_eq!(sound_for_transition(AwaitingInput, Idle), None);
        assert_eq!(sound_for_transition(Errored, Idle), None);
        // Same state / other edges stay silent.
        assert_eq!(sound_for_transition(Working, Working), None);
        assert_eq!(sound_for_transition(Idle, Working), None);
        assert_eq!(sound_for_transition(Working, Errored), None);
    }

    #[test]
    fn temp_paths_are_unique() {
        assert_ne!(temp_path(), temp_path());
    }

    #[test]
    fn embedded_chimes_are_wav() {
        for data in [SOUND_DONE, SOUND_REQUEST] {
            assert!(data.len() > 1000);
            assert_eq!(&data[..4], b"RIFF");
            assert_eq!(&data[8..12], b"WAVE");
        }
    }
}
