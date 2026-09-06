use super::Merger;
use crate::{
    SegmentInfo, StreamType,
    cache::CacheSource,
    error::IoriResult,
    util::{ordered_stream::OrderedStream, path::DuplicateOutputFileNamer},
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
};

type SendSegment = (
    Pin<Box<dyn AsyncRead + Send + 'static>>,
    StreamType,
    Pin<Box<dyn Future<Output = IoriResult<()>> + Send>>,
);

fn is_live_output(output: &Path) -> bool {
    let output = output.to_string_lossy();
    output.starts_with("rtmp://") || output.starts_with("rtmps://")
}

fn readrate_args(use_readrate_catchup: bool) -> &'static [&'static str] {
    if use_readrate_catchup {
        &["-re", "-readrate_catchup", "1.25"]
    } else {
        &["-re"]
    }
}

async fn ffmpeg_supports_readrate_catchup() -> bool {
    let Ok(output) = Command::new("ffmpeg")
        .args(["-hide_banner", "-h", "full"])
        .output()
        .await
    else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    output
        .stdout
        .into_iter()
        .chain(output.stderr)
        .collect::<Vec<_>>()
        .split(|byte| *byte == b'\n')
        .map(|line| String::from_utf8_lossy(line).trim().to_string())
        .any(|line| line.split_whitespace().next() == Some("-readrate_catchup"))
}

struct SegmentBuffer<T> {
    items: VecDeque<(u64, T)>,
    target: usize,
    primed: bool,
    ended: bool,
}

impl<T> SegmentBuffer<T> {
    fn new(target: usize) -> Self {
        Self {
            items: VecDeque::new(),
            target,
            primed: false,
            ended: false,
        }
    }

    async fn next(&mut self, stream: &mut OrderedStream<T>) -> Option<(u64, T)> {
        if !self.primed {
            while self.items.len() <= self.target {
                let Some(item) = stream.next().await else {
                    self.ended = true;
                    break;
                };
                self.items.push_back(item);
            }
            self.primed = true;
        }

        if let Some(item) = self.items.pop_front() {
            return Some(item);
        }

        if self.ended {
            None
        } else {
            stream.next().await
        }
    }
}

/// PipeMerger is a merger that pipes the segments directly to the output.
///
/// If there are any missing segments, it will skip them.
/// PipeMerger does not and can not handle discontinuities.
pub struct PipeMerger {
    recycle: bool,

    sender: Option<mpsc::UnboundedSender<(u64, u64, Option<SendSegment>)>>,
    future: Option<JoinHandle<()>>,
}

impl PipeMerger {
    pub fn stdout(recycle: bool) -> Self {
        Self::stdout_with_buffer(recycle, 0)
    }

    pub fn stdout_with_buffer(recycle: bool, buffer_segments: usize) -> Self {
        Self::writer_with_buffer(recycle, tokio::io::stdout(), buffer_segments)
    }

    pub fn writer(recycle: bool, writer: impl AsyncWrite + Unpin + Send + Sync + 'static) -> Self {
        Self::writer_with_buffer(recycle, writer, 0)
    }

    pub fn writer_with_buffer(
        recycle: bool,
        mut writer: impl AsyncWrite + Unpin + Send + Sync + 'static,
        buffer_segments: usize,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let mut stream: OrderedStream<Option<SendSegment>> = OrderedStream::new(rx);
        let future = tokio::spawn(async move {
            let mut buffered = SegmentBuffer::new(buffer_segments);
            while let Some((_, segment)) = buffered.next(&mut stream).await {
                if let Some((mut reader, _type, invalidate)) = segment {
                    _ = tokio::io::copy(&mut reader, &mut writer).await;
                    if recycle {
                        _ = invalidate.await;
                    }
                }
            }
        });

        Self {
            recycle,

            sender: Some(tx),
            future: Some(future),
        }
    }

    pub fn file(recycle: bool, target_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let mut stream: OrderedStream<Option<SendSegment>> = OrderedStream::new(rx);
        let future = tokio::spawn(async move {
            let mut namer = DuplicateOutputFileNamer::new(target_path.clone());
            let mut target = Some(
                tokio::fs::File::create(&target_path)
                    .await
                    .expect("Failed to create file"),
            );
            while let Some((_, segment)) = stream.next().await {
                if let Some((mut reader, _type, invalidate)) = segment {
                    if target.is_none() {
                        let file = tokio::fs::File::create(namer.next_path())
                            .await
                            .expect("Failed to create file");
                        target = Some(file);
                    }

                    if let Some(target) = &mut target {
                        _ = tokio::io::copy(&mut reader, target).await;
                    }
                    if recycle {
                        _ = invalidate.await;
                    }
                } else {
                    target = None;
                }
            }
        });

        Self {
            recycle,

            sender: Some(tx),
            future: Some(future),
        }
    }

