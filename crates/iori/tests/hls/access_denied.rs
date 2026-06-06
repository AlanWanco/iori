use iori::{context::IoriContext, hls::utils::load_playlist_with_retry};
use reqwest::Url;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};

const ACCESS_DENIED_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>AccessDenied</Code><Message>Access denied</Message></Error>"#;
const PLAYLIST: &str = r#"#EXTM3U
#EXT-X-TARGETDURATION:10
#EXT-X-VERSION:3
#EXTINF:9.009,
segment0.ts
#EXT-X-ENDLIST"#;

#[tokio::test]
async fn retries_after_access_denied_playlist_response() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACCESS_DENIED_XML))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PLAYLIST))
        .mount(&mock_server)
        .await;

    let context = IoriContext::default();
    let playlist_url = Url::parse(&format!("{}/playlist.m3u8", mock_server.uri()))?;
    let playlist =
        load_playlist_with_retry(&context.client, &playlist_url, context.manifest_retries).await?;

    assert!(matches!(playlist, iori::hls::iori_hls::Playlist::MediaPlaylist(_)));

    Ok(())
}
