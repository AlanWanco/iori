use futures::StreamExt;
use iori::{StreamingSource, context::IoriContext, hls::HlsLiveSource};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::time::{Duration, timeout};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

struct PlaylistSequenceResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for PlaylistSequenceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => ResponseTemplate::new(200).set_body_string(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:10\n#EXTINF:10.0,\nsegment10.ts\n",
            ),
            1..=3 => ResponseTemplate::new(404).set_body_string("<h1>error 404</h1>"),
            _ => ResponseTemplate::new(200).set_body_string(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:10.0,\nsegment0.ts\n",
            ),
        }
    }
}

fn media_playlist(media_sequence: u64, segment_count: u64) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"
    );
    for offset in 0..segment_count {
        let sequence = media_sequence + offset;
        playlist.push_str(&format!("#EXTINF:10.0,\nsegment{sequence}.ts\n"));
    }
    playlist
}

struct StalePlaylistResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for StalePlaylistResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = match call {
            0 => media_playlist(996, 7),
            1 => media_playlist(996, 5),
            _ => media_playlist(1003, 1),
        };
        ResponseTemplate::new(200).set_body_string(body)
    }
}

#[tokio::test]
async fn live_source_recovers_after_manifest_gap_and_sequence_reset() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(PlaylistSequenceResponder {
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first_batch = timeout(Duration::from_secs(2), stream.next())
        .await?
        .expect("first batch should arrive")?;
    assert_eq!(first_batch.len(), 1);
    assert_eq!(first_batch[0].filename, "segment10.ts");
    assert_eq!(first_batch[0].media_sequence, 10);

    let second_batch = timeout(Duration::from_secs(10), stream.next())
        .await?
        .expect("second batch should arrive after recovery")?;
    assert_eq!(second_batch.len(), 1);
    assert_eq!(second_batch[0].filename, "segment0.ts");
    assert_eq!(second_batch[0].media_sequence, 0);

    Ok(())
}

#[tokio::test]
async fn live_source_ignores_overlapping_stale_playlist_window() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(StalePlaylistResponder {
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first_batch = timeout(Duration::from_secs(2), stream.next())
        .await?
        .expect("first batch should arrive")?;
    assert_eq!(first_batch.len(), 7);
    assert_eq!(first_batch[0].media_sequence, 996);
    assert_eq!(first_batch[6].media_sequence, 1002);

    let second_batch = timeout(Duration::from_secs(8), stream.next())
        .await?
        .expect("new segment should arrive after stale playlist")?;
    assert_eq!(second_batch.len(), 1);
    assert_eq!(second_batch[0].media_sequence, 1003);

    Ok(())
}
