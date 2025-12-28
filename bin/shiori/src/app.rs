use clap::Args;
use crossterm::{
    cursor, execute,
    style::{Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use iori::{
    IoriResult, SegmentInfo, StreamType,
    download::{DownloaderApp, TracingApp},
};
use std::collections::HashMap;
use std::{
    io::{self, IsTerminal, Write, stdout},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex, task::JoinHandle};

use crate::commands::download::DownloadCommand;

const BASE_DISPLAY_LINES: usize = 10; // Base number of lines (without stream progress bars)

// Track download start time for speed calculation
use std::time::Instant;

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[derive(Clone, Default)]
struct StreamStats {
    total: usize,
    downloaded: usize,
    failed: usize,
    stream_type: StreamType,
}

fn stream_icon(stream_type: StreamType) -> &'static str {
    match stream_type {
        StreamType::Video => "🎥",
        StreamType::Audio => "🎵",
        StreamType::Subtitle => "📝",
        StreamType::Unknown => "❓",
    }
}

fn stream_name(stream_type: StreamType) -> &'static str {
    match stream_type {
        StreamType::Video => "Video",
        StreamType::Audio => "Audio",
        StreamType::Subtitle => "Subtitle",
        StreamType::Unknown => "Unknown",
    }
}

#[derive(Clone)]
pub struct ShioriApp<T>
where
    T: Args + Clone + Default + Send + Sync + 'static,
{
    fallback_app: Arc<Option<TracingApp>>,

    command: DownloadCommand<T>,
    streams: Arc<Mutex<HashMap<u64, StreamStats>>>,
    recent_download: Arc<Mutex<Option<String>>>,
    last_log: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    start_time: Arc<Mutex<Option<Instant>>>,
    last_line_count: Arc<Mutex<usize>>,
    marquee_offset: Arc<AtomicUsize>,

    handle: Arc<Mutex<Option<JoinHandle<io::Result<()>>>>>,
}

