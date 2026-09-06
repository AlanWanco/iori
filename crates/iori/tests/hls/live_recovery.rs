use futures::StreamExt;
use iori::{InitialSegment, StreamingSource, context::IoriContext, hls::HlsLiveSource};
use reqwest::Client;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::time::{Duration, timeout};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path},
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

struct ReusedSegmentUriResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for ReusedSegmentUriResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let media_sequence = if call == 0 { 100 } else { 0 };
        ResponseTemplate::new(200).set_body_string(format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n#EXTINF:10.0,\nreused.ts\n"
        ))
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

#[tokio::test]
async fn live_source_detects_restart_when_segment_uri_is_reused() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ReusedSegmentUriResponder {
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
    assert_eq!(first_batch[0].media_sequence, 100);

    let second_batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("restarted playlist should arrive")?;
    assert_eq!(second_batch[0].media_sequence, 0);

    Ok(())
}

#[tokio::test]
async fn live_source_switches_master_playlist_url_without_resetting_sequence() -> anyhow::Result<()>
{
    let mock_server = MockServer::start().await;
    let old_master = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=100,RESOLUTION=640x360\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200,RESOLUTION=1280x720\nhigh.m3u8\n";
    let new_master = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=100,RESOLUTION=640x360\nlow-new.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200,RESOLUTION=1280x720\nhigh-new.m3u8\n";

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(old_master))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rotated.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(new_master))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/high.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(0, 1)))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/high-new.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(1, 1)))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/segment0.ts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes([0]))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/segment1.ts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes([1]))
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first_batch = timeout(Duration::from_secs(2), stream.next())
        .await?
        .expect("first batch should arrive")?;
    assert_eq!(first_batch[0].media_sequence, 0);
    assert_eq!(first_batch[0].sequence, 0);

    assert!(
        source
            .update_playlist_url(&context, &format!("{}/rotated.m3u8", mock_server.uri()))
            .await?
    );

    let second_batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("rotated master playlist should arrive")?;
    assert_eq!(second_batch[0].media_sequence, 1);
    assert_eq!(second_batch[0].sequence, 1);

    Ok(())
}

#[tokio::test]
async fn live_source_switches_playlist_url_without_resetting_sequence() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(0, 1)))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rotated.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(media_playlist(1, 1)))
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first_batch = timeout(Duration::from_secs(2), stream.next())
        .await?
        .expect("first batch should arrive")?;
    assert_eq!(first_batch[0].media_sequence, 0);
    assert_eq!(first_batch[0].sequence, 0);

    assert!(
        source
            .update_playlist_url(&context, &format!("{}/rotated.m3u8", mock_server.uri()))
            .await?
    );

    let second_batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("rotated playlist should arrive")?;
    assert_eq!(second_batch[0].media_sequence, 1);
    assert_eq!(second_batch[0].sequence, 1);

    Ok(())
}

#[tokio::test]
async fn live_source_propagates_failed_initial_segment() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:10.0,\nsegment0.m4s\n#EXT-X-ENDLIST\n",
        ))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/init.mp4"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;
    let result = timeout(Duration::from_secs(3), stream.next()).await?;

    assert!(matches!(result, Some(Err(_))));
    Ok(())
}

#[tokio::test]
async fn live_source_requests_initial_segment_byte_range() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;
    let initial_segment = b"initial bytes";

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"12@5\"\n#EXTINF:10.0,\nsegment0.m4s\n#EXT-X-ENDLIST\n",
        ))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/init.mp4"))
        .and(header("Range", "bytes=5-16"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(initial_segment))
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;
    let batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("initial segment should arrive")?;

    assert_eq!(batch.len(), 1);
    assert_eq!(
        batch[0].initial_segment,
        InitialSegment::Clear(std::sync::Arc::new(initial_segment.to_vec()))
    );
    Ok(())
}

struct KeyTimeoutResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for KeyTimeoutResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let response = ResponseTemplate::new(200).set_body_bytes(vec![0; 16]);
        if call == 0 {
            response.set_delay(Duration::from_millis(100))
        } else {
            response
        }
    }
}

struct EncryptedPlaylistResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for EncryptedPlaylistResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let media_sequence = self.calls.fetch_add(1, Ordering::SeqCst) as u64;
        ResponseTemplate::new(200).set_body_string(format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000000\n#EXTINF:1.0,\nsegment{media_sequence}.ts\n"
        ))
    }
}

#[tokio::test]
async fn live_source_recovers_after_encryption_key_timeout() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;
    let key_calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000000\n#EXTINF:1.0,\nsegment0.ts\n",
        ))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/key.bin"))
        .respond_with(KeyTimeoutResponder {
            calls: key_calls.clone(),
        })
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext {
        client: Client::builder()
            .timeout(Duration::from_millis(20))
            .build()?,
        segment_retries: 1,
        ..IoriContext::default()
    };
    let mut stream = source.segments_stream(&context).await?;

    let batch = timeout(Duration::from_secs(5), stream.next())
        .await?
        .expect("live source should recover after a key timeout")?;
    assert_eq!(batch.len(), 1);
    assert_eq!(key_calls.load(Ordering::SeqCst), 2);

    Ok(())
}

#[tokio::test]
async fn live_source_reuses_an_unchanged_encryption_key() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;
    let playlist_calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(EncryptedPlaylistResponder {
            calls: playlist_calls.clone(),
        })
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/key.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0; 16]))
        .expect(1)
        .mount(&mock_server)
        .await;

    let source = HlsLiveSource::new(format!("{}/playlist.m3u8", mock_server.uri()), None)?;
    let context = IoriContext::default();
    let mut stream = source.segments_stream(&context).await?;

    let first_batch = timeout(Duration::from_secs(2), stream.next())
        .await?
        .expect("first encrypted batch should arrive")?;
    assert_eq!(first_batch[0].media_sequence, 0);

    let second_batch = timeout(Duration::from_secs(3), stream.next())
        .await?
        .expect("second encrypted batch should arrive")?;
    assert_eq!(second_batch[0].media_sequence, 1);
    assert_eq!(playlist_calls.load(Ordering::SeqCst), 2);

    Ok(())
}
