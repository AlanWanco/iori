use crate::model::{
    FcContentProviderResponse, FcVideoPageResponse, SessionIdResponse, SiteSettings,
};
use fake_user_agent::get_chrome_rua;
use reqwest::{
    Client, Url,
    header::{ACCEPT, HeaderValue, ORIGIN, REFERER, USER_AGENT},
};
use serde_json::json;
use shiori_plugin::iori::hls::iori_hls::{KeyFormat, KeyMethod, Playlist, parse_playlist_res};

#[derive(Clone)]
pub struct SheetaClient {
    api_base_url: String,
    origin: String,

    client: Client,
}

impl SheetaClient {
    pub fn site_regex(host: &str) -> regex::Regex {
        regex::Regex::new(&format!(
            r#"https://(?<host>{})/(?:(?<channel>[^/?#]+)/)?(?:video|live)/(?<video_id>[^/?#]+)(?:[?#].*)?$"#,
            host.replace(".", "\\.")
        ))
        .unwrap()
    }

    pub fn wild_regex() -> regex::Regex {
        regex::Regex::new(
            r#"https://(?<host>[^/]+)/(?:(?<channel>[^/?#]+)/)?(?:video|live)/(?<video_id>[^/?#]+)(?:[?#].*)?$"#,
        )
        .unwrap()
    }

    pub fn nico_channel_plus(client: Client) -> Self {
        Self::new(
            "https://api.nicochannel.jp/fc".to_string(),
            "https://nicochannel.jp".to_string(),
            client,
        )
    }

