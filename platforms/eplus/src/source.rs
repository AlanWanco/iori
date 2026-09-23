use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use iori::{
    IoriHttp, IoriResult, Stream, StreamingSource,
    context::IoriContext,
    hls::{HlsLiveSource, iori_hls, segment::M3u8Segment},
};
use reqwest::Url;
use tokio::sync::oneshot;

use crate::{
    EplusClient,
    model::{DeliveryStatus, EplusEventData},
};

/// Refresh interval for CloudFront cookies (30 minutes).
const COOKIE_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const CLOUDFRONT_COOKIE_NAMES: &[&str] = &[
    "CloudFront-Policy",
    "CloudFront-Signature",
    "CloudFront-Key-Pair-Id",
];

struct RefreshGuard<S> {
    inner: S,
    cancel: Option<oneshot::Sender<()>>,
}

impl<S: Stream + Unpin> Stream for RefreshGuard<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        match Pin::new(&mut this.inner).poll_next(context) {
            Poll::Ready(None) => {
                if let Some(cancel) = this.cancel.take() {
                    let _ = cancel.send(());
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl<S> Drop for RefreshGuard<S> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

/// An HLS streaming source for eplus.jp that periodically refreshes CloudFront cookies.
///
/// Wraps [`HlsLiveSource`] and spawns a background task that re-fetches the event page
/// every 30 minutes. Since the download-phase [`IoriHttp`] uses a shared cookie store
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
        let includes_cloudfront_cookie = cookies.iter().any(|cookie| {
            cookie
                .split_once('=')
                .map(|(name, _)| CLOUDFRONT_COOKIE_NAMES.contains(&name.trim()))
                .unwrap_or(false)
        });
        if includes_cloudfront_cookie {
            http.clear_cookies_by_names(CLOUDFRONT_COOKIE_NAMES);
        }
        http.add_cookies(cookies.to_vec(), url);
        let current_cookie_count = http.export_cookies_for_url(url).len();
        log::info!(
            "[eplus] Updated session cookies: {} -> {} visible for the target host.",
            previous_cookie_count,
            current_cookie_count
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
                Err(_) => {
                    log::warn!("[eplus] Failed to read refreshed playlist probe body.");
                    false
                }
            },
            Err(_) => {
                log::warn!("[eplus] Refreshed playlist probe request failed.");
                false
            }
        }
    }

    fn refreshed_playlist_url(current_url: &str, event_data: &EplusEventData) -> Option<String> {
        let current_host = Url::parse(current_url).ok()?.host_str()?.to_string();
        let target_host = match &event_data.delivery_status {
            DeliveryStatus::Started => "stream.live.eplus.jp",
            DeliveryStatus::ConfirmedArchive => "vod.live.eplus.jp",
            _ => current_host.as_str(),
        };
        let prefer_archive = target_host == "vod.live.eplus.jp";

        EplusClient::candidate_playlists(&event_data.m3u8_urls, prefer_archive)
            .into_iter()
            .find(|candidate| {
                Url::parse(candidate)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .as_deref()
                    == Some(target_host)
            })
    }

    async fn probe_playlist_candidates(
        http: &IoriHttp,
        current_url: &str,
        refreshed_url: &str,
    ) -> Option<String> {
        if refreshed_url != current_url && Self::probe_playlist(http, refreshed_url).await {
            return Some(refreshed_url.to_string());
        }

        Self::probe_playlist(http, current_url)
            .await
            .then(|| current_url.to_string())
    }

