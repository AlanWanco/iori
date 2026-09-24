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

#[derive(Clone)]
pub struct HlsLiveSource {
    playlist: Arc<Mutex<HlsPlaylistSource>>,
    /// If set, only keep the last N segments from the first playlist fetch.
    /// Useful for reducing initial latency when piping to ffmpeg for restreaming.
    initial_segment_limit: Option<usize>,
    /// If set, stop polling a live playlist after this long without new segments.
    idle_timeout: Option<Duration>,
    /// Optional callback used to obtain a replacement playlist URL after a
    /// manifest becomes unavailable.
    manifest_recovery: Option<ManifestRecovery>,
    /// Delay between retries of an unavailable live manifest. When configured,
    /// initial startup polls the current URL before requesting a replacement.
    manifest_retry_interval: Option<Duration>,
}

impl HlsLiveSource {
    pub fn new(m3u8_url: String, key: Option<&str>) -> IoriResult<Self> {
        Ok(Self {
            playlist: Arc::new(Mutex::new(HlsPlaylistSource::new(
                Url::parse(&m3u8_url)?,
                key,
            ))),
            initial_segment_limit: None,
            idle_timeout: None,
            manifest_recovery: None,
            manifest_retry_interval: None,
        })
    }

    /// Set the maximum number of segments to keep from the first playlist fetch.
    /// Only the last `limit` segments will be downloaded initially;
    /// subsequent fetches continue from there as normal.
    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.initial_segment_limit = limit;
        self
    }

    /// Stop polling when no new segments arrive within `timeout`.
    pub fn with_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Configure a callback that can provide a replacement playlist URL after
    /// a manifest fetch fails. The callback is only invoked after the normal
    /// manifest retry budget is exhausted.
    pub fn with_manifest_recovery<F, Fut>(mut self, recovery: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Url>> + Send + 'static,
    {
        self.manifest_recovery = Some(Arc::new(move || Box::pin(recovery())));
        self
    }

    /// Set the delay between retries while waiting for a live manifest.
    /// During initial startup, retry the current URL before requesting a new one.
    pub fn with_manifest_retry_interval(mut self, interval: Option<Duration>) -> Self {
        self.manifest_retry_interval = interval.filter(|duration| !duration.is_zero());
        self
    }

    /// Replace the active playlist URL without resetting segment sequence state.
    pub async fn update_playlist_url(
        &self,
        context: &IoriContext,
        m3u8_url: &str,
    ) -> IoriResult<bool> {
        let url = Url::parse(m3u8_url)?;
        self.playlist.lock().await.update_url(context, url).await
    }
}

