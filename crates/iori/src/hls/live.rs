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
}

impl HlsLiveSource {
    pub fn new(m3u8_url: String, key: Option<&str>) -> IoriResult<Self> {
        Ok(Self {
            playlist: Arc::new(Mutex::new(HlsPlaylistSource::new(
                Url::parse(&m3u8_url)?,
                key,
            ))),
        })
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
        tokio::spawn(async move {
            loop {
                if sender.is_closed() {
                    break;
                }

                let before_load = tokio::time::Instant::now();
                let (segments, is_end) = match playlist
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

                let segments_average_duration = segments
                    .iter()
                    .map(|ss| {
                        let total_seconds = ss.iter().map(|s| s.duration).sum::<f32>();
                        let segments_count = ss.len() as f32;

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