impl<T> ShioriApp<T>
where
    T: Args + Clone + Default + Send + Sync + 'static,
{
    pub fn new(command: DownloadCommand<T>) -> Self {
        let use_tui = !command.no_tui && stdout().is_terminal();

        Self {
            fallback_app: Arc::new(
                use_tui.then(|| TracingApp::concurrent(command.download.concurrency)),
            ),
            command,
            streams: Arc::new(Mutex::new(HashMap::new())),
            recent_download: Arc::new(Mutex::new(None)),
            last_log: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(true)),
            start_time: Arc::new(Mutex::new(None)),
            last_line_count: Arc::new(Mutex::new(0)), // Start with 0 for first render
            marquee_offset: Arc::new(AtomicUsize::new(0)),

            handle: Default::default(),
        }
    }

    fn marquee_text(text: &str, max_width: usize, offset: usize) -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= max_width {
            // Text fits, no need to scroll
            return text.to_string();
        }

        // Add spacing between end and start for continuous scrolling
        let spacing = "   ".chars().collect::<Vec<_>>();
        let mut extended: Vec<char> = chars.clone();
        extended.extend(&spacing);
        extended.extend(&chars);

        // Calculate effective offset (loop around)
        let total_len = chars.len() + spacing.len();
        let effective_offset = offset % total_len;

        // Extract the visible window
        extended
            .iter()
            .cycle()
            .skip(effective_offset)
            .take(max_width)
            .collect()
    }

    async fn set_log(&self, message: impl Into<String>) {
        *self.last_log.lock().await = Some(message.into());
    }

    fn get_display_lines(&self, stream_count: usize) -> usize {
        // Base lines + 1 line per stream for progress bar
        BASE_DISPLAY_LINES + stream_count
    }

    pub async fn run_tui_loop(&self) -> io::Result<()> {
        let mut stdout = io::stdout();

        // Calculate required lines for TUI
        let required_lines = self.get_display_lines(self.streams.lock().await.len());

        // Get terminal size and cursor position
        let (_, term_height) = terminal::size()?;
        let cursor_pos = cursor::position()?;
        let cursor_row = cursor_pos.1;

        // Calculate remaining height in terminal
        let remaining_height = term_height.saturating_sub(cursor_row);

        // Only clear screen if remaining height is not enough
        if remaining_height < required_lines as u16 {
            execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        }

        stdout.flush()?;

        loop {
            self.render_inline().await?;

            // Exit if finished
            let running = self.running.load(Ordering::Relaxed);
            if !running {
                // Move cursor down to end of tui
                let current_line_count = self.get_display_lines(self.streams.lock().await.len());
                execute!(stdout, cursor::MoveDown(current_line_count as u16))?;
                break;
            }

            // Sleep for 50ms
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Ok(())
    }

    async fn render_inline(&self) -> io::Result<()> {
        let mut stdout = stdout();

        let streams = self.streams.lock().await;
        let running = self.running.load(Ordering::Relaxed);
        let recent = self.recent_download.lock().await;
        let last_log = self.last_log.lock().await;
        let start_time = self.start_time.lock().await;

        // Calculate totals
        let total: usize = streams.values().map(|s| s.total).sum();
        let downloaded: usize = streams.values().map(|s| s.downloaded).sum();
        let failed: usize = streams.values().map(|s| s.failed).sum();

        // Calculate speed and ETA
        let (speed, eta) = if let Some(start) = *start_time {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 && downloaded > 0 {
                let speed = downloaded as f64 / elapsed;
                let remaining = total.saturating_sub(downloaded + failed);
                let eta_secs = if speed > 0.0 {
                    (remaining as f64 / speed) as u64
                } else {
                    0
                };
                (format!("{:.1} seg/s", speed), format_duration(eta_secs))
            } else {
                ("-.-- seg/s".to_string(), "--:--".to_string())
            }
        } else {
            ("-.-- seg/s".to_string(), "--:--".to_string())
        };

        let current_line_count = self.get_display_lines(streams.len());
        let mut last_line_count_guard = self.last_line_count.lock().await;

        // Save position at the START of TUI before rendering
        execute!(stdout, cursor::SavePosition)?;

        // Empty line between stderr and TUI
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            Print("\n")
        )?;

        // Top border
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            SetAttribute(Attribute::Bold),
            Print("╭─ "),
            SetForegroundColor(CColor::Cyan),
            Print("Shiori Downloader "),
            SetForegroundColor(CColor::Blue),
            Print("─"),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n")
        )?;

        // Line 1: Output file with marquee effect
        let output_name = self
            .command
            .output
            .output
            .as_ref()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_else(|| "unknown".to_string());

        // Update marquee offset (scroll every 3 renders)
        let render_count = self.marquee_offset.fetch_add(1, Ordering::Relaxed);
        let scroll_offset = render_count / 3;
        let display_name = Self::marquee_text(&output_name, 50, scroll_offset);

        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("│ "),
            SetForegroundColor(CColor::Magenta),
            SetAttribute(Attribute::Bold),
            Print("Output: "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::White),
            Print(display_name),
            ResetColor,
            Print("\n")
        )?;

        // Line 2+: Progress bars for each stream
        let mut sorted_streams: Vec<_> = streams.iter().collect();
        sorted_streams.sort_by_key(|(id, _)| **id);

        for (_stream_id, stats) in sorted_streams {
            let percentage = if stats.total == 0 {
                0.0
            } else {
                (stats.downloaded + stats.failed) as f64 / stats.total as f64
            };
            let bar_width: usize = 40;
            let filled = (bar_width as f64 * percentage) as usize;

            let bar_color = if !running {
                if stats.failed > 0 {
                    CColor::Yellow
                } else {
                    CColor::Green
                }
            } else if stats.failed > 0 {
                CColor::Yellow
            } else {
                CColor::Magenta
            };

            execute!(
                stdout,
                cursor::MoveToColumn(0),
                Clear(ClearType::CurrentLine),
                SetForegroundColor(CColor::Blue),
                Print("│ "),
                SetForegroundColor(CColor::Cyan),
                Print(stream_icon(stats.stream_type)),
                Print(" "),
                SetForegroundColor(CColor::White),
                Print(format!("{:8}", stream_name(stats.stream_type))),
                Print(" "),
                SetForegroundColor(bar_color),
                SetAttribute(Attribute::Bold),
                Print("━".repeat(filled)),
                SetAttribute(Attribute::Reset),
                SetForegroundColor(CColor::DarkGrey),
                SetAttribute(Attribute::Dim),
                Print("─".repeat(bar_width.saturating_sub(filled))),
                SetAttribute(Attribute::Reset),
                SetForegroundColor(CColor::Cyan),
                SetAttribute(Attribute::Bold),
                Print(format!(" {:>5.1}%", percentage * 100.0)),
                SetAttribute(Attribute::Reset),
                SetForegroundColor(CColor::DarkGrey),
                Print(format!(
                    " ({}/{})",
                    stats.downloaded + stats.failed,
                    stats.total
                )),
                ResetColor,
                Print("\n")
            )?;
        }

        // Line 3: Stats
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("│ "),
            SetForegroundColor(CColor::Green),
            SetAttribute(Attribute::Bold),
            Print("✓ "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::White),
            SetAttribute(Attribute::Bold),
            Print(format!("{:>5}", downloaded)),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::DarkGrey),
            Print(" downloaded  "),
            SetForegroundColor(if failed > 0 {
                CColor::Red
            } else {
                CColor::DarkGrey
            }),
            SetAttribute(if failed > 0 {
                Attribute::Bold
            } else {
                Attribute::Reset
            }),
            Print("✗ "),
            SetForegroundColor(if failed > 0 {
                CColor::Red
            } else {
                CColor::DarkGrey
            }),
            SetAttribute(if failed > 0 {
                Attribute::Bold
            } else {
                Attribute::Reset
            }),
            Print(format!("{:>5}", failed)),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::DarkGrey),
            Print(" failed  "),
            SetForegroundColor(CColor::Magenta),
            SetAttribute(Attribute::Bold),
            Print("∑ "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::White),
            SetAttribute(Attribute::Bold),
            Print(format!("{:>5}", total)),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::DarkGrey),
            Print(" total"),
            ResetColor,
            Print("\n")
        )?;

        // Line 4: Speed and ETA
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("│ "),
            SetForegroundColor(CColor::Yellow),
            SetAttribute(Attribute::Bold),
            Print("⚡ "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::Cyan),
            SetAttribute(Attribute::Bold),
            Print(format!("{:>12}", speed)),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::DarkGrey),
            Print("    "),
            SetForegroundColor(CColor::Cyan),
            SetAttribute(Attribute::Bold),
            Print("⏱ "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::White),
            Print(format!("ETA: {}", eta)),
            SetForegroundColor(CColor::DarkGrey),
            Print("    "),
            SetForegroundColor(CColor::Magenta),
            Print("⚙ "),
            SetForegroundColor(CColor::White),
            Print(format!(
                "{} threads",
                self.command.download.concurrency.get()
            )),
            ResetColor,
            Print("\n")
        )?;

        // Separator
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("├─"),
            SetAttribute(Attribute::Dim),
            Print("─".repeat(78)),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n")
        )?;

        // Line 5: Recent download
        let recent_text = recent
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("Waiting for segments...");
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("│ "),
            SetForegroundColor(CColor::Green),
            Print("⟩ "),
            SetForegroundColor(CColor::White),
            Print(&recent_text.chars().take(70).collect::<String>()),
            ResetColor,
            Print("\n")
        )?;

        // Line 6: Last log
        let log_text = last_log.as_deref().unwrap_or("");
        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("│ "),
            SetForegroundColor(CColor::DarkGrey),
            Print(&log_text.chars().take(70).collect::<String>()),
            ResetColor,
            Print("\n")
        )?;

        // Bottom border
        let status_icon = if running { "⏵" } else { "■" };
        let status_text = if running { "Running" } else { "Finished" };
        let status_color = if running { CColor::Green } else { CColor::Cyan };

        execute!(
            stdout,
            cursor::MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(CColor::Blue),
            Print("╰─ "),
            SetForegroundColor(status_color),
            SetAttribute(Attribute::Bold),
            Print(status_icon),
            Print(" "),
            Print(status_text),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::Blue),
            Print(" ─"),
            ResetColor,
            Print("\n")
        )?;

        // Update last line count
        *last_line_count_guard = current_line_count;
        drop(last_line_count_guard);

        // Restore position to the start of the TUI
        execute!(stdout, cursor::RestorePosition)?;

        stdout.flush()?;
        Ok(())
    }
}

