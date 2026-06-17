// knock IPC — single-window queue across multiple agent sessions.
//
// Every `knock annotate|ask` (and hook) invocation is a *client*: it hands its
// request to a long-lived *daemon* over a local socket and blocks for the
// decision. The daemon owns one window and shows a list of pending requests, so N
// concurrent sessions queue into a single window instead of stacking N windows.
//
// Cross-platform via `interprocess`: a Unix-domain socket on macOS/Linux, a named
// pipe on Windows. If the daemon can't be reached/spawned, the client returns
// `None` and the caller falls back to the legacy single-shot window. A gate must
// never fail open.

#![allow(dead_code)]

use std::io::{prelude::*, BufReader};

#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::{prelude::*, ListenerOptions, Name, Stream};
use serde_json::Value;

#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Per-user socket path (Unix filesystem) — also used for cleanup.
#[cfg(unix)]
fn unix_sock_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/knock-{}.sock", dir, current_uid())
}

/// Resolve the local socket name for this platform.
fn pipe_name() -> std::io::Result<Name<'static>> {
    #[cfg(windows)]
    {
        // Windows named pipe namespace, scoped per user.
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
        format!("knock-{}.sock", user).to_ns_name::<GenericNamespaced>()
    }
    #[cfg(unix)]
    {
        unix_sock_path().to_fs_name::<GenericFilePath>()
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Send `req` to the daemon and block until a decision comes back.
/// Returns the decision JSON, or `None` if the daemon is unreachable (caller
/// should fall back to the legacy single-window flow).
pub fn client_request(req: &Value) -> Option<Value> {
    let mut stream = match connect() {
        Some(s) => s,
        None => {
            // No daemon yet — spawn one and wait for the socket to come up.
            spawn_daemon();
            wait_connect()?
        }
    };

    let line = serde_json::to_string(req).ok()? + "\n";
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;

    // Block until the daemon writes one response line (user took an action).
    let mut resp = String::new();
    let mut reader = BufReader::new(&mut stream);
    match reader.read_line(&mut resp) {
        Ok(0) => None, // daemon closed without answering
        Ok(_) => serde_json::from_str(&resp).ok(),
        Err(_) => None,
    }
}

fn connect() -> Option<Stream> {
    Stream::connect(pipe_name().ok()?).ok()
}

/// Launch the daemon as a detached background process.
fn spawn_daemon() {
    use std::process::{Command, Stdio};
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("__daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        // The child is reparented to init / detached — fine.
    }
}

/// Poll-connect to the socket for up to ~4s while the daemon starts.
fn wait_connect() -> Option<Stream> {
    for _ in 0..80 {
        if let Some(s) = connect() {
            return Some(s);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

// ---------------------------------------------------------------------------
// Daemon listener
// ---------------------------------------------------------------------------

/// One in-flight request plus the means to answer it exactly once.
pub struct Incoming {
    pub payload: Value,
    pub responder: Responder,
}

/// Holds the client's stream so the daemon can write a single decision back.
pub struct Responder {
    stream: Stream,
}

impl Responder {
    /// Send the decision JSON back to the waiting client and close.
    pub fn reply(mut self, decision: &Value) {
        let line = decision.to_string() + "\n";
        let _ = self.stream.write_all(line.as_bytes());
        let _ = self.stream.flush();
        // Dropping the stream closes it.
    }
}

/// Bind the daemon socket and accept connections forever, invoking `on_request`
/// for each. Returns an error if the socket is already bound (another daemon).
pub fn serve<F>(on_request: F) -> std::io::Result<()>
where
    F: Fn(Incoming) + Send + 'static,
{
    let listener = ListenerOptions::new()
        .name(pipe_name()?)
        .try_overwrite(true) // replace a stale Unix socket file automatically
        .create_sync()?;

    for conn in listener.incoming() {
        let mut stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Read exactly one request line, then hand the stream to the responder.
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&mut stream);
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
        }
        let payload: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        on_request(Incoming {
            payload,
            responder: Responder { stream },
        });
    }
    Ok(())
}

/// Remove the socket file (daemon shutdown / cleanup). No-op on Windows
/// (named pipes are reclaimed by the OS when the last handle closes).
pub fn cleanup() {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(unix_sock_path());
    }
}
