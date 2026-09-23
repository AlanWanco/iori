use futures::StreamExt;
use iori::{StreamingSource, context::IoriContext, hls::HlsLiveSource};
use reqwest::Url;
use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::time::timeout;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn media_playlist(sequence: u64) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n#EXTINF:1.0,\nsegment-{sequence}.ts\n"
    )
}

#[tokio::test]
async fn initial_manifest_failure_recovers_with_a_fresh_url() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let expired_url = format!("{}/expired.m3u8", server.uri());
    let fresh_url = format!("{}/fresh.m3u8", server.uri());

    Mock::given(method("GET"))
        .and(path("/expired.m3u8"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fresh.m3u8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("{}#EXT-X-ENDLIST\n", media_playlist(5))),
        )
        .mount(&server)
        .await;

    let recovery_url = fresh_url.clone();
    let source = HlsLiveSource::new(expired_url, None)?.with_manifest_recovery(move || {
        let url = recovery_url.clone();
        async move { Url::parse(&url).ok() }
    });
    let context = IoriContext::default();
    let mut stream = timeout(Duration::from_secs(6), source.segments_stream(&context)).await??;
    let batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("fresh playlist segments should arrive")?;

    assert_eq!(batch[0].media_sequence, 5);
    assert_eq!(batch[0].sequence, 0);
    Ok(())
}

#[tokio::test]
async fn live_source_recovers_after_manifest_url_expires() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let initial_url = format!("{}/playlist.m3u8", server.uri());
    let recovered_url = format!("{}/recovered.m3u8", server.uri());

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(0)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/recovered.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(1)))
        .mount(&server)
        .await;

    let recovery_calls = Arc::new(AtomicUsize::new(0));
    let recovery_count = recovery_calls.clone();
    let source = HlsLiveSource::new(initial_url, None)?.with_manifest_recovery(move || {
        let url = recovered_url.clone();
        recovery_count.fetch_add(1, Ordering::SeqCst);
        async move { Url::parse(&url).ok() }
    });
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("initial segment should arrive")?;
    assert_eq!(first[0].media_sequence, 0);
    assert_eq!(first[0].sequence, 0);

    let recovered = timeout(Duration::from_secs(10), stream.next())
        .await?
        .expect("a segment from the refreshed playlist should arrive")?;
    assert_eq!(recovered[0].media_sequence, 1);
    assert_eq!(recovered[0].sequence, 1);
    assert_eq!(recovery_calls.load(Ordering::SeqCst), 1);

    Ok(())
}
