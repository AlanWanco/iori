use std::{num::ParseIntError, str::FromStr, sync::Arc};

use futures::{Stream, stream};
use tokio::sync::Mutex;
use url::Url;

use crate::{
    StreamingSource,
    context::IoriContext,
    error::IoriResult,
    hls::{segment::M3u8Segment, source::HlsPlaylistSource},
};

pub struct CommonM3u8ArchiveSource {
    playlist: Arc<Mutex<HlsPlaylistSource>>,
    range: SegmentRange,
}

/// A subrange for m3u8 archive sources to choose which segment to use
#[derive(Debug, Clone, Copy)]
pub struct SegmentRange {
    /// Start offset to use. Default to 1
    pub start: u64,
    /// End offset to use. Default to None
    pub end: Option<u64>,
}

impl Default for SegmentRange {
    fn default() -> Self {
        Self {
            start: 1,
            end: None,
        }
    }
}

impl SegmentRange {
    pub fn new(start: u64, end: Option<u64>) -> Self {
        Self { start, end }
    }

    pub fn end(&self) -> u64 {
        self.end.unwrap_or(u64::MAX)
    }
}

impl FromStr for SegmentRange {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (start, end) = s.split_once('-').unwrap_or((s, ""));
        let start = if start.is_empty() { 1 } else { start.parse()? };
        let end = if end.is_empty() {
            None
        } else {
            Some(end.parse()?)
        };
        Ok(Self { start, end })
    }
}

impl CommonM3u8ArchiveSource {
    pub fn new(playlist_url: String, key: Option<&str>, range: SegmentRange) -> IoriResult<Self> {
        Ok(Self {
            playlist: Arc::new(Mutex::new(HlsPlaylistSource::new(
                Url::parse(&playlist_url)?,
                key,
            ))),
            range,
        })
    }
}

impl StreamingSource for CommonM3u8ArchiveSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        let latest_media_sequences = self.playlist.lock().await.load_streams(context).await?;

        let (segments, _) = self
            .playlist
            .lock()
            .await
            .load_segments(context, &latest_media_sequences)
            .await?;
        let mut segments: Vec<_> = segments
            .into_iter()
            .flatten()
            .filter_map(|segment| {
                let seq = segment.sequence + 1;
                if seq >= self.range.start && seq <= self.range.end() {
                    return Some(segment);
                }
                None
            })
            .collect();

        // make sequence start form 1 again
        for (seq, segment) in segments.iter_mut().enumerate() {
            segment.sequence = seq as u64;
        }

        Ok(Box::pin(stream::once(async move { Ok(segments) })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range() {
        let range = "1-10".parse::<SegmentRange>().unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.end, Some(10));

        let range = "1-".parse::<SegmentRange>().unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.end, None);

        let range = "-10".parse::<SegmentRange>().unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.end, Some(10));

        let range = "1".parse::<SegmentRange>().unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.end, None);
    }
}
