mod auto;
mod concat;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
mod pipe;
mod skip;

pub use auto::AutoMerger;
pub use concat::ConcatAfterMerger;
pub use pipe::PipeMerger;
pub use skip::SkipMerger;
use tokio::io::AsyncWrite;

use crate::{SegmentInfo, cache::CacheSource, error::IoriResult};
use std::path::PathBuf;

pub trait Merger {
    /// Result of the merge.
    type Result: Send + Sync + 'static;

    /// Add a segment to the merger.
    ///
    /// This method might not be called in order of segment sequence.
    /// Implementations should handle order of segments by calling
    /// [StreamingSegment::sequence].
    fn update(
        &mut self,
        segment: SegmentInfo,
        cache: impl CacheSource,
    ) -> impl Future<Output = IoriResult<()>> + Send;

    /// Tell the merger that a segment has failed to download.
    fn fail(
        &mut self,
        segment: SegmentInfo,
        cache: impl CacheSource,
    ) -> impl Future<Output = IoriResult<()>> + Send;

    fn finish(
        &mut self,
        cache: impl CacheSource,
    ) -> impl Future<Output = IoriResult<Self::Result>> + Send;
}

pub enum IoriMerger {
    Pipe(PipeMerger),
    Skip(SkipMerger),
    Concat(ConcatAfterMerger),
    Auto(AutoMerger),
}

impl IoriMerger {
    pub fn pipe(recycle: bool) -> Self {
        Self::Pipe(PipeMerger::stdout(recycle))
    }

    pub fn pipe_to_writer(
        writer: impl AsyncWrite + Unpin + Send + Sync + 'static,
        recycle: bool,
    ) -> Self {
        Self::Pipe(PipeMerger::writer(recycle, writer))
    }

    pub fn pipe_to_file(output_file: PathBuf, recycle: bool) -> Self {
        Self::Pipe(PipeMerger::file(recycle, output_file))
    }

    pub fn pipe_mux(output_file: PathBuf, recycle: bool, extra_commands: Option<String>) -> Self {
        Self::Pipe(PipeMerger::mux(recycle, output_file, extra_commands))
    }

    pub fn skip() -> Self {
        Self::Skip(SkipMerger)
    }

    pub fn concat(output_file: PathBuf, recycle: bool) -> Self {
        Self::Concat(ConcatAfterMerger::new(output_file, recycle))
    }

    pub fn auto(output_file: PathBuf, recycle: bool) -> Self {
        Self::Auto(AutoMerger::new(output_file, recycle))
    }
}

impl Merger for IoriMerger {
    type Result = (); // TODO: merger might have different result types

    async fn update(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        match self {
            Self::Pipe(merger) => merger.update(segment, cache).await,
            Self::Skip(merger) => merger.update(segment, cache).await,
            Self::Concat(merger) => merger.update(segment, cache).await,
            Self::Auto(merger) => merger.update(segment, cache).await,
        }
    }

    async fn fail(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        match self {
            Self::Pipe(merger) => merger.fail(segment, cache).await,
            Self::Skip(merger) => merger.fail(segment, cache).await,
            Self::Concat(merger) => merger.fail(segment, cache).await,
            Self::Auto(merger) => merger.fail(segment, cache).await,
        }
    }

    async fn finish(&mut self, cache: impl CacheSource) -> IoriResult<Self::Result> {
        match self {
            Self::Pipe(merger) => merger.finish(cache).await,
            Self::Skip(merger) => merger.finish(cache).await,
            Self::Concat(merger) => merger.finish(cache).await,
            Self::Auto(merger) => merger.finish(cache).await,
        }
    }
}
