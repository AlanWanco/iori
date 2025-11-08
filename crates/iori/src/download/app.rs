use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

use futures::lock::Mutex;

use crate::{IoriResult, SegmentInfo};

pub trait DownloaderApp {
    fn on_start(&self) -> impl Future<Output = IoriResult<()>> + Send;

    fn on_receive_segments(&self, segments: &[SegmentInfo]) -> impl Future<Output = ()> + Send;

    fn on_downloaded_segment(&self, segment: &SegmentInfo) -> impl Future<Output = ()> + Send;

    fn on_failed_segment(&self, segment: &SegmentInfo) -> impl Future<Output = ()> + Send;

    fn on_finished(&self) -> impl Future<Output = IoriResult<()>> + Send;
}

impl DownloaderApp for () {
    async fn on_start(&self) -> IoriResult<()> {
        Ok(())
    }

    async fn on_receive_segments(&self, _segments: &[SegmentInfo]) {}

    async fn on_downloaded_segment(&self, _segment: &SegmentInfo) {}

    async fn on_failed_segment(&self, _segment: &SegmentInfo) {}

    async fn on_finished(&self) -> IoriResult<()> {
        Ok(())
    }
}

#[derive(Default)]
pub struct TracingApp {
    concurrency: Option<NonZeroU32>,

    total: AtomicUsize,
    downloaded: AtomicUsize,
    failed: AtomicUsize,
    failed_segments_name: Mutex<Vec<String>>,
}

impl TracingApp {
    pub fn concurrent(concurrency: NonZeroU32) -> Self {
        Self {
            concurrency: Some(concurrency),
            ..Default::default()
        }
    }
}

impl DownloaderApp for TracingApp {
    async fn on_start(&self) -> IoriResult<()> {
        if let Some(concurrency) = self.concurrency {
            tracing::info!("Start downloading with {} thread(s).", concurrency.get());
        } else {
            tracing::info!("Start downloading.");
        }
        Ok(())
    }

    async fn on_receive_segments(&self, segments: &[SegmentInfo]) {
        self.total.fetch_add(segments.len(), Ordering::Relaxed);
        tracing::info!("{} new segments were added to queue.", segments.len());
    }

    async fn on_downloaded_segment(&self, segment: &SegmentInfo) {
        let filename = &segment.file_name;
        let downloaded = self.downloaded.fetch_add(1, Ordering::Relaxed)
            + 1
            + self.failed.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let percentage = if total == 0 {
            0.
        } else {
            downloaded as f32 / total as f32 * 100.
        };
        tracing::info!(
            "Processing {filename} finished. ({downloaded} / {total} or {percentage:.2}%)"
        );
    }

    async fn on_failed_segment(&self, segment: &SegmentInfo) {
        let filename = &segment.file_name;

        self.failed_segments_name
            .lock()
            .await
            .push(filename.to_string());
        self.failed.fetch_add(1, Ordering::Relaxed);

        tracing::error!("Processing {filename} failed, max retries exceed, drop.");
    }

    async fn on_finished(&self) -> IoriResult<()> {
        let failed = self.failed_segments_name.lock().await;
        if !failed.is_empty() {
            tracing::error!("Failed to download {} segments:", failed.len());
            for segment in failed.iter() {
                tracing::error!("  - {}", segment);
            }
        }
        Ok(())
    }
}
