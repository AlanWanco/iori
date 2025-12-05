use crate::{
    IoriError, SegmentInfo, StreamingSegment, StreamingSource, cache::CacheSource,
    download::DownloaderApp, error::IoriResult, merge::Merger,
};
use futures::StreamExt;
use std::{future::Future, num::NonZeroU32, sync::Arc};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore, oneshot};

/// Spawn a task that listens for Ctrl-C signals and stops the downloader
///
/// The first Ctrl-C will trigger a graceful shutdown by calling `stop_signal.stop()`.
/// The second Ctrl-C will force exit the process.
pub fn spawn_ctrlc_handler() -> oneshot::Receiver<()> {
    let (stop_signal, receiver) = oneshot::channel();

    tokio::spawn(async move {
        // wait for the first ctrl-c to stop downloader
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("Ctrl-C received, stopping downloader.");
            stop_signal.send(()).expect("Failed to send stop signal");
        }

        // wait for the second ctrl-c to force exit
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("Ctrl-C received again, force exit.");
            std::process::exit(1);
        }
    });

    receiver
}

pub struct ParallelDownloader<S, M, C, A>
where
    M: Merger,
    C: CacheSource,
    A: DownloaderApp,
{
    source: Arc<S>,
    concurrency: NonZeroU32,
    permits: Arc<Semaphore>,

    app: Arc<A>,

    cache: Arc<C>,
    merger: Arc<Mutex<M>>,

    retries: u32,
    stop_signal: oneshot::Receiver<()>,
}

impl<M, C> ParallelDownloader<(), M, C, ()>
where
    M: Merger + Send + Sync + 'static,
    C: CacheSource + Send + Sync + 'static,
{
    pub fn builder() -> ParallelDownloaderBuilder<M, C, M::Result, ()> {
        ParallelDownloaderBuilder::new()
    }
}

impl<S, M, C, A> ParallelDownloader<S, M, C, A>
where
    S: StreamingSource + Send + Sync + 'static,
    M: Merger + Send + Sync + 'static,
    C: CacheSource + Send + Sync + 'static,
    A: DownloaderApp + Send + Sync + 'static,
{
    pub(crate) fn new(
        app: A,
        source: S,
        merger: M,
        cache: C,
        concurrency: NonZeroU32,
        retries: u32,
        stop_signal: oneshot::Receiver<()>,
    ) -> Self {
        let permits = Arc::new(Semaphore::new(concurrency.get() as usize));

        Self {
            app: Arc::new(app),
            source: Arc::new(source),
            merger: Arc::new(Mutex::new(merger)),
            cache: Arc::new(cache),
            concurrency,
            permits,

            retries,
            stop_signal,
        }
    }

    pub async fn download(self) -> IoriResult<M::Result> {
        self.app.on_start().await?;

        let stream = self.source.segments_stream().await?;
        tokio::pin!(stream);

        while let Some(segments) = stream.next().await {
            // If the playlist is not available, the downloader will be stopped.
            if let Err(e) = segments {
                tracing::error!("Failed to fetch segment list: {e}");
                return Err(e);
            }
            let segments = segments?;

            self.app
                .on_receive_segments(&segments.iter().map(SegmentInfo::from).collect::<Vec<_>>())
                .await;

            for segment in segments {
                let segment_info = SegmentInfo::from(&segment);

                let permit = self.permits.clone().acquire_owned().await.unwrap();

                let app = self.app.clone();
                let source = self.source.clone();
                let merger = self.merger.clone();
                let cache = self.cache.clone();

                let mut retries = self.retries;
                tokio::spawn(async move {
                    let filename = segment.file_name();

                    loop {
                        if retries == 0 {
                            app.on_failed_segment(&segment_info).await;
                            if let Err(e) = merger.lock().await.fail(segment_info, cache).await {
                                tracing::error!("Failed to mark {filename} as failed: {e}");
                            }
                            return;
                        }

                        let writer = cache.open_writer(&segment_info).await.transpose();
                        let Some(writer) = writer else {
                            app.on_downloaded_segment(&segment_info).await;
                            if let Err(e) = merger.lock().await.update(segment_info, cache).await {
                                tracing::error!("Failed to mark {filename} as downloaded: {e}");
                            }
                            return;
                        };

                        let mut writer = match writer {
                            Ok(writer) => writer,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to open writer for {filename}: {e}. Retrying later."
                                );
                                retries -= 1;
                                continue;
                            }
                        };

                        // Workaround for `higher-ranked lifetime error`
                        let result = assert_send(source.fetch_segment(&segment, &mut writer)).await;
                        let result = match result {
                            // graceful shutdown
                            Ok(_) => writer.shutdown().await.map_err(IoriError::IOError),
                            Err(e) => Err(e),
                        };
                        drop(writer);
                        match result {
                            Ok(_) => break,
                            Err(e) => {
                                // invalidate the cache on failure
                                _ = cache.invalidate(&segment_info).await;

                                tracing::warn!("Processing {filename} failed, retry later. {e}");
                                retries -= 1;
                            }
                        }
                    }

                    // here we can not drop semaphore, because the merger might take some time to process the merging

                    app.on_downloaded_segment(&segment_info).await;

                    _ = merger.lock().await.update(segment_info, cache).await;

                    // drop permit to release the semaphore
                    drop(permit);
                });
            }

            if self.stop_signal.is_terminated() {
                break;
            }
        }

        // wait for all tasks to finish
        let _ = self
            .permits
            .acquire_many(self.concurrency.get())
            .await
            .unwrap();

        self.app.on_finished().await?;

        self.merger.lock().await.finish(self.cache).await
    }
}

