mod mux;

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
        .kill_on_drop(true)
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
/// Missing segments are skipped. Raw pipe/file output cannot handle
/// discontinuities. The FFmpeg mux output owns a coordinated, replayable A/V
/// restart path, but does not guarantee gapless delivery to remote receivers.
pub struct PipeMerger {
    recycle: bool,

    sender: Option<mpsc::UnboundedSender<(u64, u64, Option<SendSegment>)>>,
    future: Option<JoinHandle<()>>,
    mux: Option<mux::Mux>,
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
            mux: None,
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
            mux: None,
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
        Self {
            recycle,
            sender: None,
            future: None,
            mux: Some(mux::Mux::new(
                recycle,
                output,
                extra_command,
                has_audio,
                buffer_segments,
            )),
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

    fn cancellation_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.mux.as_ref().map(mux::Mux::cancellation_token)
    }

    async fn update(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        if let Some(mux) = &self.mux {
            return mux.send(segment, cache, false);
        }
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
        if let Some(mux) = &self.mux {
            return mux.send(segment, cache, true);
        }
        let stream_id = segment.stream_id;
        cache.invalidate(&segment).await?;

        let _ = self.send((stream_id, segment.sequence, None));

        Ok(())
    }

    async fn finish(&mut self, cache: impl CacheSource) -> IoriResult<Self::Result> {
        if let Some(mux) = &mut self.mux {
            mux.finish().await?;
            if self.recycle && !mux.was_stopped() {
                cache.clear().await?;
            }
            return Ok(());
        }
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
