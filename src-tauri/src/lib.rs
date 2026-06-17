// knock — desktop approval / annotation / question gate for AI agents.
//
// Modes:
//   knock annotate <file.md> [--gate] [--json] [--title T]   markdown approval / annotation
//   knock ask <questions.json>                                AskUserQuestion-style
//   knock            (no args)                                Claude Code hook mode:
//                                                             reads a PermissionRequest
//                                                             payload on stdin, shows the
//                                                             plan, returns allow/deny JSON.

use std::io::{Read, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, UserAttentionType, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

mod ipc;

use std::sync::{Arc, Mutex};

/// Wrap `generate_context!` in one place — calling the macro twice (single-shot
/// + daemon builders) would define the `_EMBED_INFO_PLIST` symbol twice.
fn build_context() -> tauri::Context {
    tauri::generate_context!()
}

#[derive(Parser, Debug)]
#[command(
    name = "knock",
    version,
    about = "Desktop approval / question gate for AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Display a markdown file for approval / annotation.
    Annotate {
        file: PathBuf,
        #[arg(long)]
        gate: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        title: Option<String>,
        /// Require Touch ID / Windows Hello to approve (falls back to a button if unavailable).
        #[arg(long)]
        touch_id: bool,
    },
    /// Ask a multiple-choice question (AskUserQuestion schema). Always emits JSON.
    Ask {
        file: PathBuf,
        #[arg(long)]
        title: Option<String>,
    },
    /// Open the settings window (toggle Touch ID requirement, etc.).
    Settings,
}

fn config_path() -> PathBuf {
    let base = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(base).join(".config/knock/config.json")
}

