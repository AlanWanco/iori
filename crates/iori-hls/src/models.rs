use quick_m3u8::tag::{DecimalResolution, hls};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    MasterPlaylist(MasterPlaylist),
    MediaPlaylist(MediaPlaylist),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MasterPlaylist {
    pub variants: Vec<VariantStream>,
    pub alternatives: Vec<AlternativeMedia>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VariantStream {
    pub uri: String,
    pub bandwidth: u64,
    pub average_bandwidth: Option<u64>,
    pub resolution: Option<Resolution>,
    pub frame_rate: Option<f64>,
    pub audio: Option<String>,
    pub video: Option<String>,
}

impl<'a> From<(hls::StreamInf<'a>, Cow<'a, str>)> for VariantStream {
    fn from((value, uri): (hls::StreamInf<'a>, Cow<'a, str>)) -> Self {
        Self {
            uri: uri.to_string(),
            bandwidth: value.bandwidth(),
            average_bandwidth: value.average_bandwidth(),
            resolution: value.resolution().map(Resolution::from),
            frame_rate: value.frame_rate(),
            audio: value.audio().map(str::to_string),
            video: value.video().map(str::to_string),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub width: u64,
    pub height: u64,
}

impl From<DecimalResolution> for Resolution {
    fn from(value: DecimalResolution) -> Self {
        Self {
            width: value.width,
            height: value.height,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlternativeMedia {
    pub group_id: String,
    pub media_type: AlternativeMediaType,
    pub name: String,
    pub uri: Option<String>,
    pub default: bool,
    pub autoselect: bool,
}

impl<'a> From<hls::Media<'a>> for AlternativeMedia {
    fn from(value: hls::Media<'a>) -> Self {
        Self {
            group_id: value.group_id().to_string(),
            media_type: match value.media_type() {
                hls::EnumeratedString::Known(value) => value.into(),
                hls::EnumeratedString::Unknown(value) => {
                    AlternativeMediaType::Other(value.to_string())
                }
            },
            name: value.name().to_string(),
            uri: value.uri().map(str::to_string),
            default: value.default(),
            autoselect: value.autoselect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlternativeMediaType {
    Audio,
    Video,
    Subtitles,
    ClosedCaptions,
    Other(String),
}

impl From<hls::MediaType> for AlternativeMediaType {
    fn from(value: hls::MediaType) -> Self {
        match value {
            hls::MediaType::Audio => Self::Audio,
            hls::MediaType::Video => Self::Video,
            hls::MediaType::Subtitles => Self::Subtitles,
            hls::MediaType::ClosedCaptions => Self::ClosedCaptions,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    pub media_sequence: u64,
    pub segments: Vec<MediaSegment>,
    pub end_list: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSegment {
    pub uri: String,
    pub duration: f64,
    pub title: Option<String>,
    pub byte_range: Option<ByteRange>,
    pub key: Option<Key>,
    pub map: Option<Map>,
}

impl<'a>
    From<(
        hls::Inf<'a>,
        Cow<'a, str>,
        Option<ByteRange>,
        Option<Key>,
        Option<Map>,
    )> for MediaSegment
{
    fn from(
        (inf, uri, byte_range, key, map): (
            hls::Inf<'a>,
            Cow<'a, str>,
            Option<ByteRange>,
            Option<Key>,
            Option<Map>,
        ),
    ) -> Self {
        Self {
            uri: uri.to_string(),
            duration: inf.duration(),
            title: match inf.title() {
                "" => None,
                title => Some(title.to_string()),
            },
            byte_range,
            key,
            map,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRange {
    pub length: u64,
    pub offset: Option<u64>,
}

impl<'a> From<quick_m3u8::tag::hls::Byterange<'a>> for ByteRange {
    fn from(value: quick_m3u8::tag::hls::Byterange<'a>) -> Self {
        Self {
            length: value.length(),
            offset: value.offset(),
        }
    }
}

impl From<quick_m3u8::tag::hls::MapByterange> for ByteRange {
    fn from(value: quick_m3u8::tag::hls::MapByterange) -> Self {
        Self {
            length: value.length,
            offset: Some(value.offset),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub uri: String,
    pub byte_range: Option<ByteRange>,
    pub encrypted: bool,
}

impl<'a> From<(hls::Map<'a>, &Option<Key>)> for Map {
    fn from((value, key): (hls::Map<'a>, &Option<Key>)) -> Self {
        Self {
            uri: value.uri().to_string(),
            byte_range: value.byterange().map(ByteRange::from),
            encrypted: key.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub method: KeyMethod,
    pub uri: Option<String>,
    pub iv: Option<String>,
    pub key_format: Option<String>,
    pub key_format_versions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMethod {
    None,
    AES128,
    SampleAES,
    SampleAESCTR,
    SampleAesCenc,
    Other(String),
}

impl<'a> From<quick_m3u8::tag::hls::Key<'a>> for Key {
    fn from(value: quick_m3u8::tag::hls::Key) -> Self {
        let method = match value.method() {
            hls::EnumeratedString::Known(hls::Method::None) => KeyMethod::None,
            hls::EnumeratedString::Known(hls::Method::Aes128) => KeyMethod::AES128,
            hls::EnumeratedString::Known(hls::Method::SampleAes) => KeyMethod::SampleAES,
            hls::EnumeratedString::Known(hls::Method::SampleAesCtr) => KeyMethod::SampleAESCTR,
            hls::EnumeratedString::Unknown("SAMPLE-AES-CENC") => KeyMethod::SampleAesCenc,
            hls::EnumeratedString::Unknown(value) => KeyMethod::Other(value.to_string()),
        };

        Self {
            method,
            uri: value.uri().map(str::to_string),
            iv: value.iv().map(str::to_string),
            key_format: Some(value.keyformat().to_string()),
            key_format_versions: value.keyformatversions().map(str::to_string),
        }
    }
}

impl std::fmt::Display for KeyMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyMethod::None => write!(f, "NONE"),
            KeyMethod::AES128 => write!(f, "AES-128"),
            KeyMethod::SampleAES => write!(f, "SAMPLE-AES"),
            KeyMethod::SampleAESCTR => write!(f, "SAMPLE-AES-CTR"),
            KeyMethod::SampleAesCenc => write!(f, "SAMPLE-AES-CENC"),
            KeyMethod::Other(name) => write!(f, "{name}"),
        }
    }
}
