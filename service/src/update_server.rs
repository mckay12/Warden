//! Minimal local HTTP server (Phase 9, take 2) serving the extension update
//! manifest + `.crx` over `http://127.0.0.1` instead of `file://` — Chrome
//! silently rejected the `file://` version (see `registry_policy.rs`'s doc
//! comment). This is the standard enterprise "self-hosted update" pattern,
//! just served from the same machine instead of a real web server, which
//! keeps it consistent with the PRD's "no cloud dependency for enforcement"
//! requirement (§8) — the whole thing stays on-box.
//!
//! Hand-rolled rather than pulling in a web framework: two static GET
//! routes, no bodies to parse, no keep-alive needed. Runs for the whole
//! service lifetime (not just while a lock is active) so a fresh Chrome
//! poll can always reach it once `ExtensionSettings` references it.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::db;
use crate::extension_id::EXTENSION_ID;
use crate::logging::log_line;

pub const PORT: u16 = 47990;

/// Must match `extension/manifest.json`'s `"version"` — Chrome compares
/// this against what's already installed to decide whether to fetch the
/// `.crx` at all, so a mismatch here means updates silently never happen.
const MANIFEST_VERSION: &str = "0.1.0";

pub fn spawn() {
    thread::spawn(|| {
        let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
            Ok(l) => l,
            Err(e) => {
                log_line(&format!("update_server: bind to 127.0.0.1:{PORT} failed: {e}"));
                return;
            }
        };
        log_line(&format!("update_server: listening on 127.0.0.1:{PORT}"));
        for stream in listener.incoming().flatten() {
            handle(stream);
        }
    });
}

fn handle(mut stream: TcpStream) {
    let mut buf = [0u8; 2048];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    match path {
        "/update_manifest.xml" => {
            let body = format!(
                "<?xml version='1.0' encoding='UTF-8'?>\n\
                 <gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>\n\
                 \x20 <app appid='{EXTENSION_ID}'>\n\
                 \x20   <updatecheck codebase='http://127.0.0.1:{PORT}/extension.crx' version='{MANIFEST_VERSION}' />\n\
                 \x20 </app>\n\
                 </gupdate>\n"
            );
            respond(&mut stream, 200, "text/xml", body.as_bytes());
        }
        "/extension.crx" => match fs::read(db::db_dir().join("extension.crx")) {
            Ok(bytes) => respond(&mut stream, 200, "application/x-chrome-extension", &bytes),
            Err(_) => respond(&mut stream, 404, "text/plain", b"extension.crx not found"),
        },
        _ => respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let status_text = if status == 200 { "OK" } else { "Not Found" };
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}
