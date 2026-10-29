//! Launch-failure reporting.
//!
//! The launch script used to be fire-and-forget: the portal spawned it and
//! never looked at it again, so when Jupyter died on startup the user saw a
//! button stuck on "launching ..." and nothing else (and the dead child stayed
//! around as a zombie). This module watches the child, keeps the last lines it
//! printed, turns a failure into a human-readable report shown in a window,
//! and can e-mail that report to the admin through the local sendmail.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

pub const ADMIN_EMAIL: &str = "j35@ornl.gov";
const SENDMAIL: &str = "/usr/sbin/sendmail";
/// How many trailing output lines of the launch script are kept.
const MAX_OUTPUT_LINES: usize = 150;
/// A non-zero exit after this many seconds is a session that ended, not a
/// launch failure: the error window is only shown for early deaths.
pub const EARLY_FAILURE_SECS: u64 = 120;

/// What the user was trying to launch; copied into the report.
#[derive(Clone)]
pub struct LaunchContext {
    pub instrument: String,
    pub entry: String,
    pub destination: String,
    pub command: String,
}

/// How a watched child finished.
#[derive(Clone, Copy, Debug)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

/// A launch in flight: the script's combined stdout/stderr accumulates in
/// `output`, and `exit_rx` fires exactly once when the child is gone.
pub struct LaunchWatch {
    pub context: LaunchContext,
    pub started: Instant,
    output: Arc<Mutex<Vec<String>>>,
    exit_rx: mpsc::Receiver<Exit>,
}

fn push_line(buf: &Arc<Mutex<Vec<String>>>, line: String) {
    if let Ok(mut v) = buf.lock() {
        if v.len() >= MAX_OUTPUT_LINES {
            v.remove(0);
        }
        v.push(line);
    }
}

fn pump<R: std::io::Read + Send + 'static>(
    reader: R,
    buf: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            push_line(&buf, line);
        }
    })
}

/// Spawn `script dest`, capturing its output, and watch it until it exits.
pub fn spawn_watched(script: &str, dest: &std::path::Path, context: LaunchContext) -> Result<LaunchWatch, String> {
    let mut child: Child = Command::new(script)
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {script}: {e}"))?;

    let output = Arc::new(Mutex::new(Vec::new()));
    let pumps: Vec<_> = [
        child.stdout.take().map(|r| pump(r, Arc::clone(&output))),
        child.stderr.take().map(|r| pump(r, Arc::clone(&output))),
    ]
    .into_iter()
    .flatten()
    .collect();

    let (tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        // Reaps the child, so no zombie is left behind.
        let exit = match child.wait() {
            Ok(st) => match (st.code(), st.signal()) {
                (Some(c), _) => Exit::Code(c),
                (None, Some(s)) => Exit::Signal(s),
                (None, None) => Exit::Code(-1),
            },
            Err(_) => Exit::Code(-1),
        };
        for p in pumps {
            let _ = p.join();
        }
        let _ = tx.send(exit);
    });

    Ok(LaunchWatch {
        context,
        started: Instant::now(),
        output,
        exit_rx,
    })
}

impl LaunchWatch {
    /// `Some(exit)` once the child has exited; `None` while it still runs.
    pub fn poll(&self) -> Option<Exit> {
        self.exit_rx.try_recv().ok()
    }

    pub fn output(&self) -> Vec<String> {
        self.output.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown-host".to_string())
}

fn user_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown-user".to_string())
}

/// Full name from the password database (GECOS field), if any.
fn full_name() -> Option<String> {
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() {
        return None;
    }
    let gecos = unsafe { (*pw).pw_gecos };
    if gecos.is_null() {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(gecos) }
        .to_string_lossy()
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    (!s.is_empty()).then_some(s)
}