    pub async fn common(domain: &str, client: Client) -> anyhow::Result<Self> {
        let settings: SiteSettings = client
            .get(format!("https://{domain}/site/portal/settings.json"))
            .header(USER_AGENT, get_chrome_rua())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Self::new(
            settings.api_base_url,
            format!("https://{domain}"),
            client,
        ))
    }

    pub(crate) fn new(base_url: String, origin: String, client: Client) -> Self {
        Self {
            api_base_url: base_url,
            origin,
            client,
        }
    }

    pub async fn get_fc_site_id(&self, channel_name: &str) -> anyhow::Result<i32> {
        let response: FcContentProviderResponse = self
            .client
            .get(format!(
                "{}/content_providers/channel_domain",
                self.api_base_url
            ))
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            .query(&[(
                "current_site_domain",
                format!("{}/{channel_name}", self.origin()),
            )])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.fc_site_id())
    }

    pub async fn get_video_data(
        &self,
        fc_site_id: i32,
        video_id: &str,
    ) -> anyhow::Result<FcVideoPageResponse> {
        // https://api.nicochannel.jp/fc/content_providers/channel_domain?current_site_domain=https:%2F%2Fnicochannel.jp%2Fnot-equal-me-plus
        let url = format!("{}/video_pages/{video_id}", self.api_base_url);
        let response: FcVideoPageResponse = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            .header("fc_site_id", fc_site_id)
            .header("fc_use_device", HeaderValue::from_static("null"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response)
    }

    pub async fn get_session_id(
        &self,
        fc_site_id: i32,
        video_id: &str,
        broadcast_type: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = format!("{}/video_pages/{}/session_ids", self.api_base_url, video_id);
        let response: SessionIdResponse = self
            .client
            .post(url)
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            // .bearer_auth("")
            .header("fc_site_id", fc_site_id)
            .header("fc_use_device", HeaderValue::from_static("null"))
            .json(&session_request_body(broadcast_type))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.session_id())
    }

    pub async fn get_video_url(&self, session_id: &str) -> String {
        // https://hls-auth.cloud.stream.co.jp/auth/index.m3u8?session_id=eeff71a4-5fa3-4f1f-9ced-c2c7894c79b8
        format!("https://hls-auth.cloud.stream.co.jp/auth/index.m3u8?session_id={session_id}")
    }

    /// Probe whether a newly-created session already exposes an HLS playlist.
    ///
    /// The session endpoint can succeed before the HLS object is published. In
    /// that window the CDN returns an XML `NoSuchKey` response, which must be
    /// treated as "not ready" so callers can retry the whole session flow.
    pub async fn probe_video_url(&self, video_url: &str) -> anyhow::Result<bool> {
        let response = self
            .client
            .get(video_url)
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            .header(REFERER, HeaderValue::from_str(self.origin())?)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to request the Sheeta HLS playlist probe."))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to read the Sheeta HLS playlist probe."))?;

        Ok(status.is_success() && is_hls_playlist(&body))
    }

    /// Fetch a Sheeta HLS manifest and return its AES-128 key as lowercase hex.
    /// Master playlists are followed through their highest-resolution variant.
    pub async fn get_hls_encryption_key(
        &self,
        playlist_url: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut url = Url::parse(playlist_url)
            .map_err(|_| anyhow::anyhow!("Invalid Sheeta HLS playlist URL."))?;

        for _ in 0..4 {
            let (response_url, body) = self.fetch_hls_resource(&url).await?;
            let playlist = parse_playlist_res(&body)
                .map_err(|_| anyhow::anyhow!("Failed to parse the Sheeta HLS manifest."))?;

            match playlist {
                Playlist::MasterPlaylist(mut master) => {
                    let variant = master
                        .variants
                        .drain(..)
                        .max_by_key(|variant| {
                            (
                                variant
                                    .resolution
                                    .map(|resolution| {
                                        resolution.width.saturating_mul(resolution.height)
                                    })
                                    .unwrap_or_default(),
                                variant.bandwidth,
                            )
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!("Sheeta master playlist has no variants.")
                        })?;
                    url = response_url
                        .join(&variant.uri)
                        .map_err(|_| anyhow::anyhow!("Invalid Sheeta media playlist URL."))?;
                }
                Playlist::MediaPlaylist(media) => {
                    let Some(key_url) = aes128_key_url(&response_url, &media)? else {
                        return Ok(None);
                    };
                    let (_, key_bytes) = self.fetch_hls_resource(&key_url).await?;
                    let key = format_aes128_key(&key_bytes)?;
                    return Ok(Some(key));
                }
            }
        }

        anyhow::bail!("Sheeta HLS manifest has too many master-playlist levels.")
    }

    async fn fetch_hls_resource(&self, url: &Url) -> anyhow::Result<(Url, Vec<u8>)> {
        let response = self
            .client
            .get(url.clone())
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            .header(REFERER, HeaderValue::from_str(self.origin())?)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to request the Sheeta HLS manifest or key."))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Sheeta HLS request failed with HTTP status {status}.");
        }
        let response_url = response.url().clone();
        let body = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to read the Sheeta HLS manifest or key."))?;
        Ok((response_url, body.to_vec()))
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }
}

fn aes128_key_url(
    playlist_url: &Url,
    playlist: &shiori_plugin::iori::hls::iori_hls::MediaPlaylist,
) -> anyhow::Result<Option<Url>> {
    let Some(key) = playlist
        .segments
        .iter()
        .filter_map(|segment| segment.key.as_ref())
        .find(|key| key.method != KeyMethod::None)
    else {
        return Ok(None);
    };

    if key.method != KeyMethod::AES128 || key.key_format != KeyFormat::Identity {
        return Ok(None);
    }
    let key_uri = key
        .uri
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Sheeta AES-128 key URI is missing."))?;
    let key_url = playlist_url
        .join(key_uri)
        .map_err(|_| anyhow::anyhow!("Invalid Sheeta AES-128 key URL."))?;
    Ok(Some(key_url))
}

