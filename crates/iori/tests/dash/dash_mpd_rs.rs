use futures::StreamExt;
use iori::{
    StreamingSource,
    context::IoriContext,
    dash::{archive::CommonDashArchiveSource, live::CommonDashLiveSource},
};

use crate::{AssertWrapper, dash::setup_mock_server};

#[tokio::test]
async fn test_static_a2d_tv() -> anyhow::Result<()> {
    let data = include_str!("../fixtures/dash/dash-mpd-rs/a2d-tv.mpd");
    let (playlist_uri, _server) = setup_mock_server(data).await;

    let context = IoriContext::default();
    let playlist = CommonDashLiveSource::new(playlist_uri.parse()?, None)?;

    let mut stream = playlist.segments_stream(&context).await?;

    let segments_live = stream.next().await.assert_success()?;
    assert_eq!(segments_live.len(), 1896);
    // no further segments
    stream.next().await.assert_error();

    let playlist = CommonDashArchiveSource::new(playlist_uri.parse()?, None)?;
    let mut stream = playlist.segments_stream(&context).await?;

    let mut segments_archive = Vec::new();
    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 644);
    segments_archive.extend(segments);
    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 636);
    segments_archive.extend(segments);
    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 616);
    segments_archive.extend(segments);
    // no further segments
    stream.next().await.assert_error();

    for (i, segment) in segments_archive.iter().enumerate() {
        assert_eq!(segment.url, segments_live[i].url);
        assert_eq!(segment.initial_segment, segments_live[i].initial_segment);
        assert_eq!(segment.byte_range, segments_live[i].byte_range);
    }

    Ok(())
}

#[tokio::test]
async fn test_dash_testcases_5b_1_thomson() -> anyhow::Result<()> {
    let data = include_str!("../fixtures/dash/dash-mpd-rs/dash-testcases-5b-1-thomson.mpd");
    let (playlist_uri, _server) = setup_mock_server(data).await;

    let context = IoriContext::default();
    let playlist = CommonDashLiveSource::new(playlist_uri.parse()?, None)?;

    let mut stream = playlist.segments_stream(&context).await?;

    let segments_live = stream.next().await.assert_success()?;
    assert_eq!(segments_live.len(), 248);
    // no further segments
    stream.next().await.assert_error();

    let playlist = CommonDashArchiveSource::new(playlist_uri.parse()?, None)?;
    let mut stream = playlist.segments_stream(&context).await?;

    let mut segments_archive = Vec::new();
    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 45);
    segments_archive.extend(segments);

    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 45);
    segments_archive.extend(segments);

    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 30);
    segments_archive.extend(segments);

    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 30);
    segments_archive.extend(segments);

    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 49);
    segments_archive.extend(segments);

    let segments = stream.next().await.assert_success()?;
    assert_eq!(segments.len(), 49);
    segments_archive.extend(segments);

    stream.next().await.assert_error();

    for (i, segment) in segments_archive.iter().enumerate() {
        assert_eq!(segment.url, segments_live[i].url);
        assert_eq!(segment.initial_segment, segments_live[i].initial_segment);
        assert_eq!(segment.byte_range, segments_live[i].byte_range);
    }

    Ok(())
}
