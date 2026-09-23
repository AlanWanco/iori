use futures::StreamExt;
use iori::{StreamingSource, context::IoriContext, hls::HlsLiveSource};
use std::time::Duration;
use tokio::time::timeout;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn media_playlist(media_sequence: u64, segment_count: u64) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"
    );
    for sequence in media_sequence..media_sequence + segment_count {
        playlist.push_str(&format!("#EXTINF:1.0,\nsegment-{sequence}.ts\n"));
    }
    playlist
}

#[tokio::test]
async fn initial_segment_limit_keeps_latest_segments_and_contiguous_sequences() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let playlist_url = format!("{}/playlist.m3u8", server.uri());

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(10, 4)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(12, 3)))
        .mount(&server)
        .await;

    let source = HlsLiveSource::new(playlist_url, None)?.with_initial_segment_limit(Some(2));
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("initial segment batch should arrive")?;
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].media_sequence, 12);
    assert_eq!(first[0].sequence, 0);
    assert_eq!(first[1].media_sequence, 13);
    assert_eq!(first[1].sequence, 1);

    let next = timeout(Duration::from_secs(8), stream.next())
        .await?
        .expect("next live segment should arrive")?;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].media_sequence, 14);
    assert_eq!(next[0].sequence, 2);

    Ok(())
}

#[tokio::test]
async fn initial_segment_limit_does_not_truncate_vod_playlists() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let playlist_url = format!("{}/playlist.m3u8", server.uri());
    let playlist = media_playlist(30, 4) + "#EXT-X-ENDLIST\n";
    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(playlist))
        .mount(&server)
        .await;

    let source = HlsLiveSource::new(playlist_url, None)?.with_initial_segment_limit(Some(2));
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;
    let batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("all VOD segments should arrive")?;

    assert_eq!(batch.len(), 4);
    assert_eq!(batch[0].sequence, 0);
    assert_eq!(batch[3].sequence, 3);

    Ok(())
}

#[tokio::test]
async fn zero_initial_segment_limit_is_treated_as_unset() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let playlist_url = format!("{}/playlist.m3u8", server.uri());
    let playlist = media_playlist(20, 2) + "#EXT-X-ENDLIST\n";
    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(playlist))
        .mount(&server)
        .await;

    let source = HlsLiveSource::new(playlist_url, None)?.with_initial_segment_limit(Some(0));
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;
    let batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("all playlist segments should arrive")?;

    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].sequence, 0);
    assert_eq!(batch[1].sequence, 1);

    Ok(())
}
