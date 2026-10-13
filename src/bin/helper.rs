//! Privileged helper (caerus pattern): spawned once via pkexec and kept alive
//! for the session, speaking a line-oriented protocol over stdin/stdout.
//! Zero external crates, no GTK, so it stays small and auditable.
//!
//! Request:  "<BACKEND> <ACTION> [ARG]..." (whitespace-tokenized; every arg is pre-validated by
//! the caller to contain no whitespace of its own, so this split is lossless)
//! Response: "OK <len>\n<len bytes>" or "ERROR <len>\n<len bytes>"
//! "QUIT" replies "OK 0\n" and exits.

use std::io::{self, BufRead, Write};

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn assert_root() {
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

fn handle_line(line: &str) {
    let mut parts = line.split_whitespace();
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
    let args: Vec<&str> = parts.collect();
    match backend.execute_system(action, &args) {
        Ok(text) => respond("OK", &text),
        Err(error) => respond("ERROR", &error),
    }
}

fn main() {
    assert_root();
    println!("READY");
    let _ = io::stdout().flush();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim_end();
        if line == "QUIT" {
            respond("OK", "");
            break;
        }
        handle_line(line);
    }
}
