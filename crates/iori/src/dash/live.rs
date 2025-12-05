mod clock;
mod selector;
mod timeline;

use super::segment::DashSegment;
use crate::{IoriResult, StreamingSource, context::IoriContext, decrypt::IoriKey};
use futures::{Stream, stream};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use timeline::MPDTimeline;
use tokio::sync::{Mutex, mpsc};
use url::Url;

pub struct CommonDashLiveSource {
    mpd_url: Url,
    key: Option<Arc<IoriKey>>,
    timeline: Arc<Mutex<Option<MPDTimeline>>>,
}

impl CommonDashLiveSource {
    pub fn new(mpd_url: Url, key: Option<&str>) -> IoriResult<Self> {
        let key = key.map(IoriKey::clear_key).transpose()?.map(Arc::new);

        Ok(Self {
            mpd_url,
            key,
            timeline: Arc::new(Mutex::new(None)),
        })
    }
}

impl StreamingSource for CommonDashLiveSource {
    type Segment = DashSegment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        let (sender, receiver) = mpsc::unbounded_channel();

        let mpd = context
            .client
            .get(self.mpd_url.as_ref())
            .send()
            .await?
            .text()
            .await?;
        let mpd = dash_mpd::parse(&mpd)?;

        let sequence_number = Arc::new(AtomicU64::new(0));

        let minimum_update_period = mpd.minimumUpdatePeriod.unwrap_or(Duration::from_secs(2));
        let timeline = MPDTimeline::from_mpd(&context.client, mpd, Some(&self.mpd_url)).await?;

        let (mut segments, mut last_update) = timeline
            .segments_since(&context.client, None, self.key.clone())
            .await?;
        for segment in segments.iter_mut() {
            segment.sequence = sequence_number.fetch_add(1, Ordering::Relaxed);
        }
        sender.send(Ok(segments)).unwrap();

        if timeline.is_dynamic() {
            self.timeline.lock().await.replace(timeline);

            let mpd_url = self.mpd_url.clone();
            let client = context.client.clone();
            let timeline = self.timeline.clone();
            let key = self.key.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(minimum_update_period).await;

                    let mpd = client
                        .get(mpd_url.as_ref())
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap();
                    let mpd = dash_mpd::parse(&mpd).unwrap();

                    let mut timeline = timeline.lock().await;
                    let timeline = timeline.as_mut().unwrap();
                    timeline.update_mpd(&client, mpd, &mpd_url).await.unwrap();

                    let (segments, _last_update) = timeline
                        .segments_since(&client, last_update, key.clone())
                        .await
                        .unwrap();
                    sender.send(Ok(segments)).unwrap();

                    if let Some(_last_update) = _last_update {
                        last_update = Some(_last_update);
                    }

                    if timeline.is_static() {
                        break;
                    }
                }
            });
        }

        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}
