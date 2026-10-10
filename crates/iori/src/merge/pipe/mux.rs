//! A single owner for the FFmpeg child and both input pipes. A failed child is
//! never reused: cancel both writers, reap it, and replay complete cached A/V
//! segments through fresh pipes. Cached segments already include their init data
//! (see WriteSegment), including decrypted fMP4 initialization boxes.
use super::{SegmentBuffer, ffmpeg_supports_readrate_catchup, is_live_output, readrate_args};
use crate::{
    IoriResult, SegmentInfo, StreamType,
    cache::{CacheSource, CacheSourceReader},
    util::ordered_stream::OrderedStream,
};
use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWrite},
    process::{Child, Command},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

type Writer = Box<dyn AsyncWrite + Unpin + Send>;
type OpenFuture = Pin<Box<dyn Future<Output = IoriResult<CacheSourceReader>> + Send>>;
type CleanupFuture = Pin<Box<dyn Future<Output = IoriResult<()>> + Send>>;
type Message = (u64, u64, Arc<CachedSegment>);

// Retain a short replay window, not the entire live broadcast. Pending segments
// stay in the cache until both writers have advanced past them.
const REPLAY_SEGMENTS: usize = 8;
const MAX_RESTARTS: u32 = 8;
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

struct CachedSegment {
    info: SegmentInfo,
    open: Option<Box<dyn Fn() -> OpenFuture + Send + Sync>>,
    cleanup: Box<dyn Fn() -> CleanupFuture + Send + Sync>,
}

impl CachedSegment {
    fn new(info: SegmentInfo, cache: impl CacheSource, failed: bool) -> Arc<Self> {
        let cache = Arc::new(cache);
        let open_cache = cache.clone();
        let open_info = info.clone();
        let cleanup_info = info.clone();
        Arc::new(Self {
            info,
            open: if failed {
                None
            } else {
                Some(Box::new(move || {
                    let cache = open_cache.clone();
                    let info = open_info.clone();
                    Box::pin(async move { cache.open_reader(&info).await })
                }))
            },
            cleanup: Box::new(move || {
                let cache = cache.clone();
                let info = cleanup_info.clone();
                Box::pin(async move { cache.invalidate(&info).await })
            }),
        })
    }
}

pub(super) struct Mux {
    sender: Option<mpsc::UnboundedSender<Message>>,
    stop: CancellationToken,
    task: Option<JoinHandle<IoriResult<()>>>,
}

impl Mux {
    pub(super) fn new(
        recycle: bool,
        output: PathBuf,
        extra: Option<String>,
        has_audio: bool,
        buffer: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            let readrate = extra.is_some() || buffer > 0 || is_live_output(&output);
            let catchup = tokio::select! {
                biased;
                _ = task_stop.cancelled() => return Ok(()),
                supported = ffmpeg_supports_readrate_catchup() => readrate && supported,
            };
            let config = Config {
                program: "ffmpeg".into(),
                output,
                extra,
                has_audio,
                readrate,
                catchup,
                recycle,
                buffer,
                retry_delay: Duration::from_secs(1),
            };
            let result = run(config, receiver, task_stop.clone()).await;
            if result.is_err() {
                // Wake the downloader too; do not keep downloading forever
                // into an output worker whose retry budget is exhausted.
                task_stop.cancel();
            }
            result
        });
        Self {
            sender: Some(sender),
            stop,
            task: Some(task),
        }
    }

    pub(super) fn send(
        &self,
        info: SegmentInfo,
        cache: impl CacheSource,
        failed: bool,
    ) -> IoriResult<()> {
        if self.stop.is_cancelled() {
            // In-flight downloads may still finish after Ctrl-C. Keep their cache.
            return Ok(());
        }
        let item = CachedSegment::new(info, cache, failed);
        self.sender
            .as_ref()
            .ok_or_else(|| io::Error::other("FFmpeg mux is closed"))?
            .send((item.info.stream_id, item.info.sequence, item))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "FFmpeg mux stopped"))?;
        Ok(())
    }

    pub(super) fn cancellation_token(&self) -> CancellationToken {
        self.stop.clone()
    }

    pub(super) async fn finish(&mut self) -> IoriResult<()> {
        self.sender.take();
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|_| io::Error::other("FFmpeg mux task failed"))??;
        }
        Ok(())
    }

    pub(super) fn was_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }
}

