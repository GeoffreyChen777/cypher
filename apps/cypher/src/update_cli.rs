//! `cypher update` — one command that brings a device fully current: the
//! release binary, the running service, and the Pi Runtime.
//!
//! Linux (the headless/VPS target): download → verify → swap `current` →
//! restart the service → wait for the new engine → install the newest
//! Runtime through it. A binary that is not yet in the managed layout
//! (`~/.cypher/app/<ver>` + `current`) is adopted into it, so the command
//! works after every kind of Linux installation, not only the curl|sh one.
//! macOS app bundles swap the bundle instead; source builds are report-only.

use std::time::Duration;

use anyhow::{Context as _, bail};
use cypher_engine::EngineConfig;
use cypher_rpc::methods;
use cypher_update::{InstallKind, current_version, version_newer};

pub struct UpdateOptions {
    /// Report only; exit 1 when the binary or the Runtime has a newer release.
    pub check: bool,
    /// Restart the service even while runs are active.
    pub force: bool,
}

struct RuntimeFacts {
    installed: Option<String>,
    latest: Result<String, String>,
}

impl RuntimeFacts {
    fn newer(&self) -> bool {
        match (&self.latest, &self.installed) {
            (Ok(latest), Some(installed)) => version_newer(latest, installed),
            (Ok(_), None) => true,
            (Err(_), _) => false,
        }
    }

    fn line(&self) -> String {
        let installed = self.installed.as_deref().unwrap_or("not installed");
        match &self.latest {
            Ok(latest) if self.newer() => format!("{installed} → {latest} available"),
            Ok(_) => format!("{installed} (up to date)"),
            Err(err) => format!("{installed} (latest unknown: {err})"),
        }
    }
}

async fn runtime_facts(config: &EngineConfig) -> RuntimeFacts {
    RuntimeFacts {
        installed: cypher_engine::pi_runtime::installed_runtime(&config.data_dir)
            .map(|runtime| runtime.version),
        latest: cypher_engine::pi_runtime::latest_manifest(&config.edge_url)
            .await
            .map(|manifest| manifest.version),
    }
}

