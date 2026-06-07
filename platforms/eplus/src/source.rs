use std::time::Duration;

use iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, iori_hls, segment::M3u8Segment},
};

use crate::EplusClient;

/// Refresh interval for CloudFront cookies (45 minutes).
const COOKIE_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const CLOUDFRONT_COOKIE_NAMES: &[&str] = &[
    "CloudFront-Policy",
    "CloudFront-Signature",
    "CloudFront-Key-Pair-Id",
];

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
    playlist_url: String,
    event_url: String,
    credentials: Option<EplusCredentials>,
    refresh_interval: Duration,
}

#[derive(Clone)]
pub struct EplusCredentials {
    pub username: String,
    pub password: String,
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
        credentials: Option<EplusCredentials>,
    ) -> anyhow::Result<Self> {
        let inner = HlsLiveSource::new(playlist_url.clone(), key)?;
        Ok(Self {
            inner,
            http,
            playlist_url,
            event_url,
            credentials,
            refresh_interval: COOKIE_REFRESH_INTERVAL,
        })
    }

    /// Set the maximum number of segments to keep from the first playlist fetch.
    pub fn with_initial_segment_limit(mut self, limit: Option<usize>) -> Self {
        self.inner = self.inner.with_initial_segment_limit(limit);
        self
    }

    /// Stop polling when no new segments arrive within `timeout`.
    pub fn with_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.inner = self.inner.with_idle_timeout(timeout);
        self
    }

    pub fn with_refresh_interval(mut self, interval: Option<Duration>) -> Self {
        if let Some(interval) = interval {
            self.refresh_interval = interval;
        }
        self
    }

    fn replace_session_cookies(http: &IoriHttp, url: &str, cookies: &[String]) {
        if cookies.is_empty() {
            return;
        }

        let previous_cookie_count = http.export_cookies_for_url(url).len();
        http.clear_all_cookies();
        http.add_cookies(cookies.to_vec(), url);
        let current_cookie_count = http.export_cookies_for_url(url).len();
        log::info!(
            "[eplus] Replaced session cookies: {} -> {} visible for {}.",
            previous_cookie_count,
            current_cookie_count,
            url
        );
    }

    async fn probe_playlist(http: &IoriHttp, playlist_url: &str) -> bool {
        let probe_client = http.client();
        match probe_client.get(playlist_url).send().await {
            Ok(playlist_res) => match playlist_res.bytes().await {
                Ok(body) => {
                    if iori_hls::parse_playlist_res(&body).is_ok() {
                        log::info!("[eplus] Refreshed playlist probe succeeded.");
                        true
                    } else {
                        log::warn!("[eplus] Refreshed playlist probe returned non-m3u8 content.");
                        false
                    }
                }
                Err(error) => {
                    log::warn!("[eplus] Failed to read refreshed playlist probe body: {error}");
                    false
                }
            },
            Err(error) => {
                log::warn!("[eplus] Refreshed playlist probe request failed: {error}");
                false
            }
        }
    }
}

impl StreamingSource for EplusSource {
    type Segment = M3u8Segment;