impl Drop for Mux {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct Config {
    program: PathBuf,
    output: PathBuf,
    extra: Option<String>,
    has_audio: bool,
    readrate: bool,
    catchup: bool,
    recycle: bool,
    buffer: usize,
    retry_delay: Duration,
}

struct Generation {
    child: Child,
    pipes: [Option<Writer>; 2],
    writers: JoinSet<(usize, Writer, IoriResult<()>)>,
    busy: [bool; 2],
    stderr: JoinHandle<()>,
    progressed: Arc<AtomicBool>,
}

impl Generation {
    async fn spawn(config: &Config) -> io::Result<Self> {
        let mut command = Command::new(&config.program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Ctrl-C belongs to the downloader. It cancels this entire generation,
        // rather than killing the child first and racing its restart logic.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "windows")]
        command.creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP: parent owns Ctrl-C.

        #[cfg(not(target_os = "windows"))]
        let audio: Option<Writer> = if config.has_audio {
            use command_fds::{CommandFdExt, FdMapping};
            let (writer, reader) = tokio::net::unix::pipe::pipe()?;
            command
                .fd_mappings(vec![FdMapping {
                    parent_fd: reader.into_blocking_fd()?,
                    child_fd: 3,
                }])
                .map_err(io::Error::other)?;
            Some(Box::new(writer))
        } else {
            None
        };

        #[cfg(target_os = "windows")]
        let (audio, audio_name) = if config.has_audio {
            let name = format!(r"\\.\pipe\iori-mux-{}", rand::random::<u64>());
            let writer = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            (Some(writer), Some(name))
        } else {
            (None, None)
        };

        command.args([
            "-y",
            "-nostdin",
            "-nostats",
            "-progress",
            "pipe:2",
            "-fflags",
            "+genpts",
        ]);
        if config.readrate {
            command.args(readrate_args(config.catchup));
        }
        command.args(["-i", "pipe:0"]);
        if config.has_audio {
            if config.readrate {
                command.args(readrate_args(config.catchup));
            }
            #[cfg(not(target_os = "windows"))]
            command.args(["-i", "pipe:3"]);
            #[cfg(target_os = "windows")]
            command.args(["-i", audio_name.as_ref().unwrap()]);
            command.args(["-map", "0:v:0", "-map", "1:a:0"]);
        }
        command.args([
            "-strict",
            "unofficial",
            "-c",
            "copy",
            "-metadata",
            &format!("date={}", chrono::Utc::now().to_rfc3339()),
            "-ignore_unknown",
            "-copy_unknown",
        ]);
        if let Some(extra) = &config.extra {
            let args =
                shlex::split(extra).ok_or_else(|| io::Error::other("Invalid FFmpeg arguments"))?;
            command.args(args);
        } else if is_live_output(&config.output) {
            command.args(["-f", "flv"]).arg(&config.output);
        } else {
            command
                .args(["-f", "mpegts", "-shortest"])
                .arg(&config.output);
        }
        let mut child = command.spawn()?;
        let video: Writer = Box::new(child.stdin.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        let progressed = Arc::new(AtomicBool::new(false));
        let progress = progressed.clone();
        let output = config.output.to_string_lossy().into_owned();
        let stderr = tokio::spawn(forward_stderr(stderr, output, progress));

        // Connection must run concurrently with video probing: FFmpeg opens its
        // second input only after enough video bytes have arrived.
        #[cfg(target_os = "windows")]
        let audio: Option<Writer> =
            audio.map(|server| Box::new(ConnectingPipe::new(server)) as Writer);

        tracing::info!("[ffmpeg] mux process started; waiting for output progress");
        Ok(Self {
            child,
            pipes: [Some(video), audio],
            writers: JoinSet::new(),
            busy: [false; 2],
            stderr,
            progressed,
        })
    }

    fn pump(&mut self, tracks: &[Track; 2]) {
        for (lane, track) in tracks.iter().enumerate() {
            if self.busy[lane] {
                continue;
            }
            let Some(segment) = track.pending.front().cloned() else {
                continue;
            };
            let Some(mut pipe) = self.pipes[lane].take() else {
                continue;
            };
            self.busy[lane] = true;
            self.writers.spawn(async move {
                let result = tokio::time::timeout(WRITE_TIMEOUT, async {
                    let mut reader =
                        segment.open.as_ref().expect("failed segments are filtered")().await?;
                    tokio::io::copy(&mut reader, &mut pipe).await?;
                    Ok(())
                })
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(io::ErrorKind::TimedOut, "FFmpeg input stalled").into())
                });
                (lane, pipe, result)
            });
        }
    }

    async fn shutdown(&mut self) {
        self.writers.abort_all();
        while self.writers.join_next().await.is_some() {}
        self.pipes = [None, None];
        // kill() also waits/reaps. Never leave the old child reading any pipe.
        let _ = self.child.kill().await;
        self.stderr.abort();
        let _ = (&mut self.stderr).await;
    }
}

impl Drop for Generation {
    fn drop(&mut self) {
        self.writers.abort_all();
        self.stderr.abort();
        // Child's kill_on_drop is a final safety net for aborted owner tasks.
    }
}

#[derive(Default)]
struct Track {
    pending: VecDeque<Arc<CachedSegment>>,
    history: VecDeque<Arc<CachedSegment>>,
}