pub async fn update(config: EngineConfig, options: UpdateOptions) -> anyhow::Result<()> {
    let edge_url = config.edge_url.clone();
    let manifest = cypher_update::fetch_latest(&edge_url).await?;
    let current = current_version();
    let app_newer = version_newer(&manifest.version, current);
    println!(
        "Cypher:   {current}{}",
        if app_newer {
            format!(" → {} available", manifest.version)
        } else {
            " (up to date)".into()
        }
    );
    let runtime = runtime_facts(&config).await;
    println!("Runtime:  {}", runtime.line());
    if options.check {
        if app_newer || runtime.newer() {
            std::process::exit(1);
        }
        return Ok(());
    }

    let kind = cypher_update::detect_install();
    let exe = std::env::current_exe().context("resolving the cypher executable path")?;
    match &kind {
        InstallKind::MacApp { bundle } => {
            if !app_newer {
                return Ok(());
            }
            println!(
                "downloading {}…",
                cypher_update::mac_app_artifact(&manifest.version)
            );
            let data_dir = super::dirs_data_dir();
            let staged = cypher_update::stage_mac_app(&edge_url, &manifest, &data_dir).await?;
            cypher_update::apply_mac_app(&staged, bundle)?;
            println!("updated {} — relaunch Cypher to finish.", bundle.display());
            return Ok(());
        }
        InstallKind::Unmanaged if !cfg!(target_os = "linux") => {
            if !app_newer {
                return Ok(());
            }
            bail!(
                "this binary is not update-managed (source build or hand-copied).\n\
                 macOS: download the new Cypher.app dmg, or rebuild from source."
            )
        }
        InstallKind::Unmanaged if cypher_update::is_source_build(&exe) => {
            if !app_newer && !runtime.newer() {
                return Ok(());
            }
            bail!(
                "{} is a source build; updates are report-only here.\n\
                 Rebuild from git, or install a release: curl -fsSL {edge_url}/install.sh | sh",
                exe.display()
            )
        }
        InstallKind::Managed { .. } | InstallKind::Unmanaged => {}
    }
    let adopt = matches!(kind, InstallKind::Unmanaged);
    if !app_newer && !runtime.newer() && !adopt {
        return Ok(());
    }

    // Never restart under a live run. `connect` also refuses a foreign
    // listener on our socket rather than treating it as "no engine".
    let live = crate::setup_cli::connect(&config).await?;
    if let Some(live) = &live
        && !options.force
    {
        crate::setup_cli::ensure_idle(live).await.map_err(|err| {
            anyhow::anyhow!("{err}\nUse `cypher update --force` to restart anyway.")
        })?;
    }
    let service = crate::daemon::service_installed();

    let mut binary_changed = false;
    match kind {
        InstallKind::Managed { app_root } if app_newer => {
            println!(
                "Downloading {}…",
                cypher_update::headless_artifact(&manifest.version)
            );
            cypher_update::stage_headless(&edge_url, &manifest, &app_root).await?;
            cypher_update::apply_headless(&app_root, &manifest.version)?;
            if cypher_update::migrate_linux_service_to_current(&config.data_dir)? {
                println!("✓ Service switched to app/current");
            }
            println!(
                "✓ Cypher {} installed (current → {})",
                manifest.version, manifest.version
            );
            binary_changed = true;
        }
        InstallKind::Unmanaged => {
            println!(
                "{} is not in the managed layout; installing Cypher {} under ~/.cypher/app…",
                exe.display(),
                manifest.version
            );
            let home = cypher_env::home_dir();
            let app_root = cypher_update::adopt_managed_install(
                &edge_url,
                &manifest,
                &home,
                &config.data_dir,
                &exe,
            )
            .await?;
            println!(
                "✓ Cypher {} installed ({} → current); ~/.local/bin/cypher now links there",
                manifest.version,
                app_root.display()
            );
            binary_changed = true;
        }
        _ => {}
    }

    let mut restarted = false;
    if binary_changed {
        if service {
            match cypher_update::restart_service(&config.data_dir) {
                Ok(()) => {
                    println!("✓ Service restarted");
                    restarted = true;
                }
                Err(err) => println!(
                    "note: service restart failed ({err:#}); restart the engine to run the new version."
                ),
            }
        } else if live.is_some() {
            println!(
                "note: the running engine is not a managed service; restart it to run the new version."
            );
        }
    }
    drop(live);

    // The Runtime is installed by the engine that will use it, so its
    // install mutex, package reconciliation and catalog reload all apply.
    // Without any engine, install directly under the same data lock the
    // engine takes.
    let want_runtime = runtime.newer() || runtime.installed.is_none();
    if want_runtime && runtime.latest.is_err() {
        println!("note: Runtime update skipped; the Runtime manifest could not be fetched.");
    } else if want_runtime {
        let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
        let engine = if restarted {
            print!("Waiting for the engine…");
            std::io::Write::flush(&mut std::io::stdout())?;
            let live = crate::setup_cli::ready(&config, false, &cancel).await;
            println!();
            match live {
                Ok(live) => Some(live),
                Err(err) => {
                    println!("note: {err:#}");
                    None
                }
            }
        } else {
            crate::setup_cli::connect(&config).await?
        };
        drop(cancel_tx);
        match engine {
            Some(live) => {
                println!(
                    "Installing Runtime {}…",
                    runtime.latest.as_deref().unwrap_or("")
                );
                tokio::time::timeout(
                    Duration::from_secs(30 * 60),
                    live.client.call(methods::INSTALL_PI, serde_json::json!({})),
                )
                .await
                .context("Runtime installation timed out.")?
                .map_err(|err| anyhow::anyhow!("Runtime installation failed: {err}"))?;
            }
            None if restarted => {
                println!("note: the new engine will install the Runtime itself once it is up.")
            }
            None => {
                println!(
                    "Installing Runtime {}…",
                    runtime.latest.as_deref().unwrap_or("")
                );
                install_runtime_offline(&config).await?;
            }
        }
        println!(
            "✓ Runtime: {}",
            crate::setup_cli::runtime_label(&config.data_dir)
        );
    } else if restarted {
        // Readiness is part of "updated": a service that failed to come back
        // is not an update that finished.
        let (_cancel_tx, cancel) = tokio::sync::watch::channel(false);
        crate::setup_cli::ready(&config, false, &cancel).await?;
        println!("✓ Engine ready");
    }
    Ok(())
}

async fn install_runtime_offline(config: &EngineConfig) -> anyhow::Result<()> {
    let _lock = cypher_engine::InstanceLock::acquire(&config.data_dir).map_err(|_| {
        anyhow::anyhow!("An engine is starting; run `cypher update` again shortly.")
    })?;
    let manager = cypher_engine::pi_runtime::PiRuntimeManager::spawn(
        config.edge_url.clone(),
        &config.data_dir,
    );
    let ok = manager.check_updates().await;
    let error = manager.update_status().error.clone();
    manager.shutdown().await;
    if !ok {
        bail!(
            "Runtime installation failed: {}",
            error.unwrap_or_else(|| "unknown error".into())
        );
    }
    Ok(())
}
