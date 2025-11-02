use std::time::Duration;

use iori_radiko::{RadikoClient, RadikoTime};
use shiori_plugin::{
    iori::{
        HttpClient,
        hls::HlsPlaylistSource,
        raw::RawRemoteSegment,
        reqwest::{Client, header::HeaderMap},
    },
    *,
};

pub struct RadikoPlugin;

impl ShioriPlugin for RadikoPlugin {
    fn name(&self) -> Cow<'static, str> {
        "radiko".into()
    }

    fn version(&self) -> Cow<'static, str> {
        "0.1.0".into()
    }

    fn description(&self) -> Option<Cow<'static, str>> {
        Some("Extracts Radiko live and timefree streams from the given URL.".into())
    }

    fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
        // Register live stream inspector
        registry.register_inspector(
            Regex::new(r"https?://(?:www\.)?radiko\.jp/#!/live/(?P<station>[A-Z0-9-_]+)").unwrap(),
            Box::new(RadikoLiveInspector),
            PriorityHint::Normal,
        );

        // Register live stream shorthand inspector
        registry.register_inspector(
            Regex::new(r"https?://(?:www\.)?radiko\.jp/#(?P<station>[A-Z0-9-_]+)").unwrap(),
            Box::new(RadikoLiveInspector),
            PriorityHint::Normal,
        );

        // Register timefree inspector
        registry.register_inspector(
            Regex::new(
                r"https?://(?:www\.)?radiko\.jp/#!/ts/(?P<station>[A-Z0-9-_]+)/(?P<timestring>\d+)",
            )
            .unwrap(),
            Box::new(RadikoTimefreeInspector),
            PriorityHint::Normal,
        );

        Ok(())
    }
}

struct RadikoLiveInspector;

#[async_trait]
impl Inspect for RadikoLiveInspector {
    fn name(&self) -> Cow<'static, str> {
        "radiko-live".into()
    }

    async fn inspect(
        &self,
        _url: &str,
        captures: &Captures,
        _args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let station_id = captures.name("station").unwrap().as_str();

        let mut client = RadikoClient::new();

        // Get station region
        let region = client.get_station_region(station_id).await?;

        // Authenticate
        let auth_data = client.authenticate(&region).await?;

        // Get station info
        let station_info = client.get_station_info(&region, station_id).await?;

        // Get stream URLs
        let stream_urls = client.get_live_stream_urls(station_id, &auth_data).await?;

        if stream_urls.is_empty() {
            return Ok(InspectResult::None);
        }

        // Use the first available stream URL
        let stream = &stream_urls[0];

        Ok(InspectResult::Playlist(InspectPlaylist {
            title: Some(format!("{} - Live", station_info.name)),
            playlist_url: stream.url.to_string(),
            playlist_type: PlaylistType::HLS,
            headers: vec![
                format!("X-Radiko-AuthToken: {}", auth_data.auth_token),
                format!("X-Radiko-AreaId: {}", auth_data.area_id),
            ],
            ..Default::default()
        }))
    }
}

struct RadikoTimefreeInspector;

#[async_trait]
impl Inspect for RadikoTimefreeInspector {
    fn name(&self) -> Cow<'static, str> {
        "radiko-timefree".into()
    }

    async fn inspect(
        &self,
        _url: &str,
        captures: &Captures,
        _args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let station_id = captures.name("station").unwrap().as_str();
        let timestring = captures.name("timestring").unwrap().as_str();

        let mut client = RadikoClient::new();

        // Parse the timestring
        let time = RadikoTime::from_timestring(timestring)?;

        // Get station region
        let region = client.get_station_region(station_id).await?;

        // Get programme info
        let programme_info = client.get_programme_info(station_id, &time).await?;

        // Parse start and end times
        let mut start_time = RadikoTime::from_timestring(&programme_info.ft)?;
        let end_time = RadikoTime::from_timestring(&programme_info.to)?;

        // Check if programme is still available
        let now = RadikoTime::now();
        let (expiry_free, expiry_tf30) = end_time.expiry();

        if expiry_tf30 < now.inner() {
            return Err(anyhow!("Programme is no longer available"));
        }

        let _need_tf30 = expiry_free < now.inner();

        // Authenticate
        let auth_data = client.authenticate(&region).await?;

        // Get station info
        let station_info = client.get_station_info(&region, station_id).await?;

        let mut builder = Client::builder();
        let mut headers = HeaderMap::new();
        headers.insert("X-Radiko-AuthToken", auth_data.auth_token.parse().unwrap());
        headers.insert("X-Radiko-AreaId", auth_data.area_id.parse().unwrap());
        builder = builder.default_headers(headers);
        let http_client = HttpClient::new(builder);

        let mut index = 0;
        let mut all_segments = Vec::new();
        while start_time.inner() < end_time.inner() {
            // Get timefree stream URLs
            let stream_urls = client
                .get_timefree_stream_urls(station_id, &start_time, &end_time, &auth_data)
                .await?;

            if stream_urls.is_empty() {
                return Ok(InspectResult::None);
            }

            // Use the first available stream URL
            let stream = &stream_urls[0];
            let mut source = HlsPlaylistSource::new(http_client.clone(), stream.url.clone(), None);
            let latest_media_sequences = source.load_streams(3).await?;
            let (segments, _) = source.load_segments(&latest_media_sequences, 3).await?;

            for segment in segments.into_iter().flatten() {
                all_segments.push(RawRemoteSegment {
                    url: segment.url,
                    filename: segment.filename,
                    range: None,
                    stream_id: segment.stream_id,
                    sequence: index,
                });
                index += 1;
            }

            start_time += Duration::from_secs(300);
        }

        Ok(InspectResult::Playlist(InspectPlaylist {
            // [20251030-0100][ニッポン放送]乃木坂46のオールナイトニッポン - 乃木坂46(久保史緒里)
            title: Some(format!(
                "[{}][{}] {} - {}",
                start_time.timestring(),
                station_info.name,
                programme_info.title,
                programme_info.performer.unwrap_or_default()
            )),
            playlist_url: "".to_string(),
            playlist_type: PlaylistType::RawRemoteSegments(all_segments),
            headers: vec![
                format!("X-Radiko-AuthToken: {}", auth_data.auth_token),
                format!("X-Radiko-AreaId: {}", auth_data.area_id),
            ],
            ..Default::default()
        }))
    }
}