impl Track {
    async fn complete(&mut self, recycle: bool) {
        self.history.push_back(self.pending.pop_front().unwrap());
        if self.history.len() > REPLAY_SEGMENTS {
            let old = self.history.pop_front().unwrap();
            cleanup(&old, recycle).await;
        }
    }

    fn rewind(&mut self) {
        self.history.append(&mut self.pending);
        self.pending = std::mem::take(&mut self.history);
    }
}

async fn cleanup(segment: &CachedSegment, recycle: bool) {
    if recycle && let Err(_) = (segment.cleanup)().await {
        tracing::warn!("[ffmpeg] failed to recycle a cached segment");
    }
}

// Align only when the source provides exact synchronization keys. Never infer
// media time from unrelated local sequence counters. Failed A/V pairs are
// removed together by the downloader; unmatched startup tails are left cached.
fn align(tracks: &mut [Track; 2], has_audio: bool) -> bool {
    if !has_audio {
        return !tracks[0].pending.is_empty();
    }
    loop {
        let (Some(video), Some(audio)) = (tracks[0].pending.front(), tracks[1].pending.front())
        else {
            return false;
        };
        match (
            video.info.synchronization_key,
            audio.info.synchronization_key,
        ) {
            (Some(v), Some(a)) if v < a => {
                tracks[0].pending.pop_front();
            }
            (Some(v), Some(a)) if a < v => {
                tracks[1].pending.pop_front();
            }
            _ => return true,
        }
    }
}

async fn run(
    config: Config,
    receiver: mpsc::UnboundedReceiver<Message>,
    stop: CancellationToken,
) -> IoriResult<()> {
    let mut stream = OrderedStream::new(receiver);
    let mut buffered = SegmentBuffer::new(config.buffer);
    let mut tracks = [Track::default(), Track::default()];
    let mut generation: Option<Generation> = None;
    let mut ended = false;
    let mut restarts = 0u32;
    let mut saw_media = false;
    // Do not restart files: spawning with -y would overwrite previous output.
    let reconnect = if let Some(extra) = &config.extra {
        shlex::split(extra).is_some_and(|args| {
            args.iter()
                .any(|arg| is_live_output(std::path::Path::new(arg)))
        })
    } else {
        is_live_output(&config.output)
    };

    let result = loop {
        if stop.is_cancelled() {
            break Ok(());
        }
        if generation.is_none() && align(&mut tracks, config.has_audio) {
            match Generation::spawn(&config).await {
                Ok(child) => generation = Some(child),
                Err(error) => break Err(error.into()),
            }
        }

        if generation.is_none() && ended {
            break if saw_media {
                Err(io::Error::other("No aligned audio/video segments available for FFmpeg").into())
            } else {
                Ok(())
            };
        }

        enum Event {
            Stop,
            Finished,
            Segment(Option<(u64, Arc<CachedSegment>)>),
            Written(usize, Writer, IoriResult<()>),
            Failed,
        }
        let event = if let Some(child) = &mut generation {
            child.pump(&tracks);
            if ended
                && tracks.iter().all(|track| track.pending.is_empty())
                && child.writers.is_empty()
            {
                // Close both inputs together before waiting. EOF is not proof
                // that the output succeeded; a failed live output can replay.
                child.pipes = [None, None];
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => Event::Stop,
                    status = tokio::time::timeout(EXIT_TIMEOUT, child.child.wait()) => match status {
                        Ok(Ok(status)) if status.success() => Event::Finished,
                        _ => Event::Failed,
                    },
                }
            } else {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => Event::Stop,
                    status = child.child.wait() => {
                        match status {
                            Ok(status) => tracing::warn!("[ffmpeg] mux process exited: {status}"),
                            Err(_) => tracing::warn!("[ffmpeg] failed to wait for mux process"),
                        }
                        Event::Failed
                    },
                    result = child.writers.join_next(), if !child.writers.is_empty() => match result {
                        Some(Ok((lane, pipe, result))) => {
                            if result.is_err() {
                                let input = if lane == 0 { "video" } else { "audio" };
                                tracing::warn!("[ffmpeg] {input} input writer failed; stopping both inputs");
                            }
                            Event::Written(lane, pipe, result)
                        },
                        _ => Event::Failed,
                    },
                    item = buffered.next(&mut stream), if !ended => Event::Segment(item),
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = stop.cancelled() => Event::Stop,
                item = buffered.next(&mut stream) => Event::Segment(item),
            }
        };
        match event {
            Event::Stop | Event::Finished => break Ok(()),
            Event::Segment(None) => ended = true,
            Event::Segment(Some((_, item))) => {
                if item.open.is_none() {
                    continue;
                }
                let lane = match item.info.stream_type {
                    StreamType::Video => 0,
                    StreamType::Audio if config.has_audio => 1,
                    _ => {
                        cleanup(&item, config.recycle).await;
                        continue;
                    }
                };
                saw_media = true;
                tracks[lane].pending.push_back(item);
            }
            Event::Written(lane, pipe, Ok(())) => {
                let child = generation.as_mut().unwrap();
                child.pipes[lane] = Some(pipe);
                child.busy[lane] = false;
                tracks[lane].complete(config.recycle).await;
            }
            Event::Written(_, _, Err(_)) | Event::Failed => {
                let mut child = generation.take().unwrap();
                if child.progressed.load(Ordering::Relaxed) {
                    restarts = 0;
                }
                child.shutdown().await;
                if stop.is_cancelled() {
                    break Ok(());
                }
                if !reconnect || restarts >= MAX_RESTARTS {
                    break Err(io::Error::other(
                        "FFmpeg mux failed; keeping remaining cached segments",
                    )
                    .into());
                }
                restarts += 1;
                for track in &mut tracks {
                    track.rewind();
                }
                let delay = config.retry_delay * restarts.min(10);
                tracing::warn!(
                    "[ffmpeg] output interrupted; rebuilding both inputs in {} seconds (attempt {restarts}/{MAX_RESTARTS})",
                    delay.as_secs_f64()
                );
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break Ok(()),
                    _ = tokio::time::sleep(delay) => (),
                }
            }
        }
    };
    if let Some(mut child) = generation {
        child.shutdown().await;
    }
    if result.is_ok() && !stop.is_cancelled() {
        for track in tracks {
            for segment in track.history {
                cleanup(&segment, config.recycle).await;
            }
        }
    }
    result
}

