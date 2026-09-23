use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use iori::{
    IoriResult, SegmentInfo, StreamType,
    cache::{CacheSource, memory::MemoryCacheSource},
    download::{ParallelDownloader, TracingApp},
    merge::{Merger, SkipMerger},
};
use tokio::sync::{Mutex, oneshot};

use crate::source::{TestSegment, TestSource};

struct RecordingMerger(Arc<Mutex<Vec<(u64, bool)>>>);

impl Merger for RecordingMerger {
    type Result = ();

    async fn update(&mut self, segment: SegmentInfo, _cache: impl CacheSource) -> IoriResult<()> {
        self.0.lock().await.push((segment.stream_id, true));
        Ok(())
    }

    async fn fail(&mut self, segment: SegmentInfo, cache: impl CacheSource) -> IoriResult<()> {
        cache.invalidate(&segment).await?;
        self.0.lock().await.push((segment.stream_id, false));
        Ok(())
    }

    async fn finish(&mut self, _cache: impl CacheSource) -> IoriResult<Self::Result> {
        Ok(())
    }
}

#[tokio::test]
async fn test_parallel_downloader_with_failed_retry() -> anyhow::Result<()> {
    let source = TestSource::new(vec![
        TestSegment::new(1, 1, "test.ts".to_string()).with_fail_count(2),
    ]);

    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .retries(1)
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_with_success_retry() -> anyhow::Result<()> {
    let source = TestSource::new(vec![
        TestSegment::new(1, 1, "test.ts".to_string()).with_fail_count(2),
    ]);

    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .retries(3)
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 1);

    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_fails_synchronized_pair_atomically() -> anyhow::Result<()> {
    let key = (0, 100);
    let segments = vec![
        TestSegment::new(0, 0, "video.ts".to_string())
            .with_stream_type(StreamType::Video)
            .with_synchronization_key(key)
            .with_fail_count(1),
        TestSegment::new(1, 0, "audio.m4a".to_string())
            .with_stream_type(StreamType::Audio)
            .with_synchronization_key(key),
    ];
    let source = TestSource::new(segments);
    let cache = Arc::new(MemoryCacheSource::new());
    let events = Arc::new(Mutex::new(Vec::new()));

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(RecordingMerger(events.clone()))
        .cache(cache.clone())
        .concurrency(NonZeroU32::new(1).unwrap())
        .retries(1)
        .ctrlc_handler()
        .download(source)
        .await?;

    assert!(cache.into_inner().lock().unwrap().is_empty());
    let events = events.lock().await;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|(_, succeeded)| !succeeded));
    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_commits_synchronized_pair_together() -> anyhow::Result<()> {
    let key = (0, 100);
    let segments = vec![
        TestSegment::new(0, 0, "video.ts".to_string())
            .with_stream_type(StreamType::Video)
            .with_synchronization_key(key),
        TestSegment::new(1, 0, "audio.m4a".to_string())
            .with_stream_type(StreamType::Audio)
            .with_synchronization_key(key),
    ];
    let source = TestSource::new(segments);
    let cache = Arc::new(MemoryCacheSource::new());
    let events = Arc::new(Mutex::new(Vec::new()));

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(RecordingMerger(events.clone()))
        .cache(cache.clone())
        .concurrency(NonZeroU32::new(1).unwrap())
        .retries(1)
        .ctrlc_handler()
        .download(source)
        .await?;

    assert_eq!(cache.into_inner().lock().unwrap().len(), 2);
    let events = events.lock().await;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|(_, succeeded)| *succeeded));
    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_does_not_pair_different_keys() -> anyhow::Result<()> {
    let segments = vec![
        TestSegment::new(0, 0, "video.ts".to_string())
            .with_stream_type(StreamType::Video)
            .with_synchronization_key((0, 100))
            .with_fail_count(1),
        TestSegment::new(1, 0, "audio.m4a".to_string())
            .with_stream_type(StreamType::Audio)
            .with_synchronization_key((0, 101)),
    ];
    let source = TestSource::new(segments);
    let cache = Arc::new(MemoryCacheSource::new());
    let events = Arc::new(Mutex::new(Vec::new()));

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(RecordingMerger(events.clone()))
        .cache(cache.clone())
        .concurrency(NonZeroU32::new(1).unwrap())
        .retries(1)
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    assert_eq!(result.lock().unwrap().len(), 1);
    let events = events.lock().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events.iter().filter(|(_, succeeded)| *succeeded).count(), 1);
    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_concurrency() -> anyhow::Result<()> {
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let max_concurrent = Arc::new(std::sync::atomic::AtomicU32::new(0));

    let segments = (0..10)
        .map(|i| {
            TestSegment::new(1, i, format!("test{}.ts", i))
                .with_delay(Duration::from_millis(100))
                .with_counters(counter.clone(), max_concurrent.clone())
        })
        .collect::<Vec<_>>();

    let source = TestSource::new(segments);
    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .concurrency(NonZeroU32::new(5).unwrap())
        .ctrlc_handler()
        .download(source)
        .await?;

    // Max concurrency should be at most 5
    let max = max_concurrent.load(Ordering::SeqCst);
    println!("Max concurrent: {}", max);
    assert!(max <= 5);
    assert!(max > 1);

    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_stop_signal() -> anyhow::Result<()> {
    let segments = (0..10)
        .map(|i| {
            TestSegment::new(1, i, format!("test{}.ts", i)).with_delay(Duration::from_millis(100))
        })
        .collect::<Vec<_>>();

    let source = TestSource::new(segments);
    let cache = Arc::new(MemoryCacheSource::new());
    let (tx, rx) = oneshot::channel();

    let downloader_handle = tokio::spawn(async move {
        ParallelDownloader::builder(Default::default())
            .app(TracingApp::default())
            .merger(SkipMerger)
            .cache(cache.clone())
            .stop_signal(rx)
            .download(source)
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    tx.send(()).unwrap();

    let res = downloader_handle.await?;
    assert!(res.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_empty_stream() -> anyhow::Result<()> {
    let source = TestSource::new(vec![]);
    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_parallel_downloader_live_simulation() -> anyhow::Result<()> {
    let batch1 = vec![TestSegment::new(1, 0, "test0.ts".to_string())];
    let batch2 = vec![TestSegment::new(1, 1, "test1.ts".to_string())];

    let source = TestSource::new_with_batches(vec![batch1, batch2]);
    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains_key(&(0, 1)));
    assert!(result.contains_key(&(1, 1)));

    Ok(())
}
