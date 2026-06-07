use std::time::Instant;

use chrono::Local;

use crate::server::console_logs::shared_console_log_buffer;

/// Structured request/response logging matching 9router's logger.js.
///
/// Logs are printed to stderr with emoji icons and timing:
///   `[HH:MM:SS] 📥 POST /v1/messages model=...`
///   `[HH:MM:SS] 📤 200 (1234ms) POST /v1/messages`
///   `[HH:MM:SS] 💥 404 (42ms) POST /v1/messages`
///   `[HH:MM:SS] 🌊 [STREAM] event.type ...`
///
/// Logs are also broadcast to the SSE console-log stream so the dashboard
/// at `/dashboard/console-log` shows them in real time.

fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(n) = chars.next() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn log_both(terminal_line: &str) {
    eprintln!("{}", terminal_line);
    let clean = strip_ansi(terminal_line);
    shared_console_log_buffer().append_line_blocking(clean);
}

pub struct RequestLog {
    method: &'static str,
    path: String,
    start: Instant,
}

impl RequestLog {
    pub fn start(method: &'static str, path: &str, model: Option<&str>) -> Self {
        let time = Local::now().format("%H:%M:%S");
        match model {
            Some(m) => log_both(&format!(
                "\x1b[36m[{}] 📥 {} {} model={}\x1b[0m",
                time, method, path, m
            )),
            None => log_both(&format!("\x1b[36m[{}] 📥 {} {}\x1b[0m", time, method, path)),
        }
        Self {
            method,
            path: path.to_owned(),
            start: Instant::now(),
        }
    }

    pub fn finish(self, status: u16) {
        let elapsed = self.start.elapsed().as_millis() as u64;
        let icon = if status < 400 { "📤" } else { "💥" };
        let time = Local::now().format("%H:%M:%S");
        log_both(&format!(
            "[{}] {} {} ({}ms) {} {}",
            time, icon, status, elapsed, self.method, self.path
        ));
    }
}

pub fn stream(event: &str, data: Option<&str>) {
    let time = Local::now().format("%H:%M:%S");
    match data {
        Some(d) => log_both(&format!("[{}] 🌊 [STREAM] {} {}", time, event, d)),
        None => log_both(&format!("[{}] 🌊 [STREAM] {}", time, event)),
    }
}