fn format_aes128_key(key_bytes: &[u8]) -> anyhow::Result<String> {
    if key_bytes.len() != 16 {
        anyhow::bail!("Sheeta AES-128 key response is not 16 bytes.");
    }
    Ok(key_bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn session_request_body(broadcast_type: Option<&str>) -> serde_json::Value {
    match broadcast_type {
        Some("dvr") => json!({ "broadcast_type": "dvr" }),
        _ => json!({}),
    }
}

fn is_hls_playlist(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .lines()
        .any(|line| line.trim().trim_start_matches('\u{feff}').trim() == "#EXTM3U")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_regex() {
        let regex = SheetaClient::site_regex("nicochannel.jp");

        let captures = regex
            .captures("https://nicochannel.jp/not-equal-me-plus/video/smLzU6uZ2LnUvqeDBtoXSxvr")
            .unwrap();
        assert_eq!(captures.name("host").unwrap().as_str(), "nicochannel.jp");
        assert_eq!(
            captures.name("channel").unwrap().as_str(),
            "not-equal-me-plus"
        );
        assert_eq!(
            captures.name("video_id").unwrap().as_str(),
            "smLzU6uZ2LnUvqeDBtoXSxvr"
        );

        let captures = regex
            .captures("https://nicochannel.jp/video/smLzU6uZ2LnUvqeDBtoXSxvr")
            .unwrap();
        assert!(captures.name("channel").is_none());
        assert_eq!(
            captures.name("video_id").unwrap().as_str(),
            "smLzU6uZ2LnUvqeDBtoXSxvr"
        );

        let captures = regex
            .captures("https://nicochannel.jp/video/smLzU6uZ2LnUvqeDBtoXSxvr?from=archive")
            .unwrap();
        assert_eq!(
            captures.name("video_id").unwrap().as_str(),
            "smLzU6uZ2LnUvqeDBtoXSxvr"
        );
    }

    #[tokio::test]
    #[ignore = "requires a live Sheeta API session"]
    async fn test_get_session_id() {
        let client = SheetaClient::nico_channel_plus(Default::default());
        let session_id = client
            .get_session_id(0, "smHLeLu9aQtR3taSjgCdEqvC", None)
            .await
            .unwrap();
        println!("session_id: {}", session_id);
    }

    #[tokio::test]
    async fn test_get_video_url() {
        let client = SheetaClient::new(
            "https://api.nicochannel.jp".to_string(),
            "https://nicochannel.jp".to_string(),
            Default::default(),
        );

        let video_url = client
            .get_video_url("39447efb-e081-4b16-8984-7ee8da96bfe0")
            .await;
        assert_eq!(
            video_url,
            "https://hls-auth.cloud.stream.co.jp/auth/index.m3u8?session_id=39447efb-e081-4b16-8984-7ee8da96bfe0"
        );
    }

    #[test]
    fn test_resolves_aes128_key_url_and_formats_key() {
        let playlist_url = Url::parse("https://media.example.test/live/index.m3u8").unwrap();
        let playlist = parse_playlist_res(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"../keys/live.key\"\n#EXTINF:4,\nsegment.ts\n",
        )
        .unwrap();
        let Playlist::MediaPlaylist(playlist) = playlist else {
            panic!("Expected a media playlist");
        };

        assert_eq!(
            aes128_key_url(&playlist_url, &playlist).unwrap(),
            Some(Url::parse("https://media.example.test/keys/live.key").unwrap())
        );
        assert_eq!(
            format_aes128_key(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]).unwrap(),
            "000102030405060708090a0b0c0d0e0f"
        );
        assert!(format_aes128_key(&[0; 15]).is_err());
    }

    #[test]
    fn session_request_body_selects_dvr_only_when_requested() {
        assert_eq!(
            session_request_body(Some("dvr")),
            serde_json::json!({
                "broadcast_type": "dvr"
            })
        );
        assert_eq!(session_request_body(None), serde_json::json!({}));
    }

    #[test]
    fn test_is_hls_playlist() {
        assert!(is_hls_playlist(b"#EXTM3U\n#EXT-X-VERSION:3\n"));
        assert!(is_hls_playlist(b"\xef\xbb\xbf#EXTM3U\n"));
        assert!(!is_hls_playlist(
            br#"<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code></Error>"#
        ));
    }
}
