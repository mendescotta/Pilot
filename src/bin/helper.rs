//! Privileged helper (caerus pattern): spawned once via pkexec and kept alive
//! for the session, speaking a line-oriented protocol over stdin/stdout.
//! Zero external crates, no GTK, so it stays small and auditable.
//!
//! Request:  "2 <BACKEND> <ACTION> [ARG]..." (whitespace-tokenized; every arg is pre-validated by
//! the caller to contain no whitespace of its own, so this split is lossless)
//! Response: "OK <len>\n<len bytes>" or "ERROR <len>\n<len bytes>"
//! "QUIT" replies "OK 0\n" and exits.

use std::io::{self, BufRead, Read, Write};

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn assert_root() {
    // SAFETY: getuid has no arguments or memory preconditions.
    if unsafe { libc_getuid() } != 0 {
        eprintln!("pilot-helper: must run as root");
        std::process::exit(1);
    }
}

fn respond(status: &str, body: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{status} {}", body.len());
    let _ = out.write_all(body.as_bytes());
    let _ = out.flush();
}

fn handle_line(line: &str, monitor: &mut Option<pilot::events::EventMonitor>) {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("2") {
        respond(
            "ERROR",
            "Unsupported protocol version; update Pilot and its helper together",
        );
        return;
    }
    let backend = match parts.next() {
        Some("dinit") => pilot::Backend::Dinit,
        Some("runit") => pilot::Backend::Runit,
        _ => {
            respond("ERROR", "Unknown backend");
            return;
        }
    };
    let Some(action) = parts.next() else {
        respond("ERROR", "Missing action");
        return;
    };
    if pilot::SystemManagerProfile::system().backend != Some(backend) {
        respond(
            "ERROR",
            "Requested backend does not match PID 1; Pilot does not control secondary managers",
        );
        return;
    }
    let args: Vec<&str> = parts.collect();
    if action == "revision" && args.is_empty() && backend == pilot::Backend::Dinit {
        match monitor.as_mut().and_then(|m| m.revision()) {
            Some(revision) => respond("OK", &revision.to_string()),
            None => respond("ERROR", "Event monitor unavailable; polling continues"),
        };
        return;
    }
    match backend.execute_system(action, &args) {
        Ok(text) => {
            if action == "list" && backend == pilot::Backend::Dinit {
                let names = pilot::parse_list(&text)
                    .into_iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>();
                if monitor
                    .as_mut()
                    .is_none_or(|m| m.names != names || m.revision().is_none())
                {
                    *monitor = pilot::events::EventMonitor::start(names).ok();
                }
            }
            respond("OK", &text)
        }
        Err(error) => respond("ERROR", &error),
    }
}

fn main() {
    assert_root();
    println!("READY 2");
    let _ = io::stdout().flush();

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut monitor = None;
    loop {
        let mut bytes = Vec::new();
        let count = match input
            .by_ref()
            .take(70 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
        {
            Ok(n) => n,
            Err(_) => break,
        };
        if count == 0 {
            break;
        }
        if count > 70 * 1024 {
            respond("ERROR", "Request exceeds 70 KiB");
            break;
        }
        let Ok(line) = String::from_utf8(bytes) else {
            respond("ERROR", "Request is not UTF-8");
            break;
        };
        let line = line.trim_end();
        if line == "QUIT" {
            respond("OK", "");
            break;
        }
        handle_line(line, &mut monitor);
    }
}
