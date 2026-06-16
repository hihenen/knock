// knock — desktop approval / annotation / question gate for AI agents.
//
// Subcommands:
//   knock annotate <file.md> [--gate] [--json] [--title T]
//     plannotator-compatible stdout contract:
//       approved  -> "The user approved."      (or {"decision":"approved"} with --json)
//       dismissed -> ""                         (or {"decision":"dismissed"} with --json)
//       annotated -> <feedback text>            (or {"decision":"annotated","feedback":...} with --json)
//
//   knock ask <questions.json>
//     AskUserQuestion-shaped input; always emits JSON:
//       answered  -> {"answers": { "<header>": "<label>" | ["<label>", ...] }}
//       dismissed -> {"decision":"dismissed"}

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;
use tauri::{Manager, UserAttentionType, WindowEvent};
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
        /// Markdown file to display for review.
        file: PathBuf,
        /// Show an explicit Approve button (gate mode).
        #[arg(long)]
        gate: bool,
        /// Emit the decision as a JSON object instead of plain text.
        #[arg(long)]
        json: bool,
        /// Override the header title (defaults to the file name).
        #[arg(long)]
        title: Option<String>,
    },
    /// Ask a multiple-choice question (AskUserQuestion schema). Always emits JSON.
    Ask {
        /// JSON file with AskUserQuestion-style questions.
        file: PathBuf,
        /// Override the header title.
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
    output_annotate(&decision, feedback.as_deref(), state.json);
}

#[tauri::command]
fn submit_answers(answers: Value) {
    print_and_exit(serde_json::json!({ "answers": answers }).to_string());
}

#[tauri::command]
fn dismiss(state: tauri::State<AppState>) {
    match &state.mode {
        Mode::Annotate { .. } => output_annotate("dismissed", None, state.json),
        Mode::Ask { .. } => {
            print_and_exit(serde_json::json!({ "decision": "dismissed" }).to_string())
        }
    }
}

pub fn run() {
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
            // ask mode is always structured JSON.
            (Mode::Ask { questions, title }, true)
        }
    };

    let state = AppState { mode, json };

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_payload,
            submit,
            submit_answers,
            dismiss
        ])
        .on_window_event(|window, event| {
            // Closing the window without a decision == dismissed.
            if let WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                match &state.mode {
                    Mode::Annotate { .. } => output_annotate("dismissed", None, state.json),
                    Mode::Ask { .. } => print_and_exit(
                        serde_json::json!({ "decision": "dismissed" }).to_string(),
                    ),
                }
            }
        })
        .setup(|app| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(true);
                let _ = win.set_focus();
                // macOS: bounce the Dock icon / flash until the user looks.
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

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running knock");
}
