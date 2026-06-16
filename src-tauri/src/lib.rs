// knock — desktop approval / annotation / question gate for AI agents.
//
// Modes:
//   knock annotate <file.md> [--gate] [--json] [--title T]   plannotator-compatible
//   knock ask <questions.json>                                AskUserQuestion-style
//   knock            (no args)                                Claude Code hook mode:
//                                                             reads a PermissionRequest
//                                                             payload on stdin, shows the
//                                                             plan, returns allow/deny JSON.

use std::io::{Read, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, UserAttentionType, WindowEvent};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};
use tauri_plugin_notification::NotificationExt;

#[derive(Parser, Debug)]
#[command(name = "knock", version, about = "Desktop approval / question gate for AI agents")]
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
    },
    /// Ask a multiple-choice question (AskUserQuestion schema). Always emits JSON.
    Ask {
        file: PathBuf,
        #[arg(long)]
        title: Option<String>,
    },
}

enum Mode {
    Annotate { html: String, title: String, gate: bool },
    Ask { questions: Value, title: String },
}

struct AppState {
    mode: Mode,
    json: bool,
    /// hook mode: emit Claude Code PermissionRequest decision JSON instead of plain/contract output.
    hook: bool,
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

fn hook_allow() -> String {
    serde_json::json!({
        "decision": "allow",
        "hookSpecificOutput": { "permissionDecision": "allow" }
    })
    .to_string()
}

/// hook-mode decision -> Claude Code PermissionRequest JSON.
fn output_hook(decision: &str, feedback: Option<&str>) -> ! {
    let json = match decision {
        "approved" => serde_json::json!({
            "decision": "allow",
            "hookSpecificOutput": { "permissionDecision": "allow" }
        }),
        "annotated" => serde_json::json!({
            "decision": "deny",
            "reason": feedback.unwrap_or("User requested changes via knock"),
            "hookSpecificOutput": { "permissionDecision": "deny" }
        }),
        // dismissed
        _ => serde_json::json!({
            "decision": "deny",
            "reason": "User dismissed the plan review in knock",
            "hookSpecificOutput": { "permissionDecision": "deny" }
        }),
    };
    print_and_exit(json.to_string());
}

/// annotate-mode decision -> plannotator contract.
fn output_annotate(decision: &str, feedback: Option<&str>, json: bool) -> ! {
    match decision {
        "approved" => {
            if json {
                print_and_exit(serde_json::json!({ "decision": "approved" }).to_string())
            } else {
                print_and_exit("The user approved.".to_string())
            }
        }
        "annotated" => {
            let fb = feedback.unwrap_or("").trim().to_string();
            if json {
                print_and_exit(
                    serde_json::json!({ "decision": "annotated", "feedback": fb }).to_string(),
                )
            } else {
                print_and_exit(fb)
            }
        }
        _ => {
            if json {
                print_and_exit(serde_json::json!({ "decision": "dismissed" }).to_string())
            } else {
                print_nothing_and_exit()
            }
        }
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
        }),
        Mode::Ask { questions, title } => serde_json::json!({
            "mode": "ask",
            "questions": questions,
            "title": title,
        }),
    }
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

#[tauri::command]
fn dismiss(state: tauri::State<AppState>) {
    finish("dismissed", None, &state);
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
            };
            let _ = app
                .notification()
                .builder()
                .title(heading)
                .body(&title)
                .show();

            let info = MenuItemBuilder::with_id("info", format!("Knock v{}", env!("CARGO_PKG_VERSION")))
                .enabled(false)
                .build(app)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItemBuilder::with_id("quit", "닫기 (Quit)").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&info, &sep, &quit]).build()?;
            if let Some(icon) = app.default_window_icon().cloned() {
                let _ = TrayIconBuilder::with_id("knock")
                    .icon(icon)
                    .tooltip("Knock — 응답 대기 중")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| {
                        if event.id().as_ref() == "quit" {
                            let state = app.state::<AppState>();
                            finish("dismissed", None, &state);
                        }
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running knock");
}

/// Hook mode: read a Claude Code PermissionRequest payload on stdin, show the plan,
/// and emit an allow/deny decision. Never blocks if there's no plan to review.
fn run_hook() {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let payload: Value = serde_json::from_str(&buf).unwrap_or(Value::Null);

    let plan = payload
        .get("context")
        .and_then(|c| c.get("plan"))
        .and_then(|p| p.as_str())
        .or_else(|| {
            payload
                .get("tool_input")
                .and_then(|t| t.get("plan"))
                .and_then(|p| p.as_str())
        })
        .unwrap_or("");

    if plan.trim().is_empty() {
        // Nothing to review — let the permission flow proceed normally.
        print_and_exit(hook_allow());
    }

    launch(AppState {
        mode: Mode::Annotate {
            html: render_md(plan),
            title: "Plan 검토".to_string(),
            gate: true,
        },
        json: false,
        hook: true,
    });
}

pub fn run() {
    let argv: Vec<String> = std::env::args().collect();
    // No subcommand → Claude Code hook mode (PermissionRequest payload on stdin).
    if argv.len() <= 1 {
        run_hook();
        return;
    }

    let cli = Cli::parse();

    let (mode, json) = match cli.command {
        Command::Annotate {
            file,
            gate,
            json,
            title,
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
            (
                Mode::Annotate {
                    html: render_md(&md),
                    title,
                    gate,
                },
                json,
            )
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
            (Mode::Ask { questions, title }, true)
        }
    };

    launch(AppState {
        mode,
        json,
        hook: false,
    });
}
