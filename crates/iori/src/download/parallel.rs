use crate::WriteSegment;
use crate::context::IoriContext;
use crate::util::{Set, Unset};
use crate::{
    IoriError, SegmentInfo, StreamingSegment, StreamingSource, cache::CacheSource,
    download::DownloaderApp, error::IoriResult, merge::Merger,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::{num::NonZeroU32, sync::Arc};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore, oneshot};

#[cfg(unix)]
async fn wait_for_stop_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");

    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

struct SegmentDownloadOutcome {
    segment: SegmentInfo,
    succeeded: bool,
}

async fn download_segment<S, C>(
    segment: S,
    context: IoriContext,
    cache: Arc<C>,
    retries: u32,
) -> SegmentDownloadOutcome
where
    S: StreamingSegment + WriteSegment + Send + 'static,
    C: CacheSource + Send + Sync + 'static,
{
    let segment_info = SegmentInfo::from(&segment);
    let filename = segment_info.file_name.clone();
    let mut retries = retries;

    loop {
        if retries == 0 {
            return SegmentDownloadOutcome {
                segment: segment_info,
                succeeded: false,
            };
        }

        let writer = cache.open_writer(&segment_info).await.transpose();
        let Some(writer) = writer else {
            return SegmentDownloadOutcome {
                segment: segment_info,
                succeeded: true,
            };
        };

        let mut writer = match writer {
            Ok(writer) => writer,
            Err(e) => {
                tracing::warn!("Failed to open writer for {filename}: {e}. Retrying later.");
                retries -= 1;
                continue;
            }
        };

        // Workaround for `higher-ranked lifetime error`
        let result = segment.write_segment(&context, &mut writer).await;
        let result = match result {
            // graceful shutdown
            Ok(_) => writer.shutdown().await.map_err(IoriError::IOError),
            Err(e) => Err(e),
        };
        drop(writer);

        match result {
            Ok(_) => {
                return SegmentDownloadOutcome {
                    segment: segment_info,
                    succeeded: true,
                };
            }
            Err(e) => {
                // Invalidate the cache on failure.
                _ = cache.invalidate(&segment_info).await;
                tracing::warn!("Processing {filename} failed, retry later. {e}");
                retries -= 1;
            }
        }
    }
}

/// Spawn a task that listens for Ctrl-C signals and stops the downloader
///
/// The first Ctrl-C will trigger a graceful shutdown by calling `stop_signal.stop()`.
/// The second Ctrl-C will force exit the process.
pub fn spawn_ctrlc_handler() -> oneshot::Receiver<()> {
    let (stop_signal, receiver) = oneshot::channel();

    tokio::spawn(async move {
        // wait for the first ctrl-c to stop downloader
        wait_for_stop_signal().await;
        tracing::info!("Ctrl-C received, stopping downloader.");
        let _ = stop_signal.send(());

        // wait for the second ctrl-c to force exit
        wait_for_stop_signal().await;
        tracing::info!("Ctrl-C received again, force exit.");
        std::process::exit(1);
    });

    receiver
}

pub struct ParallelDownloader<S = (), M = (), C = (), A = ()> {
    context: IoriContext,

    source: Arc<S>,
    concurrency: NonZeroU32,
    permits: Arc<Semaphore>,

    app: Arc<A>,

    cache: Arc<C>,
    merger: Arc<Mutex<M>>,

    retries: u32,
    stop_signal: oneshot::Receiver<()>,
}

impl ParallelDownloader {
    pub fn builder(context: IoriContext) -> ParallelDownloaderBuilder {
        ParallelDownloaderBuilder::new(context)
    }
}