    pub fn mux(
        recycle: bool,
        output: PathBuf,
        extra_command: Option<String>,
        has_audio: bool,
    ) -> Self {
        Self::mux_with_buffer(recycle, output, extra_command, has_audio, 0)
    }

    pub fn mux_with_buffer(
        recycle: bool,
        output: PathBuf,
        extra_command: Option<String>,
        has_audio: bool,
        buffer_segments: usize,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let mut stream: OrderedStream<Option<SendSegment>> = OrderedStream::new(rx);

        #[cfg(target_os = "windows")]
        let (audio_pipe, audio_receiver) = if has_audio {
            let pipe_name = format!(r"\\.\pipe\iori-pipe-mux-audio-{}", rand::random::<u64>());
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)
                .unwrap();
            (Some(server), Some(pipe_name))
        } else {
            (None, None)
        };

        #[cfg(not(target_os = "windows"))]
        let (audio_pipe, audio_receiver) = if has_audio {
            let (pipe, receiver) = tokio::net::unix::pipe::pipe().unwrap();
            (Some(pipe), Some(receiver.into_nonblocking_fd().unwrap()))
        } else {
            (None, None)
        };

        let future = tokio::spawn(async move {
            let use_readrate_catchup = ffmpeg_supports_readrate_catchup().await;
            let output_for_initial = output.clone();
            let extra_for_initial = extra_command.clone();
            #[cfg(target_os = "windows")]
            let audio_receiver_for_initial = audio_receiver.clone();
            #[cfg(not(target_os = "windows"))]
            let audio_receiver_for_initial =
                audio_receiver.as_ref().and_then(|fd| fd.try_clone().ok());

            // TODO: maybe creating a new process might be better
            let mut video_pipe = tokio::spawn(async move {
                let mut command = Command::new("ffmpeg");
                command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::piped());

                #[cfg(not(target_os = "windows"))]
                {
                    if let Some(audio_rx) = audio_receiver_for_initial {
                        use command_fds::{CommandFdExt, FdMapping};
                        command
                            .fd_mappings(vec![FdMapping {
                                parent_fd: audio_rx,
                                child_fd: 3,
                            }])
                            .unwrap();
                    }
                }

                command.args(["-y", "-fflags", "+genpts"]); // , "-loglevel", "quiet"

                if extra_for_initial.is_some()
                    || buffer_segments > 0
                    || is_live_output(&output_for_initial)
                {
                    // The default catch-up rate is too slow for interleaved A/V MPEG-TS pipes.
                    // Older system FFmpeg versions do not have -readrate_catchup.
                    command.args(readrate_args(use_readrate_catchup));
                }

                // video input: stdin
                command.args(["-i", "pipe:0"]);

                // audio input: mapped fd 3 or named pipe
                if has_audio {
                    #[cfg(target_os = "windows")]
                    if let Some(audio_rx) = audio_receiver_for_initial {
                        command.args(["-i", &audio_rx]);
                    }
                    #[cfg(not(target_os = "windows"))]
                    command.args(["-i", "pipe:3"]);

                    command.args(["-map", "0", "-map", "1"]);
                }

                #[rustfmt::skip]
                command.args([
                    "-strict", "unofficial",
                    "-c", "copy",
                    "-metadata", &format!(r#"date="{}""#, chrono::Utc::now().to_rfc3339()),
                    "-ignore_unknown",
                    "-copy_unknown",
                ]);

                if let Some(dest) = extra_for_initial.and_then(|s| shlex::split(&s)) {
                    command.args(dest);
                } else if is_live_output(&output_for_initial) {
                    command.args(["-f", "flv"]).arg(output_for_initial);
                } else {
                    command
                        .args(["-f", "mpegts", "-shortest"])
                        .arg(output_for_initial);
                }

                let mut process = command.spawn().unwrap();
                let stdin = process.stdin.take().unwrap();

                // Capture and forward ffmpeg output to tracing
                let mut stderr = process.stderr.take().unwrap();
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = vec![0; 1024];
                    let mut line = String::new();
                    while let Ok(n) = stderr.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        for c in chunk.chars() {
                            if c == '\r' || c == '\n' {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    tracing::info!("[ffmpeg] {}", trimmed);
                                }
                                line.clear();
                            } else {
                                line.push(c);
                            }
                        }
                    }
                });

                tokio::spawn(async move {
                    match process.wait().await {
                        Ok(status) => {
                            if !status.success() {
                                tracing::error!("[ffmpeg] exited with status: {}", status);
                            } else {
                                tracing::info!("[ffmpeg] exited successfully");
                            }
                        }
                        Err(e) => {
                            tracing::error!("[ffmpeg] failed to wait for process: {}", e);
                        }
                    }
                });