// https://github.com/rust-lang/rust/issues/102211#issuecomment-1371414544
// TODO: remove this when this issue is fixed
fn assert_send<'a, T>(
    fut: impl Future<Output = T> + Send + 'a,
) -> impl Future<Output = T> + Send + 'a {
    fut
}

pub struct ParallelDownloaderBuilder<M, C, MR, A> {
    concurrency: NonZeroU32,
    retries: u32,
    merger: Option<M>,
    cache: Option<C>,
    stop_signal: Option<oneshot::Receiver<()>>,
    app: Option<A>,

    _merge_result: std::marker::PhantomData<MR>,
}

impl<M, C, MR, A> ParallelDownloaderBuilder<M, C, MR, A>
where
    M: Merger<Result = MR> + Send + Sync + 'static,
    C: CacheSource,
    A: DownloaderApp + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            concurrency: NonZeroU32::new(5).unwrap(),
            retries: 3,
            merger: None,
            cache: None,
            stop_signal: None,
            app: None,
            _merge_result: Default::default(),
        }
    }

    pub fn concurrency(mut self, concurrency: NonZeroU32) -> Self {
        self.concurrency = concurrency;
        self
    }

    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    pub fn merger(mut self, merger: M) -> Self {
        self.merger = Some(merger);
        self
    }

    pub fn cache(mut self, cache: C) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn stop_signal(mut self, stop_signal: oneshot::Receiver<()>) -> Self {
        self.stop_signal = Some(stop_signal);
        self
    }

    pub fn ctrlc_handler(mut self) -> Self {
        self.stop_signal = Some(spawn_ctrlc_handler());
        self
    }

    pub fn app<AA>(self, app: AA) -> ParallelDownloaderBuilder<M, C, MR, AA>
    where
        AA: DownloaderApp + Send + Sync + 'static,
    {
        ParallelDownloaderBuilder::<M, C, MR, AA> {
            app: Some(app),
            concurrency: self.concurrency,
            retries: self.retries,
            merger: self.merger,
            cache: self.cache,
            stop_signal: self.stop_signal,
            _merge_result: std::marker::PhantomData,
        }
    }

    fn build<S>(self, source: S) -> ParallelDownloader<S, M, C, A>
    where
        S: StreamingSource + Send + Sync + 'static,
    {
        ParallelDownloader::new(
            self.app.expect("App is not set"),
            source,
            self.merger.expect("Merger is not set"),
            self.cache.expect("Cache is not set"),
            self.concurrency,
            self.retries,
            self.stop_signal.expect("Stop signal is not set"),
        )
    }

    pub async fn download<S>(self, source: S) -> IoriResult<MR>
    where
        S: StreamingSource + Send + Sync + 'static,
    {
        let downloader = self.build(source);
        downloader.download().await
    }
}

impl<M, C, MR, A> Default for ParallelDownloaderBuilder<M, C, MR, A>
where
    M: Merger<Result = MR> + Send + Sync + 'static,
    C: CacheSource,
    A: DownloaderApp + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
