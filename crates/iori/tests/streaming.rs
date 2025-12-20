use iori::{
    cache::memory::{MemoryCacheSource, MemoryEntry},
    download::ParallelDownloader,
    download::TracingApp,
    hls::archive::{CommonM3u8ArchiveSource, SegmentRange},
    merge::SkipMerger,
};
use std::sync::Arc;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_hls_download_integration() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    let m3u8_content = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:10.0,
segment0.ts
#EXTINF:10.0,
segment1.ts
#EXT-X-ENDLIST
"#;

    Mock::given(method("GET"))
        .and(path("/playlist.m3u8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(m3u8_content))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/segment0.ts"))
        .respond_with(ResponseTemplate::new(200).set_body_string("segment0 content"))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/segment1.ts"))
        .respond_with(ResponseTemplate::new(200).set_body_string("segment1 content"))
        .mount(&mock_server)
        .await;

    let playlist_url = format!("{}/playlist.m3u8", mock_server.uri());
    let source = CommonM3u8ArchiveSource::new(playlist_url, None, SegmentRange::default())?;
    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    assert_eq!(result.len(), 2);

    // HlsArchiveSource uses stream_id 0 and sequences 0, 1
    match result.get(&(0, 0)).unwrap() {
        MemoryEntry::Data(data) => assert_eq!(data, b"segment0 content"),
        _ => panic!("Expected Data"),
    }
    match result.get(&(1, 0)).unwrap() {
        MemoryEntry::Data(data) => assert_eq!(data, b"segment1 content"),
        _ => panic!("Expected Data"),
    }

    Ok(())
}

#[tokio::test]
async fn test_dash_download_integration() -> anyhow::Result<()> {
    let mock_server = MockServer::start().await;

    let mpd_content = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
     xmlns="urn:mpeg:dash:schema:mpd:2011"
     xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd"
     profiles="urn:mpeg:dash:profile:isoff-live:2011"
     type="static"
     mediaPresentationDuration="PT20S"
     minBufferTime="PT1.5S">
    <Period id="0" duration="PT20S">
        <AdaptationSet id="0" contentType="video" segmentAlignment="true" bitstreamSwitching="true">
            <Representation id="0" mimeType="video/mp4" codecs="avc1.64001e" bandwidth="1000000" width="640" height="360" frameRate="30">
                <SegmentTemplate timescale="1000" initialization="init.mp4" media="segment$Number$.m4s" startNumber="1">
                    <SegmentTimeline>
                        <S t="0" d="10000" r="1" />
                    </SegmentTimeline>
                </SegmentTemplate>
            </Representation>
        </AdaptationSet>
    </Period>
</MPD>"#;

    Mock::given(method("GET"))
        .and(path("/manifest.mpd"))
        .respond_with(ResponseTemplate::new(200).set_body_string(mpd_content))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/init.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_string("init content"))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/segment1.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_string("segment1 content"))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/segment2.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_string("segment2 content"))
        .mount(&mock_server)
        .await;

    let mpd_url = Url::parse(&format!("{}/manifest.mpd", mock_server.uri()))?;
    let source = iori::dash::live::CommonDashLiveSource::new(mpd_url, None)?;
    let cache = Arc::new(MemoryCacheSource::new());

    ParallelDownloader::builder(Default::default())
        .app(TracingApp::default())
        .merger(SkipMerger)
        .cache(cache.clone())
        .ctrlc_handler()
        .download(source)
        .await?;

    let result = cache.into_inner();
    let result = result.lock().unwrap();
    println!("DASH Cache keys: {:?}", result.keys());
    // 2 media segments, each with init segment prepended
    assert_eq!(result.len(), 2);

    // HLS keys are (sequence, stream_id)
    // DASH keys are (time or sequence, stream_id)
    // For our MPD:
    // Segment 1: number=1, t=0       -> sequence=0, stream_id=0
    // Segment 2: number=2, t=10000   -> sequence=10000, stream_id=0

    match result.get(&(0, 0)).unwrap() {
        MemoryEntry::Data(data) => assert_eq!(data, b"init contentsegment1 content"),
        _ => panic!("Expected Data"),
    }
    match result.get(&(10000, 0)).unwrap() {
        MemoryEntry::Data(data) => assert_eq!(data, b"init contentsegment2 content"),
        _ => panic!("Expected Data"),
    }

    Ok(())
}
