pub mod model;
pub mod source;

use anyhow::{Context, bail};
use model::*;
use regex::Regex;
use reqwest::{Client, ClientBuilder, header::{HeaderMap, HeaderValue}};

const CLOUDFRONT_COOKIE_NAMES: &[&str] = &[
    "CloudFront-Policy",
    "CloudFront-Signature",
    "CloudFront-Key-Pair-Id",
];

#[derive(Clone)]
pub struct EplusClient {
    client: Client,
}

impl EplusClient {
    /// Create a new EplusClient without login (anonymous access).
    ///
    /// The `builder` should already have a cookie provider (e.g., from [`IoriHttp::builder()`]).
    /// Do NOT call `.cookie_store(true)` on it — that would override the shared cookie store.
    pub fn new(builder: ClientBuilder) -> anyhow::Result<Self> {
        let client = builder
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;
        Ok(Self { client })
    }

    /// Login to eplus.jp and return a client with session cookies.
    ///
    /// Implements the 2-step auth flow:
    /// 1. POST to FTAuth API with JSON credentials + X-CLTFT-Token
    /// 2. POST to auth form with form-encoded credentials
    ///
    /// The `builder` should already have a cookie provider (e.g., from [`IoriHttp::builder()`]).
    /// Login session cookies will be stored in that shared cookie store.
    pub async fn login(
        builder: ClientBuilder,
        eplus_url: &str,
        login_id: &str,
        password: &str,
    ) -> anyhow::Result<Self> {
        let client = builder
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        log::info!("Fetching auth state from {eplus_url}...");
        let res = client.get(eplus_url).send().await?;
        let final_url = res.url().clone();

        // Check if we're already logged in (not redirected to login page)
        if final_url.as_str().starts_with(eplus_url) && !final_url.as_str().contains("member/login")
        {
            log::info!("Already logged in or no login required.");
            return Ok(Self { client });
        }

        let auth_url = final_url.to_string();

        // Extract X-CLTFT-Token from response headers
        let cltft_token = res
            .headers()
            .get("X-CLTFT-Token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .context("Failed to get X-CLTFT-Token from response headers")?;

        // Step 1: POST to FTAuth API
        log::info!("Step 1: Sending pre-login API request...");
        let mut api_headers = HeaderMap::new();
        api_headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/json; charset=UTF-8"),
        );
        api_headers.insert("Referer", HeaderValue::from_str(&auth_url)?);
        api_headers.insert("X-Cltft-Token", HeaderValue::from_str(&cltft_token)?);
        api_headers.insert("Accept", HeaderValue::from_static("*/*"));

        let login_payload = serde_json::json!({
            "loginId": login_id,
            "loginPassword": password,
        });

        let api_res = client
            .post("https://live.eplus.jp/member/api/v1/FTAuth/idpw")
            .headers(api_headers)
            .json(&login_payload)
            .send()
            .await?;
        api_res.error_for_status_ref()?;

        let ft_auth: FtAuthResponse = api_res.json().await.context("Failed to parse FTAuth response")?;
        if !ft_auth.is_success {
            let error_msg = ft_auth
                .errors
                .map(|e| format!("{e}"))
                .unwrap_or_else(|| "Unknown error".to_string());
            bail!("Pre-login failed: {error_msg}");
        }

        // Step 2: POST form to auth URL
        log::info!("Step 2: Submitting login form...");
        let mut form_headers = HeaderMap::new();
        form_headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        form_headers.insert("Referer", HeaderValue::from_str(&auth_url)?);

        let form_data = [
            ("loginId", login_id),
            ("loginPassword", password),
            ("Token.Default", &cltft_token),
            ("op", "nextPage"),
        ];

        let form_res = client
            .post(&auth_url)
            .headers(form_headers)
            .form(&form_data)
            .send()
            .await?;
        form_res.error_for_status_ref()?;

        let final_login_url = form_res.url().to_string();
        if final_login_url.contains("member/login") {
            bail!("Login failed: redirected back to login page. URL: {final_login_url}");
        }

        log::info!("Login successful. Final URL: {final_login_url}");
        Ok(Self { client })
    }

    /// Fetch and parse an eplus event page to extract stream data.
    ///
    /// Extracts:
    /// - `var app = {...};` -> app_id, title, delivery_status, drm_mode
    /// - `var listChannels = [...]` -> m3u8 URLs
    /// - `var streamSession = '...'` -> session ID
    /// - CloudFront cookies from the session cookie jar
    pub async fn get_event_data(&self, eplus_url: &str) -> anyhow::Result<EplusEventData> {
        log::info!("Fetching event data from {eplus_url}...");
        let res = self.client.get(eplus_url).send().await?;
        res.error_for_status_ref()?;

        // Collect CloudFront cookies from the response/session
        let cloudfront_cookies = self.extract_cloudfront_cookies(&res);

        let body = res.text().await?;
        self.parse_event_page(&body, cloudfront_cookies)
    }

