use futures::{Stream, stream};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc};
use url::Url;

use crate::{
    StreamingSource,
    context::IoriContext,
    error::{IoriError, IoriResult},
    hls::{segment::M3u8Segment, source::HlsPlaylistSource},
    util::mix::VecMix,
};

const MANIFEST_RECOVERY_DELAY: Duration = Duration::from_secs(2);

type ManifestRecovery =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Url>> + Send>> + Send + Sync>;

pub struct HlsLiveSource {
    playlist: Arc<Mutex<HlsPlaylistSource>>,
    manifest_recovery: Option<ManifestRecovery>,
}

impl HlsLiveSource {
    pub fn new(m3u8_url: String, key: Option<&str>) -> IoriResult<Self> {
        Ok(Self {
            playlist: Arc::new(Mutex::new(HlsPlaylistSource::new(
                Url::parse(&m3u8_url)?,
                key,
            ))),
            manifest_recovery: None,
        })
    }

    /// Configure a callback that returns a replacement playlist URL after
    /// manifest retries are exhausted.
    pub fn with_manifest_recovery<F, Fut>(mut self, recovery: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Url>> + Send + 'static,
    {
        self.manifest_recovery = Some(Arc::new(move || Box::pin(recovery())));
        self
    }
}

async fn recover_manifest_url(
    playlist: &Arc<Mutex<HlsPlaylistSource>>,
    context: &IoriContext,
    recovery: &ManifestRecovery,
) -> bool {
    tracing::info!("Attempting HLS playlist URL recovery after manifest retries.");
    let Some(url) = recovery().await else {
        tracing::warn!("HLS playlist URL recovery did not return a replacement.");
        return false;
    };

    match playlist.lock().await.update_url(context, url).await {
        Ok(true) => {
            tracing::info!("HLS playlist URL recovery succeeded.");
            true
        }
        Ok(false) => {
            tracing::warn!("HLS playlist URL recovery returned an incompatible playlist.");
            false
        }
        Err(_) => {
            tracing::warn!("Failed to activate the recovered HLS playlist URL.");
            false
        }
    }
}

impl StreamingSource for HlsLiveSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        let playlist = self.playlist.clone();
        let initial_recovery = self.manifest_recovery.clone();
        let mut initial_recovery_attempted = false;
        let mut latest_media_sequences = loop {
            let load_result = {
                let mut playlist = playlist.lock().await;
                playlist.load_streams(context).await
            };
            match load_result {
                Ok(media_sequences) => break media_sequences,
                Err(IoriError::ManifestFetchError) => {
                    if !initial_recovery_attempted && let Some(recovery) = initial_recovery.as_ref()
                    {
                        initial_recovery_attempted = true;
                        tokio::time::sleep(MANIFEST_RECOVERY_DELAY).await;
                        if recover_manifest_url(&playlist, context, recovery).await {
                            continue;
                        }
                    }
                    return Err(IoriError::ManifestFetchError);
                }
                Err(error) => return Err(error),
            }
        };

        let (sender, receiver) = mpsc::unbounded_channel();

        let context = context.clone();
        let manifest_recovery = self.manifest_recovery.clone();
        tokio::spawn(async move {
            loop {
                if sender.is_closed() {
                    break;
                }

                let before_load = tokio::time::Instant::now();
                let load_result = {
                    let mut playlist = playlist.lock().await;
                    playlist
                        .load_segments(&context, &latest_media_sequences)
                        .await
                };
                let (segments, is_end) = match load_result {
                    Ok(v) => v,
                    Err(IoriError::ManifestFetchError) => {
                        if let Some(recovery) = manifest_recovery.as_ref() {
                            tokio::time::sleep(MANIFEST_RECOVERY_DELAY).await;
                            if recover_manifest_url(&playlist, &context, recovery).await {
                                continue;
                            }
                        }
                        tracing::error!("Exceeded retry limit for fetching HLS manifests.");
                        break;
                    }
                    Err(_) => {
                        tracing::error!("Failed to process HLS playlist segments.");
                        break;
                    }
                };

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
