use futures::StreamExt;
use iori::{StreamingSource, context::IoriContext, hls::HlsLiveSource};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::time::{Duration, timeout};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::{method, path}};

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