    /// Parse the HTML body of an eplus event page.
    fn parse_event_page(
        &self,
        body: &str,
        cloudfront_cookies: Vec<(String, String)>,
    ) -> anyhow::Result<EplusEventData> {
        // Parse `var app = {...};`
        let app_re =
            Regex::new(r"<script>\s*var\s+app\s*=\s*(?P<data>\{.+?\});\s*</script>").unwrap();
        let app_data: EplusAppData = app_re
            .captures(body)
            .and_then(|caps| caps.name("data"))
            .map(|m| serde_json::from_str(m.as_str()))
            .transpose()
            .context("Failed to parse app data JSON")?
            .context("Could not find `var app = {...}` in page")?;

        if app_data.is_pass_ticket.as_deref() == Some("YES") {
            bail!("Pass ticket pages are not supported. Use the player URL instead.");
        }

        let delivery_status = app_data
            .delivery_status
            .as_deref()
            .map(DeliveryStatus::from_str)
            .unwrap_or(DeliveryStatus::Unknown("missing".to_string()));

        let is_drm = app_data.drm_mode.as_deref() == Some("ON");
        if is_drm {
            log::warn!("This stream is DRM-protected. Download may not work.");
        }

        match &delivery_status {
            DeliveryStatus::Preparing => log::warn!("Event has not started yet."),
            DeliveryStatus::Started => log::info!("Event is currently live."),
            DeliveryStatus::Stopped => {
                let archive = app_data.archive_mode.as_deref() == Some("ON");
                if archive {
                    log::warn!("Event has ended; archive not yet available.");
                } else {
                    log::warn!("Event has ended with no archive.");
                }
            }
            DeliveryStatus::WaitConfirmArchived => {
                log::warn!("Event ended; archive will be available soon.");
            }
            DeliveryStatus::ConfirmedArchive => {
                log::info!("Event ended; archive is available.");
            }
            DeliveryStatus::Unknown(s) => log::warn!("Unknown delivery status: {s}"),
        }

        // Parse `var listChannels = [...]`
        let channels_re =
            Regex::new(r"var\s+listChannels\s*=\s*(?P<list>\[.+?\]);").unwrap();
        let m3u8_urls: Vec<String> = channels_re
            .captures(body)
            .and_then(|caps| caps.name("list"))
            .and_then(|m| {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(m.as_str());
                parsed.ok()
            })
            .map(|val| {
                // listChannels can be an array of strings or an array of objects with "url" key
                match val {
                    serde_json::Value::Array(arr) => arr
                        .into_iter()
                        .filter_map(|item| match item {
                            serde_json::Value::String(s) => Some(s),
                            serde_json::Value::Object(obj) => {
                                obj.get("url").and_then(|v| v.as_str().map(|s| s.to_string()))
                            }
                            _ => None,
                        })
                        .collect(),
                    _ => vec![],
                }
            })
            .unwrap_or_default();

        // Parse `var streamSession = '...'`
        let session_re =
            Regex::new(r#"var\s+streamSession\s*=\s*(['"])(?P<session>(?:(?!\1).)+)\1;"#).unwrap();
        let stream_session = session_re
            .captures(body)
            .and_then(|caps| caps.name("session"))
            .map(|m| m.as_str().to_string());

        let title = app_data
            .app_name
            .unwrap_or_else(|| "Eplus Event".to_string());

        Ok(EplusEventData {
            app_id: app_data.app_id,
            title,
            delivery_status,
            is_drm,
            m3u8_urls,
            stream_session,
            cloudfront_cookies,
        })
    }

    /// Extract CloudFront cookies from the response and the client cookie jar.
    fn extract_cloudfront_cookies(&self, res: &reqwest::Response) -> Vec<(String, String)> {
        let mut cookies = Vec::new();

        // Extract from response cookies
        for cookie in res.cookies() {
            if CLOUDFRONT_COOKIE_NAMES.contains(&cookie.name()) {
                cookies.push((cookie.name().to_string(), cookie.value().to_string()));
            }
        }

        cookies
    }

    /// Given a list of m3u8 URLs from event data, categorize them and find the best one.
    ///
    /// Returns the master playlist URL (live preferred over VOD unless `prefer_archive` is true).
    pub fn select_best_playlist(
        m3u8_urls: &[String],
        prefer_archive: bool,
    ) -> Option<String> {
        let common_id_re = Regex::new(r"/out/v1/([0-9a-fA-F]{32})/").unwrap();

        let mut live_urls: Vec<String> = Vec::new();
        let mut vod_urls: Vec<String> = Vec::new();
        let mut common_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for url in m3u8_urls {
            if url.contains("stream.live.eplus.jp") {
                live_urls.push(url.clone());
            } else if url.contains("vod.live.eplus.jp") {
                vod_urls.push(url.clone());
            }

            if let Some(caps) = common_id_re.captures(url) {
                if let Some(id) = caps.get(1) {
                    common_ids.insert(id.as_str().to_string());
                }
            }
        }

        // Construct potential live URLs from common IDs
        for common_id in &common_ids {
            let potential = format!("https://stream.live.eplus.jp/out/v1/{common_id}/index.m3u8");
            if !live_urls.contains(&potential) {
                log::info!("Constructed potential live URL from common ID: {potential}");
                live_urls.push(potential);
            }
        }

        // Select based on preference
        if prefer_archive {
            vod_urls.into_iter().next().or_else(|| live_urls.into_iter().next())
        } else {
            live_urls.into_iter().next().or_else(|| vod_urls.into_iter().next())
        }
    }

    /// Get the underlying HTTP client for manual requests if needed.
    pub fn client(&self) -> &Client {
        &self.client
    }
}