async fn recover_manifest_url(
    playlist: &Arc<Mutex<HlsPlaylistSource>>,
    context: &IoriContext,
    recovery: &ManifestRecovery,
) -> bool {
    tracing::info!("Attempting HLS playlist URL recovery after a manifest fetch failure.");

    let Some(url) = recovery().await else {
        tracing::warn!("HLS playlist URL recovery did not produce a replacement URL.");
        return false;
    };

    match playlist.lock().await.update_url(context, url).await {
        Ok(true) => {
            tracing::info!("HLS playlist URL recovery succeeded; resuming live polling.");
            true
        }
        Ok(false) => {
            tracing::warn!("HLS playlist URL recovery returned an unusable replacement URL.");
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
        let manifest_retry_interval = self.manifest_retry_interval;
        let initial_wait_started = tokio::time::Instant::now();
        let mut latest_media_sequences = loop {
            let load_result = {
                let mut playlist = playlist.lock().await;
                playlist.load_streams(context).await
            };
            match load_result {
                Ok(media_sequences) => break media_sequences,
                Err(error @ IoriError::ManifestFetchError) => {
                    if let Some(interval) = manifest_retry_interval {
                        if let Some(timeout) = self.idle_timeout {
                            let elapsed = initial_wait_started.elapsed();
                            if elapsed >= timeout {
                                return Err(error);
                            }
                            let remaining = timeout - elapsed;
                            if interval >= remaining {
                                tokio::time::sleep(remaining).await;
                                return Err(error);
                            }
                        }
                        tracing::info!(
                            "Live HLS playlist is not available yet; retrying the current URL in {} seconds.",
                            interval.as_secs_f64()
                        );
                        tokio::time::sleep(interval).await;
                        continue;
                    }
                    if let Some(recovery) = initial_recovery.as_ref() {
                        tokio::time::sleep(MANIFEST_RECOVERY_DELAY).await;
                        if recover_manifest_url(&playlist, context, recovery).await {
                            continue;
                        }
                        continue;
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        };

        let (sender, receiver) = mpsc::unbounded_channel();

        let context = context.clone();
        let manifest_recovery = self.manifest_recovery.clone();
        let manifest_retry_interval = self.manifest_retry_interval;
        let initial_segment_limit = self.initial_segment_limit;
        let idle_timeout = self.idle_timeout;
        tokio::spawn(async move {
            let mut is_first_fetch = true;
            let mut last_new_segment_at = tokio::time::Instant::now();
            let mut consecutive_manifest_failures = 0u32;
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
                let (mut segments, is_end) = match load_result {
                    Ok(v) => v,
                    Err(IoriError::ManifestFetchError) => {
                        let retry_delay =
                            manifest_retry_interval.unwrap_or(MANIFEST_RECOVERY_DELAY);
                        let mut waited_for_retry = false;
                        if let Some(recovery) = manifest_recovery.as_ref() {
                            tokio::time::sleep(retry_delay).await;
                            waited_for_retry = manifest_retry_interval.is_some();
                            if recover_manifest_url(&playlist, &context, recovery).await {
                                consecutive_manifest_failures = 0;
                                continue;
                            }
                        }

                        consecutive_manifest_failures =
                            consecutive_manifest_failures.saturating_add(1);
                        tracing::warn!(
                            "Exceeded retry limit for fetching segments; waiting {} seconds before retrying live playlist (consecutive failures: {}).",
                            retry_delay.as_secs(),
                            consecutive_manifest_failures
                        );
                        if let Some(timeout) = idle_timeout
                            && last_new_segment_at.elapsed() >= timeout
                        {
                            tracing::warn!(
                                "No new HLS segments received for {} seconds while recovering from manifest failures; stopping live playlist polling.",
                                timeout.as_secs()
                            );
                            break;
                        }
                        if !waited_for_retry {
                            tokio::time::sleep(retry_delay).await;
                        }
                        continue;
                    }
                    Err(e) if e.is_transient_network_error() => {
                        let retry_delay =
                            manifest_retry_interval.unwrap_or(MANIFEST_RECOVERY_DELAY);
                        consecutive_manifest_failures =
                            consecutive_manifest_failures.saturating_add(1);
                        tracing::warn!(
                            "Failed to process live playlist segments due to a transient network error; waiting {} seconds before retrying (consecutive failures: {}).",
                            retry_delay.as_secs(),
                            consecutive_manifest_failures
                        );
                        if let Some(timeout) = idle_timeout
                            && last_new_segment_at.elapsed() >= timeout
                        {
                            tracing::warn!(
                                "No new HLS segments received for {} seconds while recovering from transient network errors; stopping live playlist polling.",
                                timeout.as_secs()
                            );
                            break;
                        }
                        tokio::time::sleep(retry_delay).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("Failed to process live playlist segments.");
                        if sender.send(Err(e)).is_err() {
                            tracing::debug!("Failed to report live playlist segment error");
                        }
                        break;
                    }
                };
                consecutive_manifest_failures = 0;

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
                            playlist
                                .lock()
                                .await
                                .reset_stream_sequences(&new_sequence_starts);
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

                let has_new_segments = segments.iter().any(|s| !s.is_empty());
                if has_new_segments {
                    last_new_segment_at = tokio::time::Instant::now();
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

                if let Some(timeout) = idle_timeout
                    && !has_new_segments
                    && last_new_segment_at.elapsed() >= timeout
                {
                    tracing::warn!(
                        "No new HLS segments received for {} seconds; stopping live playlist polling.",
                        timeout.as_secs()
                    );
                    break;
                }

                // playlist does not end, wait for a while and fetch again
                // Be more aggressive when new segments are flowing to reduce live latency.
                let seconds_to_wait = if has_new_segments {
                    300
                } else {
                    segments_average_duration.clamp(800, 4000)
                };
                tokio::time::sleep_until(before_load + Duration::from_millis(seconds_to_wait))
                    .await;
            }
        });

        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}