fn now_string() -> String {
    // Local time via libc so we do not pull in a date crate.
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The one-line headline for the window title / mail subject.
pub fn headline(exit: Option<Exit>) -> String {
    match exit {
        Some(Exit::Code(c)) => format!("Jupyter launch failed (exit code {c})"),
        Some(Exit::Signal(s)) => format!("Jupyter launch was killed (signal {s})"),
        None => "Jupyter launch failed before the script could start".to_string(),
    }
}

/// Build the report shown in the window and sent by e-mail.
pub fn build_report(
    headline: &str,
    context: Option<&LaunchContext>,
    error: Option<&str>,
    output: &[String],
) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "{headline}");
    let _ = writeln!(r);
    let _ = writeln!(r, "When      : {}", now_string());
    let user = user_name();
    match full_name() {
        Some(n) => {
            let _ = writeln!(r, "Who       : {user} ({n})");
        }
        None => {
            let _ = writeln!(r, "Who       : {user}");
        }
    }
    let _ = writeln!(r, "Host      : {}", hostname());
    let _ = writeln!(
        r,
        "Display   : {}",
        std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into())
    );
    let _ = writeln!(
        r,
        "Home      : {}",
        std::env::var("HOME").unwrap_or_else(|_| "(unset)".into())
    );
    if let Ok(exe) = std::env::current_exe() {
        let _ = writeln!(r, "Portal    : {}", exe.display());
    }
    if let Some(c) = context {
        let _ = writeln!(r, "Instrument: {}", c.instrument);
        let _ = writeln!(r, "Selection : {}", c.entry);
        let _ = writeln!(r, "Folder    : {}", c.destination);
        let _ = writeln!(r, "Command   : {}", c.command);
    }
    let _ = writeln!(
        r,
        "PATH      : {}",
        std::env::var("PATH").unwrap_or_else(|_| "(unset)".into())
    );
    if let Some(e) = error {
        let _ = writeln!(r);
        let _ = writeln!(r, "Error:");
        let _ = writeln!(r, "{e}");
    }
    let _ = writeln!(r);
    if output.is_empty() {
        let _ = writeln!(r, "The launch script printed nothing.");
    } else {
        let _ = writeln!(r, "Last {} line(s) printed by the launch script:", output.len());
        let _ = writeln!(r, "----------------------------------------------------------------");
        for line in output {
            let _ = writeln!(r, "{line}");
        }
        let _ = writeln!(r, "----------------------------------------------------------------");
    }
    r
}

/// Hand the report to the local sendmail. Returns a channel that yields the
/// outcome once, so the UI never blocks on the mail system.
pub fn send_to_admin(subject_line: &str, report: &str) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    let user = user_name();
    let host = hostname();
    let subject = format!("[imaging notebooks portal] {subject_line} - {user}@{host}");
    let body = format!(
        "To: {ADMIN_EMAIL}\nFrom: {user}@{host}\nSubject: {subject}\n\
         Content-Type: text/plain; charset=utf-8\n\n{report}\n"
    );
    thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut child = Command::new(SENDMAIL)
                .arg("-t")
                .arg("-i")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("cannot run {SENDMAIL}: {e}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(body.as_bytes())
                    .map_err(|e| format!("writing to sendmail: {e}"))?;
            }
            let out = child
                .wait_with_output()
                .map_err(|e| format!("waiting for sendmail: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(format!(
                    "sendmail exited with {}{}",
                    out.status,
                    if err.is_empty() { String::new() } else { format!(": {err}") }
                ))
            }
        })();
        let _ = tx.send(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ctx() -> LaunchContext {
        LaunchContext {
            instrument: "VENUS".into(),
            entry: "HOME".into(),
            destination: "/tmp/x".into(),
            command: "sh /tmp/x".into(),
        }
    }

    fn wait_exit(w: &LaunchWatch) -> Exit {
        for _ in 0..100 {
            if let Some(e) = w.poll() {
                return e;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("child did not exit");
    }

    #[test]
    fn captures_output_and_exit_code() {
        let dir = std::env::temp_dir().join(format!("portal_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fail.sh");
        std::fs::write(&script, "#!/bin/sh\necho out line\necho err line >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let w = spawn_watched(script.to_str().unwrap(), &dir, ctx()).unwrap();
        assert!(matches!(wait_exit(&w), Exit::Code(3)));
        let out = w.output();
        assert!(out.contains(&"out line".to_string()), "{out:?}");
        assert!(out.contains(&"err line".to_string()), "{out:?}");

        let report = build_report(&headline(Some(Exit::Code(3))), Some(&w.context), None, &out);
        assert!(report.contains("exit code 3"));
        assert!(report.contains("Who       :"));
        assert!(report.contains("Host      :"));
        assert!(report.contains("Folder    : /tmp/x"));
        assert!(report.contains("err line"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_script_is_a_spawn_error() {
        let r = spawn_watched("/nonexistent/launch", std::path::Path::new("/tmp"), ctx());
        assert!(r.is_err());
        let report = build_report(&headline(None), Some(&ctx()), r.err().as_deref(), &[]);
        assert!(report.contains("before the script could start"));
        assert!(report.contains("printed nothing"));
    }
}
