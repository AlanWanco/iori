use crate::{
    InitialSegment, SegmentFormat, StreamType,
    context::IoriContext,
    decrypt::IoriKey,
    error::IoriResult,
    hls::{segment::M3u8Segment, utils::load_m3u8},
};
use iori_hls::{AlternativeMedia, AlternativeMediaType, MediaPlaylist, Playlist};
use reqwest::{Client, Url, header::RANGE};
use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use super::utils::load_playlist_with_retry;

const KEY_RECOVERY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SegmentIdentity {
    url: Url,
    byte_range: Option<(u64, Option<u64>)>,
    media_sequence: u64,
    part_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HlsKeyIdentity {
    method: String,
    uri: Option<Url>,
    iv: Option<String>,
    keyformat: String,
    keyformatversions: Option<String>,
    /// AES keys without an explicit IV derive it from the media sequence.
    media_sequence: Option<u64>,
    manual_key: Option<String>,
}

/// Core part to perform network operations
pub struct HlsMediaPlaylistSource {
    /// URL of the media playlist
    url: String,

    /// Override key
    key: Option<String>,

    /// Stream ID
    stream_id: u64,
    /// Override stream type
    stream_type: Option<StreamType>,

    /// Sequence number for segments retrived from the playlist
    sequence: AtomicU64,

    initial_playlist: Option<MediaPlaylist>,
    previous_playlist_segments: HashSet<SegmentIdentity>,
    cached_key: Option<(HlsKeyIdentity, Option<Arc<IoriKey>>)>,
}

/// A source to fetch segments from a Media Playlist
///
/// > A Playlist is a Media Playlist if all URI lines in the Playlist
/// > identify Media Segments.
/// >
/// > [RFC8216 Section 4.1](https://datatracker.ietf.org/doc/html/rfc8216#section-4.1)
///
/// The behavior of trying use [HlsPlaylistSource] to load a master playlist is undefined.
/// In current implementation, it will try to load the media playlist of the best quality.
/// But this may change in the future.
impl HlsMediaPlaylistSource {
    pub fn new(
        m3u8_url: String,
        initial_playlist: Option<MediaPlaylist>,
        key: Option<&str>,
        stream_type: Option<StreamType>,
        stream_id: u64,
    ) -> Self {
        Self {
            url: m3u8_url,
            initial_playlist,
            key: key.map(str::to_string),

            sequence: AtomicU64::new(0),
            stream_type,
            stream_id,
            previous_playlist_segments: HashSet::new(),
            cached_key: None,
        }
    }

    fn update_url(&mut self, url: Url) {
        self.url = url.to_string();
        // The cached playlist belongs to the old URL. Keep the sequence and
        // previous-window state so a rotated URL can continue the same stream.
        self.initial_playlist = None;
    }

    fn segment_identity(
        playlist_url: &Url,
        media_sequence: u64,
        segment: &iori_hls::MediaSegment,
    ) -> IoriResult<SegmentIdentity> {
        Ok(SegmentIdentity {
            url: playlist_url.join(&segment.uri)?,
            byte_range: segment
                .byte_range
                .as_ref()
                .map(|range| (range.length, range.offset)),
            media_sequence,
            part_index: segment.part_index,
        })
    }

    fn key_identity(
        key: &iori_hls::Key,
        playlist_url: &Url,
        media_sequence: u64,
        manual_key: Option<&str>,
    ) -> IoriResult<HlsKeyIdentity> {
        Ok(HlsKeyIdentity {
            method: format!("{:?}", key.method),
            uri: key
                .uri
                .as_deref()
                .map(|uri| playlist_url.join(uri))
                .transpose()?,
            iv: key.iv.clone(),
            keyformat: format!("{:?}", key.key_format),
            keyformatversions: key.key_format_versions.clone(),
            media_sequence: key.iv.is_none().then_some(media_sequence),
            manual_key: manual_key.map(str::to_string),
        })
    }

    async fn load_key_with_retry(
        client: &Client,
        key: &iori_hls::Key,
        playlist_url: &Url,
        media_sequence: u64,
        manual_key: Option<String>,
        retries: u32,
    ) -> IoriResult<Option<Arc<IoriKey>>> {
        let attempts = retries.max(1);
        for attempt in 1..=attempts {
            match IoriKey::from_key(
                client,
                key,
                playlist_url,
                media_sequence,
                manual_key.clone(),
            )
            .await
            {
                Ok(key) => return Ok(key.map(Arc::new)),
                Err(error) if attempt < attempts && error.is_transient_network_error() => {
                    tracing::warn!(
                        "Failed to load HLS encryption key; retrying in {} ms (attempt {attempt}/{attempts}): {error}",
                        KEY_RECOVERY_DELAY.as_millis()
                    );
                    tokio::time::sleep(KEY_RECOVERY_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }

        unreachable!("HLS key retry loop must return from an attempt")
    }

    pub async fn load_segments(
        &mut self,
        context: &IoriContext,
        latest_media_sequence: &Option<u64>,
    ) -> IoriResult<(Vec<M3u8Segment>, Url, MediaPlaylist)> {
        let (playlist_url, playlist) = if let Some(initial_playlist) = self.initial_playlist.take()
        {
            (Url::from_str(&self.url)?, initial_playlist)
        } else {
            load_m3u8(
                &context.client,
                Url::from_str(&self.url)?,
                context.manifest_retries,
            )
            .await?
        };

        let current_playlist_segments = playlist
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                Self::segment_identity(
                    &playlist_url,
                    playlist.media_sequence + index as u64,
                    segment,
                )
            })
            .collect::<IoriResult<HashSet<_>>>()?;
        let playlist_overlaps_previous = current_playlist_segments
            .iter()
            .any(|segment| self.previous_playlist_segments.contains(segment));

        let playlist_last_media_sequence = playlist
            .segments
            .len()
            .checked_sub(1)
            .map(|last_index| playlist.media_sequence + last_index as u64);
        let effective_latest_media_sequence = match (
            latest_media_sequence,
            playlist_last_media_sequence,
        ) {
            (Some(latest), Some(last)) if last < *latest => {
                if playlist_overlaps_previous {
                    tracing::warn!(
                        "Live playlist media sequence regressed from {latest} to {last}, but the playlist overlaps the previous window; treating it as a stale playlist."
                    );
                    Some(*latest)
                } else {
                    tracing::warn!(
                        "Live playlist media sequence regressed from {latest} to {last} without overlapping the previous window; treating this as a restarted stream."
                    );
                    None
                }
            }
            _ => *latest_media_sequence,
        };

        let mut key = None;
        let mut initial_segment = InitialSegment::None;
        let mut next_range_start = 0;
        let mut segments = Vec::with_capacity(playlist.segments.len());
        for (i, segment) in playlist.segments.iter().enumerate() {
            if let Some(k) = &segment.key {
                let manual_key = self.key.as_deref();
                let key_identity =
                    Self::key_identity(k, &playlist_url, playlist.media_sequence, manual_key)?;
                let cache_hit = self
                    .cached_key
                    .as_ref()
                    .is_some_and(|(identity, _)| identity == &key_identity);

                if cache_hit {
                    key = self
                        .cached_key
                        .as_ref()
                        .and_then(|(_, value)| value.clone());
                } else {
                    let loaded_key = Self::load_key_with_retry(
                        &context.client,
                        k,
                        &playlist_url,
                        playlist.media_sequence,
                        self.key.clone(),
                        context.segment_retries,
                    )
                    .await?;
                    self.cached_key = Some((key_identity, loaded_key.clone()));
                    key = loaded_key;
                }
            }

            if let Some(m) = &segment.map {
                let url = playlist_url.join(&m.uri)?;

                let mut retries = context.segment_retries;
                loop {
                    retries -= 1;

                    match self
                        .load_bytes(&context.client, url.clone(), m.byte_range.as_ref())
                        .await
                    {
                        Ok(bytes) => {
                            initial_segment = if m.encrypted {
                                InitialSegment::Encrypted(Arc::new(bytes))
                            } else {
                                InitialSegment::Clear(Arc::new(bytes))
                            };
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load bytes for initial segment {url}: {e}");
                            if retries == 0 {
                                return Err(e);
                            }
                        }
                    }
                }
            }

            let url = playlist_url.join(&segment.uri)?;
            // FIXME: filename may be too long
            let filename = url
                .path_segments()
                .and_then(|mut c| c.next_back())
                .map(|r| r.to_string())
                .unwrap_or_else(|| {
                    // 1. hash of file url
                    let mut hasher = std::hash::DefaultHasher::new();
                    url.hash(&mut hasher);
                    let value = hasher.finish();
                    let mut filename = format!("{value:016x}");

                    // 2. byte range
                    if let Some(byte_range) = &segment.byte_range {
                        filename.push_str(&format!("_{}", byte_range.length));
                        if let Some(offset) = byte_range.offset {
                            filename.push_str(&format!("_{}", offset));
                        }
                    }

                    filename
                });
            let format = SegmentFormat::from_filename(&filename);

            let media_sequence = playlist.media_sequence + i as u64;
            if let Some(latest_media_sequence) = effective_latest_media_sequence
                && media_sequence <= latest_media_sequence
            {
                continue;
            }

            let m3u8_segment = M3u8Segment {
                stream_id: self.stream_id,
                url,
                filename,
                key: key.clone(),
                initial_segment: initial_segment.clone(),
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
                media_sequence,
                part_index: segment.part_index,
                byte_range: segment.byte_range.as_ref().map(|r| crate::ByteRange {
                    offset: r.offset.unwrap_or(next_range_start),
                    length: Some(r.length),
                }),
                duration: *segment.duration,
                stream_type: self.stream_type,
                format,
            };
            segments.push(m3u8_segment);

            // [0-100)    -> 100@0  -> next_range_start  = 0 + 100 = 100
            // [100-120)  -> 20     -> next_range_start += 100 + 20 = 200
            if let Some(byte_range) = &segment.byte_range {
                if let Some(offset) = byte_range.offset {
                    next_range_start = offset + byte_range.length;
                } else {
                    next_range_start += byte_range.length;
                }
            }
        }

        if !current_playlist_segments.is_empty() {
            self.previous_playlist_segments = current_playlist_segments;
        }

        Ok((segments, playlist_url, playlist))
    }

    async fn load_bytes(
        &self,
        client: &Client,
        url: Url,
        byte_range: Option<&iori_hls::ByteRange>,
    ) -> IoriResult<Vec<u8>> {
        let mut request = client.get(url);
        if let Some(byte_range) = byte_range {
            let offset = byte_range.offset.unwrap_or(0);
            let end = offset + byte_range.length - 1;
            request = request.header(RANGE, format!("bytes={offset}-{end}"));
        }

        Ok(request
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec())
    }
}

/// A source to fetch segments from a Master Playlist OR a Media Playlist
///
/// > A Playlist is a Master Playlist if all URI lines in the Playlist identify Media Playlists.
/// >
/// > [RFC8216 Section 4.1](https://datatracker.ietf.org/doc/html/rfc8216#section-4.1)
///
/// It is recommended to always use [HlsPlaylistSource].
pub struct HlsPlaylistSource {
    url: Url,

    streams: Vec<HlsMediaPlaylistSource>,

    key: Option<String>,
    playlist_is_master: Option<bool>,
}

impl HlsPlaylistSource {
    pub fn new(url: Url, key: Option<&str>) -> Self {
        Self {
            url,
            key: key.map(str::to_string),
            streams: Vec::new(),
            playlist_is_master: None,
        }
    }

    fn selected_master_streams(
        url: &Url,
        mut playlist: iori_hls::MasterPlaylist,
    ) -> IoriResult<Vec<(String, Option<StreamType>, u64)>> {
        // Get the best variant.
        playlist.variants.sort_by(|a, b| {
            // compare resolution first
            if let (Some(a), Some(b)) = (a.resolution, b.resolution)
                && a.width != b.width
            {
                return b.width.cmp(&a.width);
            }

            // compare framerate then
            if let (Some(a), Some(b)) = (a.frame_rate, b.frame_rate) {
                let a = *a as u64;
                let b = *b as u64;
                if a != b {
                    return b.cmp(&a);
                }
            }

            // compare bandwidth finally
            b.bandwidth.cmp(&a.bandwidth)
        });
        let variant = playlist.variants.first().expect("No variant found");
        let variant_url = url.join(&variant.uri)?.to_string();
        let mut streams = vec![(variant_url, Some(StreamType::Video), 0)];

        fn load_variant<'a>(
            group_id: &str,
            media_type: AlternativeMediaType,
            alternatives: &'a [AlternativeMedia],
        ) -> Option<&'a str> {
            let alternatives: Vec<_> = alternatives
                .iter()
                .filter(|alternative| {
                    alternative.group_id == group_id && alternative.media_type == media_type
                })
                .collect();

            let best = alternatives
                .iter()
                .find(|alternative| alternative.default && alternative.autoselect)
                .or_else(|| alternatives.first());

            best.and_then(|alternative| alternative.uri.as_deref())
        }

        // Load extra streams from the selected variant.
        if let Some(group_id) = &variant.audio
            && let Some(audio_url) = load_variant(
                group_id,
                AlternativeMediaType::Audio,
                &playlist.alternatives,
            )
        {
            let m3u8_url = url.join(audio_url)?.to_string();
            if !streams
                .iter()
                .any(|(stream_url, _, _)| stream_url == &m3u8_url)
            {
                streams.push((m3u8_url, Some(StreamType::Audio), 1));
            }
        }
        if let Some(group_id) = &variant.video
            && let Some(video_url) = load_variant(
                group_id,
                AlternativeMediaType::Video,
                &playlist.alternatives,
            )
        {
            let m3u8_url = url.join(video_url)?.to_string();
            if !streams
                .iter()
                .any(|(stream_url, _, _)| stream_url == &m3u8_url)
            {
                streams.push((m3u8_url, Some(StreamType::Video), 2));
            }
        }

        Ok(streams)
    }

    /// Replace the playlist URL while preserving stream sequence state.
    ///
    /// When the current URL is a master playlist, fetch the replacement master
    /// and update its selected variant URLs before switching. This keeps the
    /// existing media stream state instead of restarting the downloader.
    pub async fn update_url(&mut self, context: &IoriContext, url: Url) -> IoriResult<bool> {
        if self.url == url {
            return Ok(false);
        }
        if self.streams.is_empty() {
            self.url = url;
            return Ok(true);
        }

        let playlist =
            load_playlist_with_retry(&context.client, &url, context.manifest_retries).await?;
        let stream_urls = match playlist {
            Playlist::MasterPlaylist(playlist) if self.playlist_is_master == Some(true) => {
                Self::selected_master_streams(&url, playlist)?
            }
            Playlist::MediaPlaylist(_) if self.playlist_is_master == Some(false) => {
                if self.streams.len() != 1 {
                    return Ok(false);
                }
                vec![(url.to_string(), Some(StreamType::Video), 0)]
            }
            _ => return Ok(false),
        };

        if stream_urls.len() != self.streams.len()
            || self
                .streams
                .iter()
                .zip(&stream_urls)
                .any(|(stream, (_, stream_type, stream_id))| {
                    stream.stream_type != *stream_type || stream.stream_id != *stream_id
                })
        {
            return Ok(false);
        }

        for (stream, (stream_url, _, _)) in self.streams.iter_mut().zip(stream_urls) {
            stream.update_url(Url::parse(&stream_url)?);
        }
        self.url = url;
        Ok(true)
    }

    pub async fn load_streams(&mut self, context: &IoriContext) -> IoriResult<Vec<Option<u64>>> {
        let playlist =
            load_playlist_with_retry(&context.client, &self.url, context.manifest_retries).await?;

        match playlist {
            Playlist::MasterPlaylist(pl) => {
                self.playlist_is_master = Some(true);
                for (m3u8_url, stream_type, stream_id) in
                    Self::selected_master_streams(&self.url, pl)?
                {
                    if !self.streams.iter().any(|stream| stream.url == m3u8_url) {
                        self.streams.push(HlsMediaPlaylistSource::new(
                            m3u8_url,
                            None,
                            self.key.as_deref(),
                            stream_type,
                            stream_id,
                        ));
                    }
                }
            }
            Playlist::MediaPlaylist(pl) => {
                self.playlist_is_master = Some(false);
                self.streams.push(HlsMediaPlaylistSource::new(
                    self.url.to_string(),
                    Some(pl),
                    self.key.as_deref(),
                    Some(StreamType::Video),
                    0,
                ));
            }
        }
        Ok(vec![None; self.streams.len()])
    }

    pub async fn load_segments(
        &mut self,
        context: &IoriContext,
        latest_media_sequences: &[Option<u64>],
    ) -> IoriResult<(Vec<Vec<M3u8Segment>>, bool /* is_end */)> {
        let mut segments = Vec::new();
        let mut is_end = true;
        for (stream, latest_media_sequence) in self.streams.iter_mut().zip(latest_media_sequences) {
            let (stream_segments, _, stream_playlist) =
                stream.load_segments(context, latest_media_sequence).await?;
            segments.push(stream_segments);
            if !stream_playlist.end_list {
                is_end = false;
            }
        }

        Ok((segments, is_end))
    }

    /// Reset the internal sequence counters for each stream.
    ///
    /// This is used after truncating the initial segment list so that
    /// subsequent fetches produce sequence numbers that continue from
    /// where the truncated list left off, keeping `OrderedStream` happy.
    pub fn reset_stream_sequences(&self, values: &[u64]) {
        for (stream, &val) in self.streams.iter().zip(values.iter()) {
            stream.sequence.store(val, Ordering::Relaxed);
        }
    }
}