fn redact(line: &str, output: &str) -> String {
    static URL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let line = if output != "-" && !output.is_empty() {
        line.replace(output, "<output>")
    } else {
        line.to_owned()
    };
    URL.get_or_init(|| regex::Regex::new(r#"(?i)(?:https?|rtmps?)://[^\s'\"]+"#).unwrap())
        .replace_all(&line, "<redacted-url>")
        .into_owned()
}

async fn forward_stderr(
    mut stderr: tokio::process::ChildStderr,
    output: String,
    progressed: Arc<AtomicBool>,
) {
    let mut bytes = [0; 4096];
    let mut line = Vec::new();
    let mut last_logged_time = 0i64;
    while let Ok(n) = stderr.read(&mut bytes).await {
        if n == 0 {
            break;
        }
        for &byte in &bytes[..n] {
            if byte == b'\n' || byte == b'\r' {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim();
                if let Some(value) = text.strip_prefix("out_time_us=") {
                    if let Ok(time) = value.parse::<i64>()
                        && time > 0
                    {
                        if !progressed.swap(true, Ordering::Relaxed) {
                            tracing::info!(
                                "[ffmpeg] output progress observed; mux is producing media"
                            );
                        }
                        if time.saturating_sub(last_logged_time) >= 5_000_000 {
                            tracing::info!(
                                "[ffmpeg] output time: {:.2}s",
                                time as f64 / 1_000_000.0
                            );
                            last_logged_time = time;
                        }
                    }
                } else if !text.is_empty() && !is_progress_line(text) {
                    tracing::info!("[ffmpeg] {}", redact(text, &output));
                }
                line.clear();
            } else if line.len() < 16384 {
                line.push(byte);
            }
        }
    }
}

fn is_progress_line(line: &str) -> bool {
    [
        "frame=",
        "fps=",
        "stream_",
        "bitrate=",
        "total_size=",
        "out_time",
        "dup_frames=",
        "drop_frames=",
        "speed=",
        "progress=",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[cfg(target_os = "windows")]
struct ConnectingPipe {
    connect: Pin<
        Box<
            dyn Future<Output = io::Result<tokio::net::windows::named_pipe::NamedPipeServer>>
                + Send,
        >,
    >,
    pipe: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(target_os = "windows")]
impl ConnectingPipe {
    fn new(pipe: tokio::net::windows::named_pipe::NamedPipeServer) -> Self {
        Self {
            connect: Box::pin(async move {
                pipe.connect().await?;
                Ok(pipe)
            }),
            pipe: None,
        }
    }
}

#[cfg(target_os = "windows")]
impl AsyncWrite for ConnectingPipe {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        if self.pipe.is_none() {
            self.pipe = Some(std::task::ready!(self.connect.as_mut().poll(cx))?);
        }
        Pin::new(self.pipe.as_mut().unwrap()).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.pipe.as_mut() {
            Some(pipe) => Pin::new(pipe).poll_flush(cx),
            None => std::task::Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.pipe.as_mut() {
            Some(pipe) => Pin::new(pipe).poll_shutdown(cx),
            None => std::task::Poll::Ready(Ok(())),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod rtmp_test;
