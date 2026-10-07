//! Dev-only UI driver (`--features dev-capture`, never in release builds).
//!
//! The agent iterating on the UI has no Screen Recording or Accessibility
//! permission, so it can neither screenshot the window nor click it. This
//! module lets it do both through files: with `CYPHER_CAPTURE_DIR` set, the
//! main window polls `{dir}/cmd`, runs its lines in order, deletes it, and
//! appends results to `{dir}/log`. Captures come from gpui's own Metal
//! renderer (`Window::render_to_image`), not the window server.
//!
//! Commands (coordinates are logical px from the window's top-left):
//! `capture NAME` · `wait MS` · `move X Y` · `click X Y [cmd|shift|alt|ctrl]…`
//! · `rclick X Y` · `mclick X Y` · `dclick X Y` · `drag X1 Y1 X2 Y2`
//! · `scroll X Y DY` · `key COMBO` (e.g. `cmd-\`) · `type TEXT` · `action NAME` (e.g.
//! `shell::SplitRight`) · `size W H`.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Point, ScrollDelta,
    ScrollWheelEvent, TouchPhase, Window, point, px, size,
};

/// Start polling for commands if `CYPHER_CAPTURE_DIR` is set. Idempotent:
/// the main window calls it from every render; only the first call starts.
pub fn start_once(window: AnyWindowHandle, cx: &mut App) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(dir) = cypher_env::var("CAPTURE_DIR").map(PathBuf::from) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    tracing::info!(dir = %dir.display(), "dev capture driver listening");
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let cmd = dir.join("cmd");
            let Ok(script) = std::fs::read_to_string(&cmd) else {
                continue;
            };
            let _ = std::fs::remove_file(&cmd);
            for line in script.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let result = run(line, window, &dir, cx).await;
                log(&dir, line, result);
            }
            log(&dir, "done", Ok(()));
        }
    })
    .detach();
}

fn log(dir: &std::path::Path, line: &str, result: anyhow::Result<()>) {
    use std::io::Write as _;
    let entry = match result {
        Ok(()) => format!("ok {line}\n"),
        Err(err) => format!("ERR {line}: {err:#}\n"),
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("log"))
    {
        let _ = file.write_all(entry.as_bytes());
    }
}

fn num(parts: &[&str], ix: usize) -> anyhow::Result<f32> {
    parts
        .get(ix)
        .ok_or_else(|| anyhow::anyhow!("missing argument {ix}"))?
        .parse::<f32>()
        .map_err(Into::into)
}

fn at(parts: &[&str], ix: usize) -> anyhow::Result<Point<gpui::Pixels>> {
    Ok(point(px(num(parts, ix)?), px(num(parts, ix + 1)?)))
}

fn modifiers(parts: &[&str]) -> Modifiers {
    let mut m = Modifiers::default();
    for part in parts {
        match *part {
            "cmd" => m.platform = true,
            "shift" => m.shift = true,
            "alt" => m.alt = true,
            "ctrl" => m.control = true,
            _ => {}
        }
    }
    m
}

async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn input(window: AnyWindowHandle, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    cx.update_window(window, |_, window: &mut Window, cx| {
        window.dispatch_event(event, cx);
        window.refresh();
    })
}

async fn click(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    position: Point<gpui::Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        }),
    )?;
    pause(cx, 30).await;
    for count in 1..=click_count {
        input(
            window,
            cx,
            PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers,
                click_count: count,
                first_mouse: false,
            }),
        )?;
        pause(cx, 30).await;
        input(
            window,
            cx,
            PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers,
                click_count: count,
            }),
        )?;
        pause(cx, 30).await;
    }
    Ok(())
}

async fn run(
    line: &str,
    window: AnyWindowHandle,
    dir: &std::path::Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts[0] {
        "wait" => pause(cx, num(&parts, 1)? as u64).await,
        "capture" => {
            let name = parts.get(1).copied().unwrap_or("capture");
            // A fresh frame first: the capture reads the last rendered scene.
            cx.update_window(window, |_, window, _| window.refresh())?;
            pause(cx, 150).await;
            let image = cx.update_window(window, |_, window, _| window.render_to_image())??;
            image.save(dir.join(format!("{name}.png")))?;
        }
        "move" => input(
            window,
            cx,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: at(&parts, 1)?,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
        )?,
        "click" => {
            let m = modifiers(&parts[3..]);
            click(window, cx, at(&parts, 1)?, MouseButton::Left, m, 1).await?
        }
        "dclick" => {
            click(
                window,
                cx,
                at(&parts, 1)?,
                MouseButton::Left,
                Modifiers::default(),
                2,
            )
            .await?
        }
        "rclick" => {
            click(
                window,
                cx,
                at(&parts, 1)?,
                MouseButton::Right,
                Modifiers::default(),
                1,
            )
            .await?
        }
        "mclick" => {
            click(
                window,
                cx,
                at(&parts, 1)?,
                MouseButton::Middle,
                Modifiers::default(),
                1,
            )
            .await?
        }
        "drag" => {
            let from = at(&parts, 1)?;
            let to = at(&parts, 3)?;
            let m = Modifiers::default();
            input(
                window,
                cx,
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from,
                    pressed_button: None,
                    modifiers: m,
                }),
            )?;
            input(
                window,
                cx,
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: m,
                    click_count: 1,
                    first_mouse: false,
                }),
            )?;
            const STEPS: usize = 12;
            for step in 1..=STEPS {
                let t = step as f32 / STEPS as f32;
                let position = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                pause(cx, 20).await;
                input(
                    window,
                    cx,
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: m,
                    }),
                )?;
            }
            pause(cx, 60).await;
            input(
                window,
                cx,
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers: m,
                    click_count: 1,
                }),
            )?;
        }
        "scroll" => input(
            window,
            cx,
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: at(&parts, 1)?,
                delta: ScrollDelta::Pixels(point(px(0.0), px(num(&parts, 3)?))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            }),
        )?,
        "key" => {
            let combo = parts
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("missing combo"))?;
            let keystroke = Keystroke::parse(combo)?;
            cx.update_window(window, |_, window, cx| {
                window.dispatch_keystroke(keystroke, cx);
                window.refresh();
            })?;
        }
        "type" => {
            // `dispatch_keystroke` simulates IME input for plain keys, so
            // each character lands in the focused text field.
            let text = line
                .split_once(' ')
                .map_or("", |(_, rest)| rest)
                .to_string();
            for ch in text.chars() {
                let combo = match ch {
                    ' ' => "space".to_string(),
                    c if c.is_ascii_uppercase() => format!("shift-{}", c.to_ascii_lowercase()),
                    c => c.to_string(),
                };
                let keystroke = Keystroke::parse(&combo)?;
                cx.update_window(window, |_, window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })?;
                pause(cx, 10).await;
            }
            cx.update_window(window, |_, window, _| window.refresh())?;
        }
        "action" => {
            let name = parts
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("missing action"))?;
            cx.update_window(window, |_, window, cx| -> anyhow::Result<()> {
                let action = cx.build_action(name, None)?;
                window.dispatch_action(action, cx);
                Ok(())
            })??;
        }
        "size" => {
            let target = size(px(num(&parts, 1)?), px(num(&parts, 2)?));
            cx.update_window(window, |_, window, _| window.resize(target))?;
        }
        other => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}
