use anyhow::Context;
use iori_sheeta::client::SheetaClient;
use shiori_plugin::iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, segment::M3u8Segment},
    reqwest::Url,
};

/// Returns whether a page URL matches a supported Sheeta URL shape.
pub fn is_sheeta_url(url: &str) -> bool {
    SheetaClient::wild_regex().is_match(url)
}

/// HLS source that refreshes the Sheeta session when its playlist expires.
pub struct SheetaSource {
    inner: HlsLiveSource,
}

impl SheetaSource {
    pub async fn new(
        http: IoriHttp,
        playlist_url: String,
        original_url: String,
        key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let captures = SheetaClient::wild_regex()
            .captures(&original_url)
            .context("Invalid Sheeta page URL")?;
        let host = captures
            .name("host")
            .context("Missing Sheeta host")?
            .as_str();
        let video_id = captures
            .name("video_id")
            .context("Missing Sheeta video ID")?
            .as_str()
            .to_string();

        let client = SheetaClient::common(host, http.client()).await?;
        let recovery_client = client.clone();
        let inner = HlsLiveSource::new(playlist_url, key)?.with_manifest_recovery(move || {
            let client = recovery_client.clone();
            let video_id = video_id.clone();
            async move {
                let session_id = client.get_session_id(0, &video_id).await.ok()?;
                let video_url = client.get_video_url(&session_id).await;
                if !matches!(client.probe_video_url(&video_url).await, Ok(true)) {
                    return None;
                }
                Url::parse(&video_url).ok()
            }
        });

        Ok(Self { inner })
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
