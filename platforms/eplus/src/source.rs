use std::time::Duration;

use iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, segment::M3u8Segment},
};

/// Refresh interval for CloudFront cookies (45 minutes).
const COOKIE_REFRESH_INTERVAL: Duration = Duration::from_secs(45 * 60);

/// An HLS streaming source for eplus.jp that periodically refreshes CloudFront cookies.
///
/// Wraps [`HlsLiveSource`] and spawns a background task that re-fetches the event page
/// every 45 minutes. Since the download-phase [`IoriHttp`] uses a shared cookie store
/// (`Arc<CookieStoreMutex>`) as reqwest's `cookie_provider`, and the `Client` in
/// [`IoriContext`] was built from that same `IoriHttp`, the `Set-Cookie` headers
/// from the refresh response automatically update the jar. Subsequent segment
/// fetches by the inner [`HlsLiveSource`] pick up the new CloudFront cookies.
pub struct EplusSource {
    inner: HlsLiveSource,
    /// A clone of the download-phase IoriHttp. Shares the same `Arc<CookieStoreMutex>`
    /// as the `Client` inside the `IoriContext` passed to `segments_stream`.
    http: IoriHttp,
    event_url: String,
}

impl EplusSource {
    /// Create a new `EplusSource`.
    ///
    /// # Arguments
    /// * `http` — The download-phase [`IoriHttp`] that already has session + CloudFront cookies.
    ///   Its shared cookie store is the same one used by `IoriContext.client`.
    /// * `playlist_url` — The m3u8 playlist URL.
    /// * `event_url` — The eplus event page URL, used to refresh CloudFront cookies.
    /// * `key` — Optional decryption key.
    pub fn new(
        http: IoriHttp,
        playlist_url: String,
        event_url: String,
        key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let inner = HlsLiveSource::new(playlist_url, key)?;
        Ok(Self {
            inner,
            http,
            event_url,
        })
    }

    /// Set the maximum number of segments to keep from the first playlist fetch.
    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.inner = self.inner.with_initial_segment_limit(limit);
        self
    }
}

impl StreamingSource for EplusSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        // Spawn a background task that refreshes CloudFront cookies every 45 minutes.
        //
        // The `refresh_http` clone shares the same `Arc<CookieStoreMutex>` as the
        // `context.client`. When the refresh GET to the event page returns Set-Cookie
        // headers, reqwest automatically stores them in the shared cookie jar.
        // The inner HlsLiveSource's segment fetches (using `context.client`) will then
        // send the updated cookies.
        let refresh_http = self.http.clone();
        let event_url = self.event_url.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(COOKIE_REFRESH_INTERVAL).await;
                log::info!("[eplus] Refreshing CloudFront cookies...");

                // Use client() from the cloned IoriHttp. If the OnceLock was already
                // initialized, it returns the same Client (which shares the cookie store).
                // If not, it builds a new Client with the shared cookie_provider.
                let client = refresh_http.client();
                match client.get(&event_url).send().await {
                    Ok(res) => {
                        if res.status().is_success() {
                            log::info!("[eplus] CloudFront cookies refreshed successfully.");
                        } else {
                            log::warn!(
                                "[eplus] Cookie refresh request returned status {}",
                                res.status()
                            );
                        }
                        // Consume the body to complete the request.
                        let _ = res.text().await;
                    }
                    Err(e) => {
                        log::error!("[eplus] Failed to refresh cookies: {e}");
                    }
                }
            }
        });

        // Delegate to the inner HlsLiveSource, passing through the context unchanged.
        // The context.client already uses our shared cookie store.
        self.inner.segments_stream(context).await
    }
}
