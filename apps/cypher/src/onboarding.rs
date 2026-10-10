//! Terminal sign-in and workspace onboarding for the CLI (`cypher login`,
//! `cypher setup`, and a headless start whose synced session has no
//! workspace yet).

use cypher_engine::{Auth, AuthState};

const DEFAULT_PERSONAL_ORG_NAME: &str = "Personal";

/// Block until the WorkOS session is signed in AND org-scoped. On a TTY, print the
/// headless (paste-code) sign-in URL, read the pasted `state.code` from stdin, and
/// run workspace onboarding (auto-provision / auto-join / numbered picker). Off a
/// TTY this errors immediately — a daemon under systemd/launchd must load the
/// session that `cypher login` persisted, never wait on a prompt nobody can see.
pub async fn terminal_sign_in(auth: &Auth) -> anyhow::Result<()> {
    terminal_sign_in_until(auth, std::future::pending()).await
}

/// Cancellable CLI onboarding. Reader tasks are aborted and joined before
/// returning, so a caller may safely release its auth lock/restart a service.
pub async fn terminal_sign_in_until(
    auth: &Auth,
    cancelled: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(cancelled);
    use std::io::IsTerminal;
    let interactive = std::io::stdin().is_terminal();
    let mut state_rx = auth.watch_state();
    let mut stdin_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut org_reader: Option<tokio::task::JoinHandle<()>> = None;
    let outcome = loop {
        let state = state_rx.borrow().clone();
        match state {
            AuthState::SignedIn { user, org_id } => {
                tracing::info!(email = %user.email, org = org_id.as_deref().unwrap_or("<none>"),
                    "auth: session ready");
                break Ok(());
            }
            AuthState::NeedsOrganization { user } => {
                if !interactive {
                    // No reader tasks have been spawned on this path (both spawns
                    // are TTY-gated), so an early return leaks nothing.
                    return Err(anyhow::anyhow!(
                        "signed in as {} but no workspace is selected — run `cypher login` on this machine to pick one",
                        user.email
                    ));
                }
                if org_reader.is_none() {
                    // Workspace onboarding on the TTY: provision a personal
                    // organization if none exists, auto-join a single
                    // membership, or show a numbered picker otherwise.
                    println!("Signed in as {}.", user.email);
                    org_reader = Some(tokio::spawn(run_org_onboarding(auth.clone())));
                }
            }
            AuthState::SignedOut => {
                if !interactive {
                    return Err(anyhow::anyhow!(
                        "not signed in — run `cypher login` on this machine first"
                    ));
                }
                if stdin_reader.is_none() {
                    let url = auth.start_headless_sign_in();
                    println!("Sign in to Cypher:\n\n  {url}\n");
                    println!("Then paste the code shown in the browser here and press enter.");
                    let auth = auth.clone();
                    stdin_reader = Some(tokio::spawn(async move {
                        loop {
                            let Some(line) = read_stdin_line().await else {
                                return;
                            };
                            let pasted = line.trim();
                            if pasted.is_empty() {
                                continue;
                            }
                            match auth.complete_sign_in(pasted).await {
                                Ok(()) => return,
                                Err(err) => {
                                    println!("Sign-in failed: {err}");
                                    if auth.email_verification_pending() {
                                        println!(
                                            "Enter the six-digit verification code sent to your email:"
                                        );
                                        loop {
                                            let Some(line) = read_stdin_line().await else {
                                                return;
                                            };
                                            let code = line.trim();
                                            if code.is_empty() {
                                                continue;
                                            }
                                            match auth.complete_email_verification(code).await {
                                                Ok(()) => return,
                                                Err(err) => {
                                                    println!("Email verification failed: {err}")
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }));
                }
            }
        }
        // EOF, a failed workspace request, or a reader panic does not change
        // auth state. Waiting only for that state used to hang login forever.
        tokio::select! {
            _ = &mut cancelled => {
                break Err(anyhow::anyhow!("sign-in canceled; run `cypher setup` to continue"));
            }
            changed = state_rx.changed() => {
                if changed.is_err() {
                    break Err(anyhow::anyhow!("authentication closed before sign-in completed"));
                }
            }
            result = wait_terminal_reader(&mut stdin_reader) => {
                stdin_reader = None;
                if result.is_err() || matches!(auth.state(), AuthState::SignedOut) {
                    break Err(anyhow::anyhow!("sign-in did not complete (terminal input closed)"));
                }
            }
            result = wait_terminal_reader(&mut org_reader) => {
                org_reader = None;
                if result.is_err() || !auth.state().is_signed_in() {
                    break Err(anyhow::anyhow!("workspace setup did not complete — retry `cypher login`"));
                }
            }
        }
    };
    if let Some(reader) = stdin_reader {
        reader.abort();
        let _ = reader.await;
    }
    if let Some(reader) = org_reader {
        reader.abort();
        let _ = reader.await;
    }
    outcome
}

async fn wait_terminal_reader(
    reader: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), tokio::task::JoinError> {
    match reader {
        Some(reader) => reader.await,
        None => std::future::pending().await,
    }
}

/// One line from stdin (blocking read off the runtime). `None` = stdin closed.
async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None, // EOF / error
            Ok(_) => Some(line),
        }
    })
    .await
    .ok()
    .flatten()
}

/// TTY workspace onboarding for an org-less session: no memberships →
/// automatically provision `Personal`; exactly one → auto-join; several →
/// numbered picker. Success flips the auth state to `SignedIn`, which ends
/// [`terminal_sign_in`]'s wait (and aborts this task).
async fn run_org_onboarding(auth: Auth) {
    let orgs = match auth.list_orgs().await {
        Ok(orgs) => orgs,
        Err(err) => {
            println!(
                "Could not list workspaces ({err}) — create or select one from the Cypher UI to continue."
            );
            return;
        }
    };
    match orgs.len() {
        0 => {
            println!("Preparing your synced workspace…");
            if let Err(err) = auth.create_org(DEFAULT_PERSONAL_ORG_NAME).await {
                println!("Creating workspace failed: {err}");
            }
        }
        1 => {
            let only = &orgs[0];
            println!("Joining workspace \"{}\"…", only.name);
            if let Err(err) = auth.select_org(&only.organization_id).await {
                println!("Joining workspace failed: {err}");
            }
        }
        _ => {
            println!("\nYour workspaces:");
            for (index, org) in orgs.iter().enumerate() {
                println!("  {}. {}", index + 1, org.name);
            }
            println!("Pick a workspace [1-{}]:", orgs.len());
            loop {
                let Some(line) = read_stdin_line().await else {
                    return;
                };
                let choice = line
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|index| orgs.get(index));
                let Some(org) = choice else {
                    println!("Pick a workspace [1-{}]:", orgs.len());
                    continue;
                };
                match auth.select_org(&org.organization_id).await {
                    Ok(()) => return,
                    Err(err) => println!("Joining workspace failed: {err}"),
                }
            }
        }
    }
}
