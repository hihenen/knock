// knock IPC — single-window queue across multiple agent sessions.
//
// Every `knock annotate|ask` (and hook) invocation is a *client*: it hands its
// request to a long-lived *daemon* over a Unix domain socket and blocks for the
// decision. The daemon owns one window and shows a list of pending requests, so N
// concurrent sessions queue into a single window instead of stacking N windows.
//
// Safety: if the daemon can't be reached/spawned, the client returns `None` and the
// caller falls back to the legacy single-shot window. A gate must never fail open.

#![allow(dead_code)]

use serde_json::Value;

/// Path of the per-user daemon socket.
#[cfg(unix)]
pub fn sock_path() -> std::path::PathBuf {
    // Prefer XDG_RUNTIME_DIR, else /tmp, keyed by uid so each user gets their own.
    let uid = libc_getuid();
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(dir).join(format!("knock-{}.sock", uid))
}

#[cfg(unix)]
fn libc_getuid() -> u32 {
    // Avoid pulling the whole libc crate for one call.
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Send `req` to the daemon and block until a decision comes back.
/// Returns the decision JSON, or `None` if the daemon is unreachable (caller
/// should fall back to the legacy single-window flow).
#[cfg(unix)]
pub fn client_request(req: &Value) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let path = sock_path();

    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            // No daemon yet — spawn one and wait for the socket to come up.
            spawn_daemon();
            wait_connect(&path)?
        }
    };

    let line = serde_json::to_string(req).ok()? + "\n";
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;

    // Block until the daemon writes one response line (user took an action).
    let mut resp = String::new();
    let mut reader = BufReader::new(stream);
    match reader.read_line(&mut resp) {
        Ok(0) => None, // daemon closed without answering
        Ok(_) => serde_json::from_str(&resp).ok(),
        Err(_) => None,
    }
}

#[cfg(not(unix))]
pub fn client_request(_req: &Value) -> Option<Value> {
    // No Unix sockets — always fall back to the single-window flow.
    None
}

/// Launch the daemon as a detached background process.
#[cfg(unix)]
fn spawn_daemon() {
    use std::process::{Command, Stdio};
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("__daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        // The child is reparented to init when this client exits — fine.
    }
}

/// Poll-connect to the socket for up to ~4s while the daemon starts.
#[cfg(unix)]
fn wait_connect(path: &std::path::Path) -> Option<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;
    for _ in 0..80 {
        if let Ok(s) = UnixStream::connect(path) {
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
#[cfg(unix)]
pub struct Incoming {
    pub payload: Value,
    pub responder: Responder,
}

/// Holds the client's stream so the daemon can write a single decision back.
#[cfg(unix)]
pub struct Responder {
    stream: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl Responder {
    /// Send the decision JSON back to the waiting client and close.
    pub fn reply(self, decision: &Value) {
        use std::io::Write;
        let mut stream = self.stream;
        let line = decision.to_string() + "\n";
        let _ = stream.write_all(line.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Bind the daemon socket and accept connections forever, invoking `on_request`
/// for each. Returns an error if the socket is already bound (another daemon).
#[cfg(unix)]
pub fn serve<F>(on_request: F) -> std::io::Result<()>
where
    F: Fn(Incoming) + Send + 'static,
{
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    let path = sock_path();

    // If a stale socket exists but nothing is listening, connecting fails — clear it.
    if path.exists() && std::os::unix::net::UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;

    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        let read_stream = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Read exactly one request line.
        let mut line = String::new();
        let mut reader = BufReader::new(read_stream);
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            continue;
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

/// Remove the socket file (daemon shutdown / cleanup).
#[cfg(unix)]
pub fn cleanup() {
    let _ = std::fs::remove_file(sock_path());
}

#[cfg(not(unix))]
pub fn cleanup() {}
