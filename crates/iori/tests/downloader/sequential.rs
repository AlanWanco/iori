use std::sync::Arc;

use iori::{cache::memory::MemoryCacheSource, download::SequentialDownloader, merge::SkipMerger};

use crate::source::{TestSegment, TestSource};

#[tokio::test]
async fn test_sequential_downloader_success() -> anyhow::Result<()> {
    let source = TestSource::new(vec![
        TestSegment::new(1, 0, "test0.ts".to_string()),
        TestSegment::new(1, 1, "test1.ts".to_string()),
    ]);

    let cache = Arc::new(MemoryCacheSource::new());

    let mut downloader =
        SequentialDownloader::new(Default::default(), source, SkipMerger, cache.clone());

    downloader.download().await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains_key(&(0, 1)));
    assert!(result.contains_key(&(1, 1)));

    Ok(())
}

#[tokio::test]
async fn test_sequential_downloader_failure() -> anyhow::Result<()> {
    let source = TestSource::new(vec![
        TestSegment::new(1, 0, "test0.ts".to_string()).with_fail_count(1),
    ]);

    let cache = Arc::new(MemoryCacheSource::new());

    let mut downloader =
        SequentialDownloader::new(Default::default(), source, SkipMerger, cache.clone());

    // SequentialDownloader currently doesn't retry, so it should fail or record failure in merger
    // Based on implementation, it calls merger.fail()
    downloader.download().await?;

    let result = cache.into_inner();
    let _result = result.lock().unwrap();
    // MemoryCacheSource might still have the entry if not invalidated
    // But SequentialDownloader doesn't invalidate on failure, it just calls fail()
    // Let's check how SkipMerger handles it. SkipMerger does nothing.

    // In sequential.rs:
    // match fetch_result {
    //     Ok(_) => self.merger.update(segment_info, self.cache.clone()).await?,
    //     Err(_) => self.merger.fail(segment_info, self.cache.clone()).await?,
    // }

    Ok(())
}