impl<S, M, C, A> ParallelDownloader<S, M, C, A>
where
    S: StreamingSource + Send + Sync + 'static,
    S::Segment: StreamingSegment + WriteSegment + Send + 'static,
    M: Merger + Send + Sync + 'static,
    C: CacheSource + Send + Sync + 'static,
    A: DownloaderApp + Send + Sync + 'static,
{
    pub async fn download(mut self) -> IoriResult<M::Result> {
        self.app.on_start().await?;

        let stream = self.source.segments_stream(&self.context).await?;
        tokio::pin!(stream);

        loop {
            let segments = tokio::select! {
                segments = stream.next() => segments,
                _ = &mut self.stop_signal => {
                    tracing::info!("Stop signal received, finishing downloaded segments.");
                    break;
                }
            };

            let Some(segments) = segments else {
                break;
            };

            // If the playlist is not available, the downloader will be stopped.
            if let Err(e) = segments {
                tracing::error!("Failed to fetch segment list: {e}");
                return Err(e);
            }
            let segments = segments?;

            self.app
                .on_receive_segments(&segments.iter().map(SegmentInfo::from).collect::<Vec<_>>())
                .await;

            let mut synchronized_groups: Vec<Vec<S::Segment>> = Vec::new();
            let mut group_indexes: HashMap<(u64, u64), usize> = HashMap::new();
            for segment in segments {
                if let Some(key) = segment.synchronization_key() {
                    if let Some(&index) = group_indexes.get(&key) {
                        synchronized_groups[index].push(segment);
                    } else {
                        let index = synchronized_groups.len();
                        group_indexes.insert(key, index);
                        synchronized_groups.push(vec![segment]);
                    }
                } else {
                    synchronized_groups.push(vec![segment]);
                }
            }

            for group in synchronized_groups {
                let synchronized = group.len() > 1
                    && group
                        .iter()
                        .any(|segment| segment.stream_type() == crate::StreamType::Audio)
                    && group
                        .iter()
                        .any(|segment| segment.stream_type() == crate::StreamType::Video);

                let context = self.context.clone();
                let app = self.app.clone();
                let merger = self.merger.clone();
                let cache = self.cache.clone();
                let retries = self.retries;
                let permit = self.permits.clone().acquire_owned().await.unwrap();
                tokio::spawn(async move {
                    let mut outcomes = Vec::with_capacity(group.len());
                    for segment in group {
                        outcomes.push(
                            download_segment(segment, context.clone(), cache.clone(), retries)
                                .await,
                        );
                    }
                    let group_failed = synchronized && outcomes.iter().any(|o| !o.succeeded);

                    for outcome in outcomes {
                        let filename = outcome.segment.file_name.clone();
                        if group_failed || !outcome.succeeded {
                            app.on_failed_segment(&outcome.segment).await;
                            if let Err(e) = merger
                                .lock()
                                .await
                                .fail(outcome.segment, cache.clone())
                                .await
                            {
                                tracing::error!("Failed to mark {filename} as failed: {e}");
                            }
                        } else {
                            app.on_downloaded_segment(&outcome.segment).await;
                            if let Err(e) = merger
                                .lock()
                                .await
                                .update(outcome.segment, cache.clone())
                                .await
                            {
                                tracing::error!("Failed to mark {filename} as downloaded: {e}");
                            }
                        }
                    }
                    drop(permit);
                });
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

pub struct ParallelDownloaderBuilder<
    M = (),
    C = (),
    MR = (),
    A = (),
    MergerSet = Unset,
    CacheSet = Unset,
    AppSet = Unset,
> {
    context: IoriContext,

    concurrency: NonZeroU32,
    retries: u32,
    merger: Option<M>,
    cache: Option<C>,
    stop_signal: Option<oneshot::Receiver<()>>,
    app: Option<A>,

    _merge_result: PhantomData<MR>,
    _set: (
        PhantomData<MergerSet>,
        PhantomData<CacheSet>,
        PhantomData<AppSet>,
    ),
}

impl<M, C, MR, A, MergerSet, CacheSet, AppSet>
    ParallelDownloaderBuilder<M, C, MR, A, MergerSet, CacheSet, AppSet>
{
    fn new(context: IoriContext) -> Self {
        Self {
            context,
            concurrency: NonZeroU32::new(5).unwrap(),
            retries: 3,
            merger: None,
            cache: None,
            stop_signal: None,
            app: None,
            _merge_result: Default::default(),
            _set: Default::default(),
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

    pub fn stop_signal(mut self, stop_signal: oneshot::Receiver<()>) -> Self {
        self.stop_signal = Some(stop_signal);
        self
    }

    pub fn ctrlc_handler(mut self) -> Self {
        self.stop_signal = Some(spawn_ctrlc_handler());
        self
    }

    pub fn merger<MM>(
        self,
        merger: MM,
    ) -> ParallelDownloaderBuilder<MM, C, MR, A, Set, CacheSet, AppSet>
    where
        MM: Merger<Result = MR> + Send + Sync + 'static,
    {
        ParallelDownloaderBuilder {
            context: self.context,
            concurrency: self.concurrency,
            retries: self.retries,
            merger: Some(merger),
            cache: self.cache,
            stop_signal: self.stop_signal,
            app: self.app,
            _merge_result: Default::default(),
            _set: Default::default(),
        }
    }

    pub fn cache<CC>(
        self,
        cache: CC,
    ) -> ParallelDownloaderBuilder<M, CC, MR, A, MergerSet, Set, AppSet>
    where
        CC: CacheSource,
    {
        ParallelDownloaderBuilder {
            context: self.context,
            concurrency: self.concurrency,
            retries: self.retries,
            merger: self.merger,
            cache: Some(cache),
            stop_signal: self.stop_signal,
            app: self.app,
            _merge_result: Default::default(),
            _set: Default::default(),
        }
    }

    pub fn app<AA>(
        self,
        app: AA,
    ) -> ParallelDownloaderBuilder<M, C, MR, AA, MergerSet, CacheSet, Set>
    where
        AA: DownloaderApp + Send + Sync + 'static,
    {
        ParallelDownloaderBuilder {
            context: self.context,
            concurrency: self.concurrency,
            retries: self.retries,
            merger: self.merger,
            cache: self.cache,
            stop_signal: self.stop_signal,
            app: Some(app),
            _merge_result: Default::default(),
            _set: Default::default(),
        }
    }
}

impl<M, C, MR, A, MS, CS, AS> Default for ParallelDownloaderBuilder<M, C, MR, A, MS, CS, AS> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<M, C, MR, A> ParallelDownloaderBuilder<M, C, MR, A, Set, Set, Set>
where
    M: Merger<Result = MR> + Send + Sync + 'static,
    C: CacheSource,
    A: DownloaderApp + Send + Sync + 'static,
{
    fn build<S>(self, source: S) -> ParallelDownloader<S, M, C, A>
    where
        S: StreamingSource + Send + Sync + 'static,
    {
        ParallelDownloader {
            context: self.context,
            app: Arc::new(self.app.expect("App is not set")),
            source: Arc::new(source),
            merger: Arc::new(Mutex::new(self.merger.expect("Merger is not set"))),
            cache: Arc::new(self.cache.expect("Cache is not set")),
            concurrency: self.concurrency,
            permits: Arc::new(Semaphore::new(self.concurrency.get() as usize)),
            retries: self.retries,
            stop_signal: self.stop_signal.expect("Stop signal is not set"),
        }
    }

    pub async fn download<S>(self, source: S) -> IoriResult<MR>
    where
        S: StreamingSource + Send + Sync + 'static,
    {
        let downloader = self.build(source);
        downloader.download().await
    }
}