fn read_config() -> Value {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn config_touch_id() -> bool {
    read_config()
        .get("touch_id")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

enum Mode {
    Annotate {
        html: String,
        title: String,
        gate: bool,
    },
    Ask {
        questions: Value,
        title: String,
    },
    Settings,
}

struct AppState {
    mode: Mode,
    json: bool,
    /// hook mode: emit Claude Code PermissionRequest decision JSON instead of plain/contract output.
    hook: bool,
    /// require biometric (Touch ID / Windows Hello) for the approve action.
    touch_id: bool,
}

fn render_md(md: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(md, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn print_and_exit(line: String) -> ! {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", line);
    let _ = lock.flush();
    std::process::exit(0);
}

fn print_nothing_and_exit() -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// hook-mode decision -> Claude Code PermissionRequest output schema:
///   {"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow|deny","message":...}}}
fn hook_decision_json(decision: &str, feedback: Option<&str>) -> Value {
    let inner = match decision {
        "approved" => serde_json::json!({
            "hookEventName": "PermissionRequest",
            "decision": { "behavior": "allow" }
        }),
        "annotated" => serde_json::json!({
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "deny",
                "message": feedback.unwrap_or("User requested changes via knock")
            }
        }),
        // dismissed
        _ => serde_json::json!({
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "deny",
                "message": "User dismissed the plan review in knock"
            }
        }),
    };
    serde_json::json!({ "hookSpecificOutput": inner })
}

fn output_hook(decision: &str, feedback: Option<&str>) -> ! {
    print_and_exit(hook_decision_json(decision, feedback).to_string());
}

/// Extract the plan markdown from a PermissionRequest payload.
/// tool_input.plan is the real ExitPlanMode field; context.plan is a fallback.
fn extract_plan(payload: &Value) -> &str {
    payload
        .get("tool_input")
        .and_then(|t| t.get("plan"))
        .and_then(|p| p.as_str())
        .or_else(|| {
            payload
                .get("context")
                .and_then(|c| c.get("plan"))
                .and_then(|p| p.as_str())
        })
        .unwrap_or("")
}

/// annotate-mode decision -> stdout contract line, or None = print nothing.
fn annotate_contract(decision: &str, feedback: Option<&str>, json: bool) -> Option<String> {
    match decision {
        "approved" => Some(if json {
            serde_json::json!({ "decision": "approved" }).to_string()
        } else {
            "The user approved.".to_string()
        }),
        "annotated" => {
            let fb = feedback.unwrap_or("").trim().to_string();
            Some(if json {
                serde_json::json!({ "decision": "annotated", "feedback": fb }).to_string()
            } else {
                fb
            })
        }
        _ => {
            if json {
                Some(serde_json::json!({ "decision": "dismissed" }).to_string())
            } else {
                None
            }
        }
    }
}

fn output_annotate(decision: &str, feedback: Option<&str>, json: bool) -> ! {
    match annotate_contract(decision, feedback, json) {
        Some(line) => print_and_exit(line),
        None => print_nothing_and_exit(),
    }
}

/// Route a finished decision to the right output for the current mode.
fn finish(decision: &str, feedback: Option<&str>, state: &AppState) -> ! {
    if state.hook {
        output_hook(decision, feedback);
    }
    match &state.mode {
        Mode::Annotate { .. } => output_annotate(decision, feedback, state.json),
        Mode::Ask { .. } => {
            print_and_exit(serde_json::json!({ "decision": "dismissed" }).to_string())
        }
        Mode::Settings => print_nothing_and_exit(),
    }
}

#[tauri::command]
fn get_payload(state: tauri::State<AppState>) -> Value {
    match &state.mode {
        Mode::Annotate { html, title, gate } => serde_json::json!({
            "mode": "annotate",
            "html": html,
            "title": title,
            "gate": gate,
            "touchId": state.touch_id,
        }),
        Mode::Ask { questions, title } => serde_json::json!({
            "mode": "ask",
            "questions": questions,
            "title": title,
        }),
        Mode::Settings => serde_json::json!({
            "mode": "settings",
            "touchId": config_touch_id(),
        }),
    }
}

/// Persist the Touch ID requirement toggle to the config file.
fn set_config_touch_id(enabled: bool) -> Result<(), String> {
    let mut cfg = read_config();
    cfg["touch_id"] = serde_json::json!(enabled);
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_touch_id(enabled: bool) -> Result<(), String> {
    set_config_touch_id(enabled)
}

#[tauri::command]
fn submit(decision: String, feedback: Option<String>, state: tauri::State<AppState>) {
    if state.hook {
        output_hook(&decision, feedback.as_deref());
    }
    output_annotate(&decision, feedback.as_deref(), state.json);
}

#[tauri::command]
fn submit_answers(answers: Value) {
    print_and_exit(serde_json::json!({ "answers": answers }).to_string());
}

/// Save a pasted image (data: URL) to a temp file and return its path.
#[tauri::command]
fn save_pasted_image(data_url: String) -> Result<String, String> {
    use base64::Engine;
    let b64 = data_url
        .split(',')
        .nth(1)
        .ok_or_else(|| "invalid data url".to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("knock-paste-{}.png", ts));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Biometric approval (macOS Touch ID / Windows Hello) via robius-authentication.
/// Returns true if the user authenticated, false otherwise (incl. no hardware).
#[tauri::command]
fn touch_id_approve() -> bool {
    use robius_authentication::{
        AndroidText, BiometricStrength, Context, PolicyBuilder, Text, WindowsText,
    };
    let policy = match PolicyBuilder::new()
        .biometrics(Some(BiometricStrength::Strong))
        .password(true)
        .build()
    {
        Some(p) => p,
        None => return false,
    };
    let windows = match WindowsText::new("Knock", "Approve the request") {
        Some(w) => w,
        None => return false,
    };
    let text = Text {
        android: AndroidText {
            title: "Knock",
            subtitle: None,
            description: None,
        },
        apple: "approve the knock request",
        windows,
    };
    Context::new(())
        .blocking_authenticate(text, &policy)
        .is_ok()
}

#[tauri::command]
fn dismiss(state: tauri::State<AppState>) {
    finish("dismissed", None, &state);
}

// ---------------------------------------------------------------------------
// Daemon mode: one window, a queue of requests from many client invocations.
// ---------------------------------------------------------------------------

#[cfg(unix)]
struct QueueEntry {
    id: String,
    kind: String,
    payload: Value,
    responder: ipc::Responder,
}

#[cfg(unix)]
struct DaemonState {
    queue: Arc<Mutex<Vec<QueueEntry>>>,
}

/// The pending queue, shaped for the frontend list (no responders).
#[cfg(unix)]
#[tauri::command]
fn daemon_queue(state: tauri::State<DaemonState>) -> Value {
    let q = state.queue.lock().unwrap();
    let items: Vec<Value> = q
        .iter()
        .map(|e| {
            let title = e
                .payload
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("Knock")
                .to_string();
            serde_json::json!({
                "id": e.id,
                "kind": e.kind,
                "title": title,
                "payload": e.payload,
            })
        })
        .collect();
    serde_json::json!({ "mode": "queue", "items": items, "touchId": config_touch_id() })
}

/// Resolve one queued request: reply to its client and drop it from the queue.
#[cfg(unix)]
#[tauri::command]
fn daemon_resolve(
    app: tauri::AppHandle,
    id: String,
    decision: String,
    feedback: Option<String>,
    answers: Option<Value>,
    state: tauri::State<DaemonState>,
) {
    let entry = {
        let mut q = state.queue.lock().unwrap();
        q.iter().position(|e| e.id == id).map(|pos| q.remove(pos))
    };
    if let Some(entry) = entry {
        let mut resp = serde_json::json!({ "decision": decision });
        if let Some(f) = feedback {
            resp["feedback"] = Value::String(f);
        }
        if let Some(a) = answers {
            resp["answers"] = a;
        }
        entry.responder.reply(&resp);
    }
    let empty = state.queue.lock().unwrap().is_empty();
    let _ = app.emit("queue-changed", ());
    if empty {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }
}

#[cfg(unix)]
fn run_daemon() {
    let queue: Arc<Mutex<Vec<QueueEntry>>> = Arc::new(Mutex::new(Vec::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(DaemonState {
            queue: queue.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            daemon_queue,
            daemon_resolve,
            save_pasted_image,
            touch_id_approve,
            save_touch_id
        ])
        .on_window_event(|window, event| {
            // Closing the window must not kill the daemon — just hide it.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            let sc = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyK);
            let _ = app
                .global_shortcut()
                .on_shortcut(sc, |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.unminimize();
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                });

            // Start hidden; the window appears when the first request arrives.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(true);
                let _ = win.hide();
            }

            // Socket listener: push each incoming request and wake the window.
            let handle = app.handle().clone();
            let q = queue.clone();
            std::thread::spawn(move || {
                let h = handle.clone();
                let served = ipc::serve(move |incoming| {
                    let payload = incoming.payload;
                    let id = payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let kind = payload
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("annotate")
                        .to_string();
                    let inner = payload
                        .get("payload")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    {
                        let mut qq = q.lock().unwrap();
                        qq.push(QueueEntry {
                            id,
                            kind,
                            payload: inner,
                            responder: incoming.responder,
                        });
                    }
                    if let Some(win) = h.get_webview_window("main") {
                        let _ = win.unminimize();
                        let _ = win.show();
                        let _ = win.set_focus();
                        let _ = win.request_user_attention(Some(UserAttentionType::Critical));
                    }
                    let _ = h
                        .notification()
                        .builder()
                        .title("Knock — 새 승인 요청")
                        .body("대기 중인 요청이 있습니다")
                        .show();
                    let _ = h.emit("queue-changed", ());
                });
                // serve() only returns on bind failure (another daemon already runs).
                if served.is_err() {
                    std::process::exit(0);
                }
            });

            // Tray with the Touch ID toggle (same as single-shot mode).
            let info = MenuItemBuilder::with_id(
                "info",
                format!("Knock v{} (daemon)", env!("CARGO_PKG_VERSION")),
            )
            .enabled(false)
            .build(app)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let touch_toggle =
                CheckMenuItemBuilder::with_id("touch_id", "Touch ID for critical gates")
                    .checked(config_touch_id())
                    .build(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = MenuItemBuilder::with_id("quit", "종료 (Quit daemon)").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&info, &sep, &touch_toggle, &sep2, &quit])
                .build()?;
            let toggle_handle = touch_toggle.clone();
            if let Some(icon) = app.default_window_icon().cloned() {
                let _ = TrayIconBuilder::with_id("knock")
                    .icon(icon)
                    .tooltip("Knock daemon")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| match event.id().as_ref() {
                        "quit" => {
                            ipc::cleanup();
                            app.exit(0);
                        }
                        "touch_id" => {
                            let next = !config_touch_id();
                            let _ = set_config_touch_id(next);
                            let _ = toggle_handle.set_checked(next);
                        }
                        _ => {}
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .run(build_context())
        .expect("error while running knock daemon");

    ipc::cleanup();
}

fn launch(state: AppState) {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_payload,
            submit,
            submit_answers,
            save_pasted_image,
            touch_id_approve,
            save_touch_id,
            dismiss
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                finish("dismissed", None, &state);
            }
        })
        .setup(|app| {
            let sc = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyK);
            let _ = app
                .global_shortcut()
                .on_shortcut(sc, |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.unminimize();
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                });

            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(true);
                let _ = win.set_focus();
                let _ = win.request_user_attention(Some(UserAttentionType::Critical));
            }

            let (heading, title) = match &app.state::<AppState>().mode {
                Mode::Annotate { title, .. } => ("Knock — 승인 요청", title.clone()),
                Mode::Ask { title, .. } => ("Knock — 확인 필요", title.clone()),
                Mode::Settings => ("Knock — 설정", "설정".to_string()),
            };
            let _ = app
                .notification()
                .builder()
                .title(heading)
                .body(&title)
                .show();

            let info =
                MenuItemBuilder::with_id("info", format!("Knock v{}", env!("CARGO_PKG_VERSION")))
                    .enabled(false)
                    .build(app)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let touch_toggle =
                CheckMenuItemBuilder::with_id("touch_id", "Touch ID for critical gates")
                    .checked(config_touch_id())
                    .build(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = MenuItemBuilder::with_id("quit", "닫기 (Quit)").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&info, &sep, &touch_toggle, &sep2, &quit])
                .build()?;
            let toggle_handle = touch_toggle.clone();
            if let Some(icon) = app.default_window_icon().cloned() {
                let _ = TrayIconBuilder::with_id("knock")
                    .icon(icon)
                    .tooltip("Knock — 응답 대기 중")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| match event.id().as_ref() {
                        "quit" => {
                            let state = app.state::<AppState>();
                            finish("dismissed", None, &state);
                        }
                        "touch_id" => {
                            let next = !config_touch_id();
                            let _ = set_config_touch_id(next);
                            let _ = toggle_handle.set_checked(next);
                        }
                        _ => {}
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .run(build_context())
        .expect("error while running knock");
}

/// Try to hand this request to the daemon (single-window queue). If the daemon
/// answers, convert the decision to this invocation's stdout contract and exit.
/// Returns normally only if the daemon is unreachable (caller falls back to launch).
#[cfg(unix)]
fn try_daemon(kind: &str, inner: Value, json: bool, hook: bool) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{}-{}", std::process::id(), ts);
    let req = serde_json::json!({
        "id": id,
        "kind": kind,
        "json": json,
        "hook": hook,
        "payload": inner,
    });
    if let Some(resp) = ipc::client_request(&req) {
        let decision = resp
            .get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or("dismissed");
        let feedback = resp.get("feedback").and_then(|v| v.as_str());
        if kind == "ask" {
            match resp.get("answers") {
                Some(ans) => print_and_exit(serde_json::json!({ "answers": ans }).to_string()),
                None => print_and_exit(serde_json::json!({ "decision": "dismissed" }).to_string()),
            }
        }
        if hook {
            output_hook(decision, feedback);
        }
        output_annotate(decision, feedback, json);
    }
    // daemon unreachable → caller falls back to the single-shot window
}

#[cfg(not(unix))]
fn try_daemon(_kind: &str, _inner: Value, _json: bool, _hook: bool) {}

/// Hook mode: read a Claude Code PermissionRequest payload on stdin, show the plan,
/// and emit an allow/deny decision. Never blocks if there's no plan to review.
fn run_hook() {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);

    // Fail-safe: a malformed payload must NOT auto-approve. Emit nothing and let
    // Claude Code's normal permission flow handle it (a gate should never fail open).
    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("knock hook: could not parse stdin payload: {}", e);
            std::process::exit(0);
        }
    };

    let plan = extract_plan(&payload);

    if plan.trim().is_empty() {
        // No plan to review (not an ExitPlanMode request) — stay out of the way.
        std::process::exit(0);
    }

    let html = render_md(plan);
    let inner = serde_json::json!({
        "mode": "annotate",
        "html": html.clone(),
        "title": "Plan 검토",
        "gate": true,
        "touchId": false,
    });
    try_daemon("annotate", inner, false, true);

    launch(AppState {
        mode: Mode::Annotate {
            html,
            title: "Plan 검토".to_string(),
            gate: true,
        },
        json: false,
        hook: true,
        touch_id: false,
    });
}

