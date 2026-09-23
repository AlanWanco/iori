use crate::model::{
    FcContentProviderResponse, FcVideoPageResponse, SessionIdResponse, SiteSettings,
};
use fake_user_agent::get_chrome_rua;
use reqwest::{
    Client,
    header::{HeaderValue, ORIGIN, REFERER, USER_AGENT},
};
use serde_json::json;

#[derive(Clone)]
pub struct SheetaClient {
    api_base_url: String,
    origin: String,

    client: Client,
}

impl SheetaClient {
    pub fn site_regex(host: &str) -> regex::Regex {
        regex::Regex::new(&format!(
            r#"https://(?<host>{})/(?:[^/]+/)?(?:video|live)/(?<video_id>.+)"#,
            host.replace(".", "\\.")
        ))
        .unwrap()
    }

    pub fn wild_regex() -> regex::Regex {
        regex::Regex::new(r#"https://(?<host>[^/]+)/(?:[^/]+/)?(?:video|live)/(?<video_id>.+)"#)
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
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            .header("fc_site_id", fc_site_id) // FIXME: get correct site_id
            .header("fc_use_device", HeaderValue::from_static("null"))
            .send()
            .await?
            .json()
            .await?;
        Ok(response)
    }

    pub async fn get_session_id(&self, fc_site_id: i32, video_id: &str) -> anyhow::Result<String> {
        let url = format!("{}/video_pages/{}/session_ids", self.api_base_url, video_id);
        let response: SessionIdResponse = self
            .client
            .post(url)
            .header(USER_AGENT, get_chrome_rua())
            .header(ORIGIN, HeaderValue::from_str(self.origin())?)
            // .bearer_auth("")
            .header("fc_site_id", fc_site_id)
            .header("fc_use_device", HeaderValue::from_static("null"))
            .json(&json!({}))
            .send()
            .await?
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
            .map_err(|_| anyhow::anyhow!("Failed to probe the Sheeta HLS playlist."))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to read the Sheeta HLS probe response."))?;

        Ok(status.is_success() && is_hls_playlist(&body))
    }

    pub fn origin(&self) -> &str {
        &self.origin
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
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn test_regex() {
        let regex = SheetaClient::site_regex("nicochannel.jp");

        let captures = regex
            .captures("https://nicochannel.jp/not-equal-me-plus/video/smLzU6uZ2LnUvqeDBtoXSxvr")
            .unwrap();
        assert_eq!(
            captures.name("video_id").unwrap().as_str(),
            "smLzU6uZ2LnUvqeDBtoXSxvr"
        );

        let captures = regex
            .captures("https://nicochannel.jp/video/smLzU6uZ2LnUvqeDBtoXSxvr")
            .unwrap();
        assert_eq!(
            captures.name("video_id").unwrap().as_str(),
            "smLzU6uZ2LnUvqeDBtoXSxvr"
        );
    }

    #[tokio::test]
    #[ignore = "requires the live Sheeta API"]
    async fn test_get_session_id() {
        let client = SheetaClient::nico_channel_plus(Default::default());
        let session_id = client
            .get_session_id(0, "smHLeLu9aQtR3taSjgCdEqvC")
            .await
            .unwrap();
        assert!(!session_id.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires the live HLS authorization endpoint"]
    async fn test_get_video_url() -> anyhow::Result<()> {
        let client = SheetaClient::new(
            "https://api.nicochannel.jp".to_string(),
            "https://nicochannel.jp".to_string(),
            Default::default(),
        );

        let video_url = client
            .get_video_url("39447efb-e081-4b16-8984-7ee8da96bfe0")
            .await;
        let response = reqwest::get(video_url).await?;
        assert!(response.status().is_success());
        Ok(())
    }

    #[test]
    fn test_is_hls_playlist() {
        assert!(is_hls_playlist(b"#EXTM3U\n#EXT-X-VERSION:3\n"));
        assert!(is_hls_playlist(b"\xef\xbb\xbf#EXTM3U\n"));
        assert!(!is_hls_playlist(
            br#"<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code></Error>"#
        ));
    }

    #[tokio::test]
    async fn probe_requires_a_successful_hls_playlist_response() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ready"))
            .respond_with(ResponseTemplate::new(200).set_body_string("#EXTM3U\n#EXT-X-VERSION:3\n"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/not-playlist"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<Error>NoSuchKey</Error>"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/http-error"))
            .respond_with(ResponseTemplate::new(403).set_body_string("#EXTM3U\n"))
            .mount(&server)
            .await;

        let client = SheetaClient::new(
            "https://api.example.test".to_string(),
            server.uri(),
            reqwest::Client::new(),
        );
        assert!(
            client
                .probe_video_url(&format!("{}/ready", server.uri()))
                .await?
        );
        assert!(
            !client
                .probe_video_url(&format!("{}/not-playlist", server.uri()))
                .await?
        );
        assert!(
            !client
                .probe_video_url(&format!("{}/http-error", server.uri()))
                .await?
        );

        Ok(())
    }
}