                stdin
            })
            .await
            .unwrap();

            let (video_sender, mut video_receiver) = mpsc::unbounded_channel::<SendSegment>();
            let output_for_restart = output.clone();
            let extra_for_restart = extra_command.clone();
            #[cfg(target_os = "windows")]
            let audio_receiver_for_restart = audio_receiver.clone();
            #[cfg(not(target_os = "windows"))]
            let audio_receiver_for_restart = audio_receiver;

            let video_handle = tokio::spawn(async move {
                while let Some((mut reader, _, invalidate)) = video_receiver.recv().await {
                    if let Err(e) = tokio::io::copy(&mut reader, &mut video_pipe).await {
                        tracing::error!("[ffmpeg] Broken video pipe: {}", e);
                        tracing::warn!("[ffmpeg] trying to restart ffmpeg mux process...");

                        let restarted = tokio::spawn({
                            let output = output_for_restart.clone();
                            let extra_command = extra_for_restart.clone();
                            #[cfg(target_os = "windows")]
                            let audio_receiver = audio_receiver_for_restart.clone();
                            #[cfg(not(target_os = "windows"))]
                            let audio_receiver = audio_receiver_for_restart
                                .as_ref()
                                .and_then(|fd| fd.try_clone().ok());

                            async move {
                                let mut command = Command::new("ffmpeg");
                                command
                                    .stdin(Stdio::piped())
                                    .stdout(Stdio::inherit())
                                    .stderr(Stdio::piped());

                                #[cfg(not(target_os = "windows"))]
                                {
                                    if let Some(audio_rx) = audio_receiver {
                                        use command_fds::{CommandFdExt, FdMapping};
                                        command
                                            .fd_mappings(vec![FdMapping {
                                                parent_fd: audio_rx,
                                                child_fd: 3,
                                            }])
                                            .unwrap();
                                    }
                                }

                                command.args(["-y", "-fflags", "+genpts"]);
                                if extra_command.is_some()
                                    || buffer_segments > 0
                                    || is_live_output(&output)
                                {
                                    command.args(readrate_args(use_readrate_catchup));
                                }
                                command.args(["-i", "pipe:0"]);

                                if has_audio {
                                    #[cfg(target_os = "windows")]
                                    if let Some(audio_rx) = audio_receiver {
                                        command.args(["-i", &audio_rx]);
                                    }
                                    #[cfg(not(target_os = "windows"))]
                                    command.args(["-i", "pipe:3"]);
                                    command.args(["-map", "0", "-map", "1"]);
                                }

                                #[rustfmt::skip]
                                command.args([
                                    "-strict", "unofficial",
                                    "-c", "copy",
                                    "-metadata", &format!(r#"date=\"{}\""#, chrono::Utc::now().to_rfc3339()),
                                    "-ignore_unknown",
                                    "-copy_unknown",
                                ]);

                                if let Some(dest) = extra_command.and_then(|s| shlex::split(&s)) {
                                    command.args(dest);
                                } else if is_live_output(&output) {
                                    command.args(["-f", "flv"]).arg(output);
                                } else {
                                    command.args(["-f", "mpegts", "-shortest"]).arg(output);
                                }

                                let mut process = command.spawn().unwrap();
                                let stdin = process.stdin.take().unwrap();

                                let mut stderr = process.stderr.take().unwrap();
                                tokio::spawn(async move {
                                    use tokio::io::AsyncReadExt;
                                    let mut buf = vec![0; 1024];
                                    let mut line = String::new();
                                    while let Ok(n) = stderr.read(&mut buf).await {
                                        if n == 0 {
                                            break;
                                        }
                                        let chunk = String::from_utf8_lossy(&buf[..n]);
                                        for c in chunk.chars() {
                                            if c == '\r' || c == '\n' {
                                                let trimmed = line.trim();
                                                if !trimmed.is_empty() {
                                                    tracing::info!("[ffmpeg] {}", trimmed);
                                                }
                                                line.clear();
                                            } else {
                                                line.push(c);
                                            }
                                        }
                                    }
                                });

                                tokio::spawn(async move {
                                    match process.wait().await {
                                        Ok(status) => {
                                            if !status.success() {
                                                tracing::error!("[ffmpeg] exited with status: {}", status);
                                            } else {
                                                tracing::info!("[ffmpeg] exited successfully");
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!("[ffmpeg] failed to wait for process: {}", e);
                                        }
                                    }
                                });

                                stdin
                            }
                        })
                        .await;

                        match restarted {
                            Ok(new_pipe) => {
                                video_pipe = new_pipe;
                                tracing::warn!("[ffmpeg] restart succeeded, continue piping");
                                continue;
                            }
                            Err(e) => {
                                tracing::error!("[ffmpeg] restart failed: {}", e);
                                break;
                            }
                        }
                    }
                    if recycle && let Err(e) = invalidate.await {
                        tracing::warn!("[ffmpeg] Failed to invalidate segment: {}", e);
                    }
                }
            });

            let (audio_sender, mut audio_receiver) = mpsc::unbounded_channel::<SendSegment>();
            let audio_handle = tokio::spawn(async move {
                if let Some(mut audio_pipe) = audio_pipe {
                    #[cfg(target_os = "windows")]
                    audio_pipe.connect().await.unwrap();

                    while let Some((mut reader, _, invalidate)) = audio_receiver.recv().await {
                        if let Err(e) = tokio::io::copy(&mut reader, &mut audio_pipe).await {
                            tracing::error!("[ffmpeg] Broken audio pipe: {}", e);
                            break;
                        }
                        if recycle && let Err(e) = invalidate.await {
                            tracing::warn!("[ffmpeg] Failed to invalidate segment: {}", e);
                        }
                    }
                } else {
                    // Just drain and discard if there's no audio pipe but we still got audio segments
                    while let Some((_, _, invalidate)) = audio_receiver.recv().await {
                        if recycle && let Err(e) = invalidate.await {
                            tracing::warn!("[ffmpeg] Failed to invalidate segment: {}", e);
                        }
                    }
                }
            });

            let mut buffered = SegmentBuffer::new(buffer_segments);
            while let Some((_, segment)) = buffered.next(&mut stream).await {
                if let Some((reader, r#type, invalidate)) = segment {
                    match r#type {
                        StreamType::Video => {
                            if video_sender.send((reader, r#type, invalidate)).is_err() {
                                tracing::debug!("[ffmpeg] video receiver dropped, stopping mux");
                                break;
                            }
                        }
                        StreamType::Audio => {
                            if audio_sender.send((reader, r#type, invalidate)).is_err() {
                                tracing::debug!("[ffmpeg] audio receiver dropped, stopping mux");
                                break;
                            }
                        }
                        StreamType::Subtitle | StreamType::Unknown => {
                            if recycle {
                                _ = invalidate.await;
                            }
                        }
                    }
                }
            }

            tracing::debug!("Waiting for video handler...");
            drop(video_sender);
            video_handle.await.unwrap();

            tracing::debug!("Waiting for audio handler...");
            drop(audio_sender);
            audio_handle.await.unwrap();
        });

        Self {
            recycle,

            sender: Some(tx),
            future: Some(future),
        }
    }

    fn send(&self, message: (u64, u64, Option<SendSegment>)) -> Result<(), ()> {
        if let Some(sender) = &self.sender {
            sender.send(message).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

impl Merger for PipeMerger {
    type Result = ();

    async fn update(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        let stream_id = segment.stream_id;
        let sequence = segment.sequence;
        let stream_type = segment.stream_type;
        let reader = cache.open_reader(&segment).await?;
        let invalidate = async move { cache.invalidate(&segment).await };

        if self
            .send((
                stream_id,
                sequence,
                Some((Box::pin(reader), stream_type, Box::pin(invalidate))),
            ))
            .is_err()
        {
            tracing::warn!("[ffmpeg] pipe closed, dropping segment");
            return Err(crate::error::IoriError::IOError(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Pipe to ffmpeg was closed",
            )));
        }

        Ok(())
    }

    async fn fail(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        let stream_id = segment.stream_id;
        cache.invalidate(&segment).await?;

        let _ = self.send((stream_id, segment.sequence, None));

        Ok(())
    }

    async fn finish(&mut self, cache: impl CacheSource) -> IoriResult<Self::Result> {
        // drop the sender so that the future can finish
        drop(self.sender.take());

        self.future
            .take()
            .unwrap()
            .await
            .expect("Failed to join pipe");

        if self.recycle {
            cache.clear().await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SegmentBuffer, readrate_args};
    use crate::util::ordered_stream::OrderedStream;
    use tokio::sync::mpsc;

    #[test]
    fn readrate_args_are_compatible_with_older_ffmpeg() {
        assert_eq!(readrate_args(false), &["-re"]);
        assert_eq!(readrate_args(true), &["-re", "-readrate_catchup", "1.25"]);
    }

    #[tokio::test]
    async fn segment_buffer_drains_primed_items_during_input_pause() {
        let (sender, receiver) = mpsc::unbounded_channel();
        for sequence in 0..4 {
            sender.send((0, sequence, sequence)).unwrap();
        }
        drop(sender);

        let mut stream = OrderedStream::new(receiver);
        let mut buffer = SegmentBuffer::new(3);

        assert_eq!(buffer.next(&mut stream).await, Some((0, 0)));
        assert_eq!(buffer.next(&mut stream).await, Some((0, 1)));
        assert_eq!(buffer.next(&mut stream).await, Some((0, 2)));
        assert_eq!(buffer.next(&mut stream).await, Some((0, 3)));
        assert_eq!(buffer.next(&mut stream).await, None);
    }
}