pub fn run() {
    let argv: Vec<String> = std::env::args().collect();
    // No subcommand → Claude Code hook mode (PermissionRequest payload on stdin).
    if argv.len() <= 1 {
        run_hook();
        return;
    }
    // Hidden entry: the long-lived single-window daemon (spawned by clients).
    #[cfg(unix)]
    if argv.get(1).map(|s| s.as_str()) == Some("__daemon") {
        run_daemon();
        return;
    }

    let cli = Cli::parse();

    match cli.command {
        Command::Annotate {
            file,
            gate,
            json,
            title,
            touch_id,
        } => {
            let md = std::fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("knock: cannot read {}: {}", file.display(), e);
                std::process::exit(2);
            });
            let title = title.unwrap_or_else(|| {
                file.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Knock".to_string())
            });
            let html = render_md(&md);
            let inner = serde_json::json!({
                "mode": "annotate",
                "html": html.clone(),
                "title": title.clone(),
                "gate": gate,
                "touchId": touch_id,
            });
            try_daemon("annotate", inner, json, false);
            launch(AppState {
                mode: Mode::Annotate { html, title, gate },
                json,
                hook: false,
                touch_id,
            });
        }
        Command::Ask { file, title } => {
            let raw = std::fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("knock: cannot read {}: {}", file.display(), e);
                std::process::exit(2);
            });
            let questions: Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
                eprintln!("knock: invalid question JSON in {}: {}", file.display(), e);
                std::process::exit(2);
            });
            let title = title
                .or_else(|| {
                    questions
                        .get("questions")
                        .and_then(|q| q.get(0))
                        .and_then(|q0| q0.get("header"))
                        .and_then(|h| h.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "확인 필요".to_string());
            let inner = serde_json::json!({
                "mode": "ask",
                "questions": questions.clone(),
                "title": title.clone(),
            });
            try_daemon("ask", inner, true, false);
            launch(AppState {
                mode: Mode::Ask { questions, title },
                json: true,
                hook: false,
                touch_id: false,
            });
        }
        // Settings is always a single-shot window (never queued through the daemon).
        Command::Settings => launch(AppState {
            mode: Mode::Settings,
            json: false,
            hook: false,
            touch_id: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_approved_allows() {
        let v = hook_decision_json("approved", None);
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
    }

    #[test]
    fn hook_annotated_denies_with_message() {
        let v = hook_decision_json("annotated", Some("fix the KMS policy"));
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["decision"]["message"],
            "fix the KMS policy"
        );
    }

    #[test]
    fn hook_dismissed_denies() {
        let v = hook_decision_json("dismissed", None);
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }

    #[test]
    fn extract_plan_prefers_tool_input() {
        let p =
            serde_json::json!({ "tool_input": { "plan": "# Plan" }, "context": { "plan": "ctx" } });
        assert_eq!(extract_plan(&p), "# Plan");
    }

    #[test]
    fn extract_plan_falls_back_to_context() {
        let p = serde_json::json!({ "context": { "plan": "ctx plan" } });
        assert_eq!(extract_plan(&p), "ctx plan");
    }

    #[test]
    fn extract_plan_missing_is_empty() {
        let p = serde_json::json!({ "tool_name": "ExitPlanMode" });
        assert_eq!(extract_plan(&p), "");
    }

    #[test]
    fn annotate_approved_plain() {
        assert_eq!(
            annotate_contract("approved", None, false).unwrap(),
            "The user approved."
        );
    }

    #[test]
    fn annotate_approved_json() {
        let s = annotate_contract("approved", None, true).unwrap();
        assert!(s.contains("\"decision\":\"approved\""));
    }

    #[test]
    fn annotate_annotated_returns_feedback() {
        assert_eq!(
            annotate_contract("annotated", Some("  needs work  "), false).unwrap(),
            "needs work"
        );
    }

    #[test]
    fn annotate_dismissed_plain_is_none() {
        assert!(annotate_contract("dismissed", None, false).is_none());
    }

    #[test]
    fn annotate_dismissed_json_some() {
        let s = annotate_contract("dismissed", None, true).unwrap();
        assert!(s.contains("\"dismissed\""));
    }

    #[test]
    fn render_md_makes_table() {
        let h = render_md("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(h.contains("<table>"));
    }
}
