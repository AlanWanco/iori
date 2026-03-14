use iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, segment::M3u8Segment},
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::model::WatchResponse;

#[derive(Debug, Serialize, Deserialize)]
pub struct NicoTimeshiftSegmentInfo {
    sequence: u64,
    file_name: String,
}

pub struct NicoTimeshiftSource(HlsLiveSource);

impl NicoTimeshiftSource {
    pub async fn new(
        http: IoriHttp,
        wss_url: String,
        quality: &str,
        chase_play: bool,
    ) -> anyhow::Result<Self> {
        let watcher = crate::watch::WatchClient::new(http.builder(), &wss_url).await?;
        watcher.start_watching(quality, chase_play).await?;

        let stream = loop {
            let msg = watcher.recv().await?;
            if let Some(WatchResponse::Stream(stream)) = msg {
                break stream;
            }
        };

        log::info!("Playlist: {}", stream.uri);
        let url = Url::parse(&stream.uri)?;
        http.add_cookies(stream.cookies.into_cookies(), url);

        // keep seats
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = watcher.recv() => {
                        let Ok(msg) = msg else {
                            break;
                        };
                        log::debug!("message: {:?}", msg);
                    }
                    _ = watcher.keep_seat() => (),
                }
            }
            log::info!("watcher disconnected");
        });

        Ok(Self(HlsLiveSource::new(stream.uri, None)?))
    }

    /// Set the maximum number of segments to keep from the first playlist fetch.
    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.0 = self.0.with_initial_segment_limit(limit);
        self
    }
}

impl StreamingSource for NicoTimeshiftSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        self.0.segments_stream(context).await
    }
}
