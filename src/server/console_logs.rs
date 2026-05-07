use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;

pub const DEFAULT_CONSOLE_LOG_MAX_LINES: usize = 200;

static SHARED_CONSOLE_LOG_BUFFER: OnceLock<Arc<ConsoleLogBuffer>> = OnceLock::new();

#[derive(Debug, Clone)]
pub enum ConsoleLogEvent {
    Line(String),
    Clear,
}

pub struct ConsoleLogBuffer {
    logs: Mutex<Vec<String>>,
    max_lines: usize,
    sender: broadcast::Sender<ConsoleLogEvent>,
}

pub fn shared_console_log_buffer() -> Arc<ConsoleLogBuffer> {
    SHARED_CONSOLE_LOG_BUFFER
        .get_or_init(|| Arc::new(ConsoleLogBuffer::new(DEFAULT_CONSOLE_LOG_MAX_LINES)))
        .clone()
}

impl ConsoleLogBuffer {
    pub fn new(max_lines: usize) -> Self {
        let (sender, _) = broadcast::channel(128);

        Self {
            logs: Mutex::new(Vec::new()),
            max_lines: max_lines.max(1),
            sender,
        }
    }

    pub async fn get_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    pub async fn append_line(&self, line: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(line.clone());

        let overflow = logs.len().saturating_sub(self.max_lines);
        if overflow > 0 {
            logs.drain(0..overflow);
        }

        drop(logs);
        let _ = self.sender.send(ConsoleLogEvent::Line(line));
    }

    pub fn append_line_blocking(&self, line: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(line.clone());

        let overflow = logs.len().saturating_sub(self.max_lines);
        if overflow > 0 {
            logs.drain(0..overflow);
        }

        drop(logs);
        let _ = self.sender.send(ConsoleLogEvent::Line(line));
    }

    pub async fn clear(&self) {
        self.logs.lock().unwrap().clear();
        let _ = self.sender.send(ConsoleLogEvent::Clear);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConsoleLogEvent> {
        self.sender.subscribe()
    }
}

#[derive(Clone)]
pub struct ConsoleLogMakeWriter {
    buffer: Arc<ConsoleLogBuffer>,
}

impl ConsoleLogMakeWriter {
    pub fn new(buffer: Arc<ConsoleLogBuffer>) -> Self {
        Self { buffer }
    }
}

pub struct ConsoleLogWriter {
    buffer: Arc<ConsoleLogBuffer>,
    current_line: Vec<u8>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ConsoleLogMakeWriter {
    type Writer = ConsoleLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ConsoleLogWriter {
            buffer: self.buffer.clone(),
            current_line: Vec::new(),
        }
    }
}

impl Write for ConsoleLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.current_line.extend_from_slice(buf);

        while let Some(pos) = self.current_line.iter().position(|byte| *byte == b'\n') {
            let line = self.current_line.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if !line.is_empty() {
                self.buffer.append_line_blocking(line);
            }
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.current_line.is_empty() {
            let line = String::from_utf8_lossy(&self.current_line)
                .trim()
                .to_string();
            self.current_line.clear();
            if !line.is_empty() {
                self.buffer.append_line_blocking(line);
            }
        }
        Ok(())
    }
}
