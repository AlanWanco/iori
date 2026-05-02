pub mod app;
pub mod commands;
mod i18n;
pub mod inspect;

pub use app::ShioriApp;
pub use shiori_plugin::async_trait;

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc;

pub static USE_TUI: AtomicBool = AtomicBool::new(false);
pub static LOG_TX: OnceLock<mpsc::UnboundedSender<String>> = OnceLock::new();
pub static TUI_LOG_RX: Mutex<Option<mpsc::UnboundedReceiver<String>>> = Mutex::new(None);

pub fn init_tui_logger() {
    let (tx, rx) = mpsc::unbounded_channel();
    LOG_TX.set(tx).unwrap();
    *TUI_LOG_RX.lock().unwrap() = Some(rx);
}

#[derive(Clone)]
pub struct SmartWriter;

impl std::io::Write for SmartWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if USE_TUI.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(tx) = LOG_TX.get() {
                let mut s = String::from_utf8_lossy(buf).into_owned();
                if s.ends_with('\n') {
                    s.pop();
                }

                static ANSI_RE: OnceLock<regex::Regex> = OnceLock::new();
                let re =
                    ANSI_RE.get_or_init(|| regex::Regex::new(r"\x1B\[[0-9;]*[a-zA-Z]").unwrap());
                let clean = re.replace_all(&s, "").into_owned();

                let _ = tx.send(clean);
            }
            Ok(buf.len())
        } else {
            std::io::stderr().write(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !USE_TUI.load(std::sync::atomic::Ordering::Relaxed) {
            std::io::stderr().flush()
        } else {
            Ok(())
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SmartWriter {
    type Writer = Self;
    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}
