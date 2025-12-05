use std::sync::Arc;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::{
    IoriError, SegmentInfo, StreamingSource, WriteSegment, cache::CacheSource,
    context::IoriContext, error::IoriResult, merge::Merger,
};

pub struct SequencialDownloader<S, M, C>
where
    S: StreamingSource,
    M: Merger,
    C: CacheSource,
{
    context: IoriContext,

    source: S,
    merger: M,
    cache: Arc<C>,
}

impl<S, M, C> SequencialDownloader<S, M, C>
where
    S: StreamingSource,
    M: Merger,
    C: CacheSource,
{
    pub fn new(context: IoriContext, source: S, merger: M, cache: C) -> Self {
        Self {
            context,
            source,
            merger,
            cache: Arc::new(cache),
        }
    }

    pub async fn download(&mut self) -> IoriResult<()> {
        let stream = self.source.segments_stream(&self.context).await?;
        tokio::pin!(stream);

        while let Some(segment) = stream.next().await {
            for segment in segment? {
                let segment_info = SegmentInfo::from(&segment);
                let writer = self.cache.open_writer(&segment_info).await?;
                let Some(mut writer) = writer else {
                    continue;
                };

                let fetch_result = segment.write_segment(&self.context, &mut writer).await;
                let fetch_result = match fetch_result {
                    // graceful shutdown
                    Ok(_) => writer.shutdown().await.map_err(IoriError::IOError),
                    Err(e) => Err(e),
                };
                drop(writer);

                match fetch_result {
                    Ok(_) => self.merger.update(segment_info, self.cache.clone()).await?,
                    Err(_) => self.merger.fail(segment_info, self.cache.clone()).await?,
                }
            }
        }

        self.merger.finish(self.cache.clone()).await?;
        Ok(())
    }
}
