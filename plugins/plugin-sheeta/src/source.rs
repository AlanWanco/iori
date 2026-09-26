use anyhow::Context;
use iori_sheeta::client::SheetaClient;
use shiori_plugin::iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, segment::M3u8Segment},
    reqwest::Url,
};

/// Returns whether a page URL follows the generic Sheeta page shape.
pub fn is_sheeta_url(url: &str) -> bool {
    SheetaClient::wild_regex().is_match(url)
}

/// HLS source for Sheeta pages with session recovery after manifest failures.
pub struct SheetaSource {
    inner: HlsLiveSource,
}

impl SheetaSource {
    pub async fn new(
        http: IoriHttp,
        playlist_url: String,
        original_url: String,
        key: Option<&str>,
        use_dvr: bool,
        enable_recovery: bool,
    ) -> anyhow::Result<Self> {
        let captures = SheetaClient::wild_regex()
            .captures(&original_url)
            .with_context(|| "Invalid Sheeta page URL")?;
        let host = captures
            .name("host")
            .with_context(|| "Missing Sheeta host")?
            .as_str()
            .to_string();
        let channel = captures
            .name("channel")
            .map(|value| value.as_str().to_string());
        let video_id = captures
            .name("video_id")
            .with_context(|| "Missing Sheeta video ID")?
            .as_str()
            .to_string();

        let client = SheetaClient::common(&host, http.client()).await?;
        let fc_site_id = if let Some(channel) = &channel {
            client.get_fc_site_id(channel).await?
        } else {
            0
        };

        let mut inner = HlsLiveSource::new(playlist_url, key)?;
        if enable_recovery {
            let recovery_client = client.clone();
            let broadcast_type = use_dvr.then_some("dvr");
            inner = inner.with_manifest_recovery(move || {
                let client = recovery_client.clone();
                let video_id = video_id.clone();
                async move {
                    let session_id = client
                        .get_session_id(fc_site_id, &video_id, broadcast_type)
                        .await
                        .ok()?;
                    let video_url = client.get_video_url(&session_id).await;
                    Url::parse(&video_url).ok()
                }
            });
        }

        Ok(Self { inner })
    }

    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.inner = self.inner.with_initial_segment_limit(limit);
        self
    }

    pub fn with_manifest_retry_interval(mut self, interval: Option<std::time::Duration>) -> Self {
        self.inner = self.inner.with_manifest_retry_interval(interval);
        self
    }

    pub fn with_idle_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.inner = self.inner.with_idle_timeout(timeout);
        self
    }
}

impl StreamingSource for SheetaSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        self.inner.segments_stream(context).await
    }
}