    async fn activate_playlist_url(
        source: &HlsLiveSource,
        context: &IoriContext,
        current_url: &mut String,
        refreshed_url: Option<String>,
    ) {
        let Some(refreshed_url) = refreshed_url else {
            return;
        };
        if *current_url == refreshed_url {
            return;
        }

        match source.update_playlist_url(context, &refreshed_url).await {
            Ok(true) => {
                *current_url = refreshed_url;
                log::info!("[eplus] Active playlist URL was updated after refresh.");
            }
            Ok(false) => {
                log::warn!(
                    "[eplus] Refreshed playlist URL changed, but the active HLS source could not switch to it."
                );
            }
            Err(_) => {
                log::warn!("[eplus] Failed to update the active playlist URL.");
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
        let inner_stream = self.inner.segments_stream(context).await?;
        let (cancel_refresh, mut cancel_refresh_rx) = oneshot::channel();
        let refresh_http = self.http.clone();
        let mut playlist_url = self.playlist_url.clone();
        let refresh_inner = self.inner.clone();
        let refresh_context = context.clone();
        let event_url = self.event_url.clone();
        let credentials = self.credentials.clone();
        let refresh_interval = self.refresh_interval;
        tokio::spawn(async move {
            loop {
                if tokio::time::timeout(refresh_interval, &mut cancel_refresh_rx)
                    .await
                    .is_ok()
                {
                    break;
                }
                log::info!("[eplus] Refreshing CloudFront cookies...");

                let client = match EplusClient::new(refresh_http.builder()) {
                    Ok(client) => client,
                    Err(_) => {
                        log::error!("[eplus] Failed to create refresh client.");
                        continue;
                    }
                };
                let status_client = match EplusClient::new(refresh_http.raw_builder()) {
                    Ok(client) => client,
                    Err(_) => {
                        log::error!("[eplus] Failed to create stateless status client.");
                        continue;
                    }
                };

                let refresh_cycle = async {
                    let cookies_before = refresh_http.export_cookies_for_url(&playlist_url);
                    let previous_cookie_snapshot = refresh_http.snapshot_cookies();
                    let event_data = client.get_event_data(&event_url).await?;
                    let refreshed_url = Self::refreshed_playlist_url(&playlist_url, &event_data)
                        .unwrap_or_else(|| playlist_url.clone());

                    let mut status_cookie_count = 0usize;
                    let mut successful_playlist_url = None;
                    if let Some(session_update_url) = event_data.session_update_url.as_deref() {
                        log::info!("[eplus] Refreshing CloudFront cookies via the status API.");
                        match status_client
                            .refresh_status_cookies(session_update_url)
                            .await
                        {
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
                                    successful_playlist_url = Self::probe_playlist_candidates(
                                        &refresh_http,
                                        &playlist_url,
                                        &refreshed_url,
                                    )
                                    .await;
                                }
                            }
                            Err(_) => {
                                log::warn!("[eplus] Stateless status API cookie refresh failed.");
                            }
                        }
                    } else {
                        log::warn!(
                            "[eplus] No streamSession/session_update_url found; falling back to event-page cookies."
                        );
                    }

                    if successful_playlist_url.is_none() {
                        log::info!("[eplus] Restoring previous session cookies before fallback.");
                        refresh_http.restore_cookies(previous_cookie_snapshot.clone());
                        successful_playlist_url = Self::probe_playlist_candidates(
                            &refresh_http,
                            &playlist_url,
                            &refreshed_url,
                        )
                        .await;

                        if successful_playlist_url.is_none() {
                            log::info!(
                                "[eplus] Falling back to event-page CloudFront cookies for this refresh cycle."
                            );
                            if event_data.cloudfront_cookies.is_empty() {
                                log::warn!(
                                    "[eplus] Event-page refresh returned no CloudFront cookies; retaining previous cookies."
                                );
                            } else {
                                let removed =
                                    refresh_http.clear_cookies_by_names(CLOUDFRONT_COOKIE_NAMES);
                                log::info!(
                                    "[eplus] Replacing CloudFront cookies from event page fallback (removed {}).",
                                    removed
                                );
                                refresh_http
                                    .add_cookies(event_data.cloudfront_cookies.clone(), &event_url);
                            }
                            successful_playlist_url = Self::probe_playlist_candidates(
                                &refresh_http,
                                &playlist_url,
                                &refreshed_url,
                            )
                            .await;
                        }
                    }

                    if successful_playlist_url.is_none() {
                        return Err(anyhow::anyhow!(
                            "[eplus] CloudFront refresh could not validate either playlist URL."
                        ));
                    }

                    Self::activate_playlist_url(
                        &refresh_inner,
                        &refresh_context,
                        &mut playlist_url,
                        successful_playlist_url,
                    )
                    .await;

                    let cookies_after = refresh_http.export_cookies_for_url(&playlist_url);
                    log::info!(
                        "[eplus] CloudFront refresh finished. playlist cookies: {} -> {}, status api cookies: {}",
                        cookies_before.len(),
                        cookies_after.len(),
                        status_cookie_count
                    );
                    anyhow::Ok(())
                };

                if refresh_cycle.await.is_err() {
                    log::warn!("[eplus] Cookie refresh with the current session failed.");

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
                            let previous_cookie_snapshot = refresh_http.snapshot_cookies();
                            match relogged_client.get_event_data(&event_url).await {
                                Ok(event_data) => {
                                    let refreshed_url =
                                        Self::refreshed_playlist_url(&playlist_url, &event_data)
                                            .unwrap_or_else(|| playlist_url.clone());
                                    let mut successful_playlist_url = None;

                                    if let Some(session_update_url) =
                                        event_data.session_update_url.as_deref()
                                    {
                                        let status_client = match EplusClient::new(
                                            refresh_http.raw_builder(),
                                        ) {
                                            Ok(client) => client,
                                            Err(_) => {
                                                log::error!(
                                                    "[eplus] Failed to create stateless status client after re-login."
                                                );
                                                continue;
                                            }
                                        };
                                        log::info!(
                                            "[eplus] Refreshing CloudFront cookies via the status API after re-login."
                                        );
                                        match status_client
                                            .refresh_status_cookies(session_update_url)
                                            .await
                                        {
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
                                                    successful_playlist_url =
                                                        Self::probe_playlist_candidates(
                                                            &refresh_http,
                                                            &playlist_url,
                                                            &refreshed_url,
                                                        )
                                                        .await;
                                                }
                                            }
                                            Err(_) => {
                                                log::warn!(
                                                    "[eplus] Stateless status API after re-login failed."
                                                );
                                            }
                                        }
                                    } else {
                                        log::warn!(
                                            "[eplus] Re-login succeeded but streamSession/session_update_url is still missing."
                                        );
                                    }

                                    if successful_playlist_url.is_none() {
                                        log::info!(
                                            "[eplus] Restoring previous session cookies before fallback after re-login."
                                        );
                                        refresh_http
                                            .restore_cookies(previous_cookie_snapshot.clone());
                                        successful_playlist_url = Self::probe_playlist_candidates(
                                            &refresh_http,
                                            &playlist_url,
                                            &refreshed_url,
                                        )
                                        .await;

                                        if successful_playlist_url.is_none() {
                                            log::info!(
                                                "[eplus] Falling back to event-page CloudFront cookies after re-login."
                                            );
                                            if event_data.cloudfront_cookies.is_empty() {
                                                log::warn!(
                                                    "[eplus] Event-page refresh after re-login returned no CloudFront cookies; retaining previous cookies."
                                                );
                                            } else {
                                                let removed = refresh_http.clear_cookies_by_names(
                                                    CLOUDFRONT_COOKIE_NAMES,
                                                );
                                                log::info!(
                                                    "[eplus] Replacing CloudFront cookies from event page fallback after re-login (removed {}).",
                                                    removed
                                                );
                                                refresh_http.add_cookies(
                                                    event_data.cloudfront_cookies.clone(),
                                                    &event_url,
                                                );
                                            }
                                            successful_playlist_url =
                                                Self::probe_playlist_candidates(
                                                    &refresh_http,
                                                    &playlist_url,
                                                    &refreshed_url,
                                                )
                                                .await;
                                        }
                                    }

                                    if successful_playlist_url.is_some() {
                                        Self::activate_playlist_url(
                                            &refresh_inner,
                                            &refresh_context,
                                            &mut playlist_url,
                                            successful_playlist_url,
                                        )
                                        .await;
                                    } else {
                                        log::error!(
                                            "[eplus] Re-login refresh could not validate either playlist URL."
                                        );
                                    }
                                }
                                Err(_) => {
                                    log::error!(
                                        "[eplus] Re-login succeeded but event data refresh still failed."
                                    );
                                }
                            }
                        }
                        Err(_) => {
                            log::error!("[eplus] Re-login failed during cookie refresh.");
                        }
                    }
                }
            }
        });

        Ok(RefreshGuard {
            inner: inner_stream,
            cancel: Some(cancel_refresh),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::EplusSource;
    use crate::model::{DeliveryStatus, EplusEventData};
    use iori::IoriHttp;

    #[test]
    fn status_refresh_without_cloudfront_cookies_preserves_existing_cookies() {
        let http = IoriHttp::new(reqwest::Client::builder);
        let playlist_url = "https://stream.live.eplus.jp/out/v1/test/index.m3u8";
        let status_url = "https://live.eplus.jp/api/stream/test/status";

        http.add_cookies(
            vec![
                "CloudFront-Policy=old-policy; Domain=.live.eplus.jp; Path=/".to_string(),
                "CloudFront-Signature=old-signature; Domain=.live.eplus.jp; Path=/".to_string(),
                "CloudFront-Key-Pair-Id=old-key; Domain=.live.eplus.jp; Path=/".to_string(),
            ],
            playlist_url,
        );

        EplusSource::replace_session_cookies(
            &http,
            status_url,
            &["session=new-session; Domain=.live.eplus.jp; Path=/".to_string()],
        );

        let playlist_cookies = http.export_cookies_for_url(playlist_url);
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Policy=old-policy")
        );
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Signature=old-signature")
        );
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Key-Pair-Id=old-key")
        );
    }

    #[test]
    fn status_refresh_applies_cloudfront_cookies_to_their_playlist_path() {
        let http = IoriHttp::new(reqwest::Client::builder);
        let old_playlist = "https://stream.live.eplus.jp/out/v1/old/index.m3u8";
        let new_playlist = "https://stream.live.eplus.jp/out/v1/new/index.m3u8";
        let status_url = "https://live.eplus.jp/api/stream/test/status";

        http.add_cookies(
            vec![
                "CloudFront-Policy=old-policy; Domain=.live.eplus.jp; Path=/out/v1/old".to_string(),
                "CloudFront-Signature=old-signature; Domain=.live.eplus.jp; Path=/out/v1/old"
                    .to_string(),
                "CloudFront-Key-Pair-Id=old-key; Domain=.live.eplus.jp; Path=/out/v1/old"
                    .to_string(),
            ],
            old_playlist,
        );
        EplusSource::replace_session_cookies(
            &http,
            status_url,
            &[
                "CloudFront-Policy=new-policy; Domain=.live.eplus.jp; Path=/out/v1/new".to_string(),
                "CloudFront-Signature=new-signature; Domain=.live.eplus.jp; Path=/out/v1/new"
                    .to_string(),
                "CloudFront-Key-Pair-Id=new-key; Domain=.live.eplus.jp; Path=/out/v1/new"
                    .to_string(),
            ],
        );

        let playlist_cookies = http.export_cookies_for_url(new_playlist);
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Policy=new-policy")
        );
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Signature=new-signature")
        );
        assert!(
            playlist_cookies
                .iter()
                .any(|cookie| cookie == "CloudFront-Key-Pair-Id=new-key")
        );
    }

    #[test]
    fn refreshed_playlist_url_keeps_the_active_stream_kind() {
        let current = "https://stream.live.eplus.jp/out/v1/old/index.m3u8";
        let event_data = EplusEventData {
            app_id: "event".to_string(),
            title: "event".to_string(),
            delivery_status: DeliveryStatus::Started,
            archive_mode: Some("ON".to_string()),
            is_drm: false,
            m3u8_urls: vec![
                "https://vod.live.eplus.jp/out/v1/archive/index.m3u8".to_string(),
                "https://stream.live.eplus.jp/out/v1/new/index.m3u8".to_string(),
            ],
            stream_session: None,
            session_update_url: None,
            cloudfront_cookies: Vec::new(),
        };

        assert_eq!(
            EplusSource::refreshed_playlist_url(current, &event_data).as_deref(),
            Some("https://stream.live.eplus.jp/out/v1/new/index.m3u8")
        );
    }

    #[test]
    fn confirmed_archive_switches_to_archive_playlist_url() {
        let current = "https://stream.live.eplus.jp/out/v1/live/index.m3u8";
        let event_data = EplusEventData {
            app_id: "event".to_string(),
            title: "event".to_string(),
            delivery_status: DeliveryStatus::ConfirmedArchive,
            archive_mode: Some("ON".to_string()),
            is_drm: false,
            m3u8_urls: vec![
                "https://stream.live.eplus.jp/out/v1/live/index.m3u8".to_string(),
                "https://vod.live.eplus.jp/out/v1/archive/index.m3u8".to_string(),
            ],
            stream_session: None,
            session_update_url: None,
            cloudfront_cookies: Vec::new(),
        };

        assert_eq!(
            EplusSource::refreshed_playlist_url(current, &event_data).as_deref(),
            Some("https://vod.live.eplus.jp/out/v1/archive/index.m3u8")
        );
    }
}
