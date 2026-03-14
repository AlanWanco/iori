use futures::{Stream, stream};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc};
use url::Url;

use crate::{
    StreamingSource,
    context::IoriContext,
    error::{IoriError, IoriResult},
    hls::{segment::M3u8Segment, source::HlsPlaylistSource},
    util::mix::VecMix,
};

pub struct HlsLiveSource {
    playlist: Arc<Mutex<HlsPlaylistSource>>,
    /// If set, only keep the last N segments from the first playlist fetch.
    /// Useful for reducing initial latency when piping to ffmpeg for restreaming.
    initial_segment_limit: Option<usize>,
}

impl HlsLiveSource {
    pub fn new(m3u8_url: String, key: Option<&str>) -> IoriResult<Self> {
        Ok(Self {
            playlist: Arc::new(Mutex::new(HlsPlaylistSource::new(
                Url::parse(&m3u8_url)?,
                key,
            ))),
            initial_segment_limit: None,
        })
    }

    /// Set the maximum number of segments to keep from the first playlist fetch.
    /// Only the last `limit` segments will be downloaded initially;
    /// subsequent fetches continue from there as normal.
    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.initial_segment_limit = limit;
        self
    }
}

impl StreamingSource for HlsLiveSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        let mut latest_media_sequences = self.playlist.lock().await.load_streams(context).await?;

        let (sender, receiver) = mpsc::unbounded_channel();

        let playlist = self.playlist.clone();
        let context = context.clone();
        let initial_segment_limit = self.initial_segment_limit;
        tokio::spawn(async move {
            let mut is_first_fetch = true;
            loop {
                if sender.is_closed() {
                    break;
                }

                let before_load = tokio::time::Instant::now();
                let (mut segments, is_end) = match playlist
                    .lock()
                    .await
                    .load_segments(&context, &latest_media_sequences)
                    .await
                {
                    Ok(v) => v,
                    Err(IoriError::ManifestFetchError) => {
                        tracing::error!("Exceeded retry limit for fetching segments, exiting...");
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch segments: {e}");
                        break;
                    }
                };

                // On the first fetch, truncate each stream's segments to the last N
                // so that we start close to the live edge instead of from the beginning.
                if is_first_fetch {
                    is_first_fetch = false;
                    if let Some(limit) = initial_segment_limit {
                        let mut new_sequence_starts = Vec::with_capacity(segments.len());
                        let mut did_truncate = false;
                        for stream_segments in segments.iter_mut() {
                            let len = stream_segments.len();
                            if len > limit {
                                let skipped = len - limit;
                                tracing::info!(
                                    "Initial segment limit: keeping last {limit} of {len} segments (skipping {skipped})"
                                );
                                *stream_segments = stream_segments.split_off(skipped);
                                // Re-number sequences starting from 0 so that
                                // OrderedStream (which expects seq to start at 0)
                                // can output them immediately.
                                for (i, seg) in stream_segments.iter_mut().enumerate() {
                                    seg.sequence = i as u64;
                                }
                                new_sequence_starts.push(stream_segments.len() as u64);
                                did_truncate = true;
                            } else {
                                new_sequence_starts.push(stream_segments.len() as u64);
                            }
                        }
                        // Reset the source's internal sequence counters so that
                        // subsequent fetches produce sequences continuing from
                        // where the truncated batch left off.
                        if did_truncate {
                            playlist.lock().await.reset_stream_sequences(&new_sequence_starts);
                        }
                    }
                }

                let segments_average_duration = segments
                    .iter()
                    .map(|ss| {
                        let total_seconds = ss.iter().map(|s| s.duration).sum::<f64>();
                        let segments_count = ss.len() as f64;

                        if segments_count == 0. {
                            0
                        } else {
                            (total_seconds * 1000. / segments_count) as u64
                        }
                    })
                    .min()
                    .unwrap_or(5);

                for (segments, latest_media_sequence) in
                    segments.iter().zip(latest_media_sequences.iter_mut())
                {
                    *latest_media_sequence = segments
                        .last()
                        .map(|r| r.media_sequence)
                        .or(*latest_media_sequence);
                }

                let mixed_segments = segments.mix();
                if !mixed_segments.is_empty()
                    && let Err(e) = sender.send(Ok(mixed_segments))
                {
                    tracing::error!("Failed to send mixed segments: {e}");
                    break;
                }

                if is_end {
                    break;
                }

                // playlist does not end, wait for a while and fetch again
                let seconds_to_wait = segments_average_duration.clamp(1000, 5000);
                tokio::time::sleep_until(before_load + Duration::from_millis(seconds_to_wait))
                    .await;
            }
        });

        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}