impl<T> DownloaderApp for ShioriApp<T>
where
    T: Args + Clone + Default + Send + Sync + 'static,
{
    async fn on_start(&self) -> IoriResult<()> {
        if let Some(app) = self.fallback_app.as_ref() {
            return app.on_start().await;
        }

        *self.start_time.lock().await = Some(Instant::now());
        self.set_log("Download started").await;
        let me = self.clone();
        *self.handle.lock().await = Some(tokio::spawn(async move { me.run_tui_loop().await }));
        Ok(())
    }

    async fn on_receive_segments(&self, segments: &[SegmentInfo]) {
        if let Some(app) = self.fallback_app.as_ref() {
            return app.on_receive_segments(segments).await;
        }

        let mut streams = self.streams.lock().await;
        for seg in segments {
            streams
                .entry(seg.stream_id)
                .or_insert_with(|| StreamStats {
                    stream_type: seg.stream_type,
                    ..Default::default()
                })
                .total += 1;
        }
        self.set_log(format!("{} segments added to queue", segments.len()))
            .await;
    }

    async fn on_downloaded_segment(&self, segment: &SegmentInfo) {
        if let Some(app) = self.fallback_app.as_ref() {
            return app.on_downloaded_segment(segment).await;
        }

        let mut streams = self.streams.lock().await;
        if let Some(stats) = streams.get_mut(&segment.stream_id) {
            stats.downloaded += 1;
        }
        *self.recent_download.lock().await = Some(segment.file_name.clone());
    }

    async fn on_failed_segment(&self, segment: &SegmentInfo) {
        if let Some(app) = self.fallback_app.as_ref() {
            return app.on_failed_segment(segment).await;
        }

        let mut streams = self.streams.lock().await;
        if let Some(stats) = streams.get_mut(&segment.stream_id) {
            stats.failed += 1;
        }
        self.set_log(format!("Failed: {}", segment.file_name)).await;
        tracing::error!("Failed: {}", segment.file_name);
    }

    async fn on_finished(&self) -> IoriResult<()> {
        if let Some(app) = self.fallback_app.as_ref() {
            return app.on_finished().await;
        }

        self.running.store(false, Ordering::Relaxed);

        let streams = self.streams.lock().await;
        let total: usize = streams.values().map(|s| s.total).sum();
        let downloaded: usize = streams.values().map(|s| s.downloaded).sum();
        let failed: usize = streams.values().map(|s| s.failed).sum();
        drop(streams); // drop immediately to avoid deadlock

        self.set_log(format!(
            "Download finished: {} succeeded, {} failed, {} total",
            downloaded, failed, total
        ))
        .await;

        if let Some(handle) = self.handle.lock().await.take() {
            handle.await.unwrap()?;
        }

        Ok(())
    }
}