    async fn segments_stream(
        &self,
        context: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        let refresh_http = self.http.clone();
        let playlist_url = self.playlist_url.clone();
        let event_url = self.event_url.clone();
        let credentials = self.credentials.clone();
        let refresh_interval = self.refresh_interval;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(refresh_interval).await;
                log::info!("[eplus] Refreshing CloudFront cookies...");

                let client = match EplusClient::new(refresh_http.builder()) {
                    Ok(client) => client,
                    Err(error) => {
                        log::error!("[eplus] Failed to create refresh client: {error:#}");
                        continue;
                    }
                };
                let status_client = match EplusClient::new(refresh_http.raw_builder()) {
                    Ok(client) => client,
                    Err(error) => {
                        log::error!("[eplus] Failed to create stateless status client: {error:#}");
                        continue;
                    }
                };

                let refresh_cycle = async {
                    let event_data = client.get_event_data(&event_url).await?;
                    let cookies_before = refresh_http.export_cookies_for_url(&playlist_url);
                    let previous_cookie_snapshot = refresh_http.snapshot_cookies();

                    let mut status_cookie_count = 0usize;
                    let mut status_probe_succeeded = false;
                    if let Some(session_update_url) = event_data.session_update_url.as_deref() {
                        log::info!(
                            "[eplus] Refreshing CloudFront cookies via status API: {session_update_url}"
                        );
                        match status_client.refresh_status_cookies(session_update_url).await {
                            Ok(status_result) => {
                                status_cookie_count = status_result.cloudfront_cookie_count;
                                log::info!(
                                    "[eplus] Stateless status API returned {} CloudFront cookies.",
                                    status_cookie_count
                                );
                                if !status_result.set_cookies.is_empty() {
                                    Self::replace_session_cookies(
                                        &refresh_http,
                                        session_update_url,
                                        &status_result.set_cookies,
                                    );
                                    status_probe_succeeded =
                                        Self::probe_playlist(&refresh_http, &playlist_url).await;
                                }
                            }
                            Err(error) => {
                                log::warn!(
                                    "[eplus] Stateless status API cookie refresh failed: {error:#}"
                                );
                            }
                        }
                    } else {
                        log::warn!(
                            "[eplus] No streamSession/session_update_url found; falling back to event-page cookies."
                        );
                    }

                    if !status_probe_succeeded {
                        log::info!("[eplus] Restoring previous session cookies before fallback.");
                        refresh_http.restore_cookies(previous_cookie_snapshot.clone());

                        log::info!(
                            "[eplus] Falling back to event-page CloudFront cookies for this refresh cycle."
                        );
                        let removed = refresh_http.clear_cookies_by_names(CLOUDFRONT_COOKIE_NAMES);
                        log::info!(
                            "[eplus] Replacing CloudFront cookies from event page fallback (removed {}).",
                            removed
                        );
                        refresh_http.add_cookies(event_data.cloudfront_cookies.clone(), &event_url);
                        let _ = Self::probe_playlist(&refresh_http, &playlist_url).await;
                    }

                    let cookies_after = refresh_http.export_cookies_for_url(&playlist_url);
                    log::info!(
                        "[eplus] CloudFront refresh finished. playlist cookies: {} -> {}, status api cookies: {}",
                        cookies_before.len(),
                        cookies_after.len(),
                        status_cookie_count
                    );
                    anyhow::Ok(())
                };

                if let Err(error) = refresh_cycle.await {
                    log::warn!("[eplus] Cookie refresh with current session failed: {error:#}");

                    let Some(credentials) = &credentials else {
                        log::error!(
                            "[eplus] Failed to refresh cookies and no credentials are available for re-login."
                        );
                        continue;
                    };

                    log::info!("[eplus] Attempting eplus re-login before retrying refresh...");
                    match EplusClient::login(
                        refresh_http.builder(),
                        &event_url,
                        &credentials.username,
                        &credentials.password,
                    )
                    .await
                    {
                        Ok(relogged_client) => {
                            log::info!("[eplus] Re-login succeeded; retrying status refresh.");
                            match relogged_client.get_event_data(&event_url).await {
                                Ok(event_data) => {
                                    let previous_cookie_snapshot = refresh_http.snapshot_cookies();
                                    let mut status_probe_succeeded = false;

                                    if let Some(session_update_url) = event_data.session_update_url.as_deref() {
                                        let status_client = match EplusClient::new(refresh_http.raw_builder()) {
                                            Ok(client) => client,
                                            Err(error) => {
                                                log::error!(
                                                    "[eplus] Failed to create stateless status client after re-login: {error:#}"
                                                );
                                                continue;
                                            }
                                        };
                                        log::info!(
                                            "[eplus] Refreshing CloudFront cookies via status API after re-login: {session_update_url}"
                                        );
                                        match status_client.refresh_status_cookies(session_update_url).await {
                                            Ok(status_result) => {
                                                log::info!(
                                                    "[eplus] Stateless status API after re-login returned {} CloudFront cookies.",
                                                    status_result.cloudfront_cookie_count
                                                );
                                                if !status_result.set_cookies.is_empty() {
                                                    Self::replace_session_cookies(
                                                        &refresh_http,
                                                        session_update_url,
                                                        &status_result.set_cookies,
                                                    );
                                                    status_probe_succeeded = Self::probe_playlist(
                                                        &refresh_http,
                                                        &playlist_url,
                                                    )
                                                    .await;
                                                }
                                            }
                                            Err(error) => {
                                                log::warn!(
                                                    "[eplus] Stateless status API after re-login failed: {error:#}"
                                                );
                                            }
                                        }
                                    } else {
                                        log::warn!(
                                            "[eplus] Re-login succeeded but streamSession/session_update_url is still missing."
                                        );
                                    }

                                    if !status_probe_succeeded {
                                        log::info!("[eplus] Restoring previous session cookies before fallback after re-login.");
                                        refresh_http.restore_cookies(previous_cookie_snapshot.clone());

                                        log::info!(
                                            "[eplus] Falling back to event-page CloudFront cookies after re-login."
                                        );
                                        let removed = refresh_http.clear_cookies_by_names(CLOUDFRONT_COOKIE_NAMES);
                                        log::info!(
                                            "[eplus] Replacing CloudFront cookies from event page fallback after re-login (removed {}).",
                                            removed
                                        );
                                        refresh_http.add_cookies(event_data.cloudfront_cookies.clone(), &event_url);
                                        let _ = Self::probe_playlist(&refresh_http, &playlist_url).await;
                                    }
                                }
                                Err(error) => {
                                    log::error!(
                                        "[eplus] Re-login succeeded but event data refresh still failed: {error:#}"
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            log::error!("[eplus] Re-login failed during cookie refresh: {error:#}");
                        }
                    }
                }
            }
        });

        self.inner.segments_stream(context).await
    }
}
