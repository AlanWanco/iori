use aes::cipher::block_padding::UnpadError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IoriError {
    #[error("HTTP error: {0}")]
    HttpError(reqwest::StatusCode),

    #[error("Manifest fetch error")]
    ManifestFetchError,

    #[error("Decryption key required")]
    DecryptionKeyRequired,

    #[error("Invalid hex key: {0}")]
    InvalidHexKey(String),

    #[error("Invalid binary key: {0:?}")]
    InvalidBinaryKey(Vec<u8>),

    #[error("mp4decrypt error: {0}")]
    Mp4DecryptError(#[from] mp4decrypt::Error),

    #[error("iori-ssa error: {0:?}")]
    IoriSsaError(#[from] iori_ssa::Error),

    #[error("Pkcs7 unpad error")]
    UnpadError(#[from] UnpadError),

    #[error("Invalid m3u8 file: {0}")]
    M3u8ParseError(#[from] iori_hls::M3u8ParseError),

    #[error(transparent)]
    IOError(#[from] std::io::Error),

    #[error(transparent)]
    UrlParseError(#[from] url::ParseError),

    #[error(transparent)]
    HexDecodeError(#[from] hex::FromHexError),

    // Keep request URLs out of logs because HLS URLs may contain signed credentials.
    #[error("network request failed")]
    RequestError(Box<reqwest::Error>),

    // MPEG-DASH errors
    #[error(transparent)]
    MpdParseError(Box<dash_mpd::DashMpdError>),

    #[error("invalid mpd: {0}")]
    MpdParsing(String),

    #[error(transparent)]
    TimeDeltaOutOfRange(#[from] chrono::OutOfRangeError),

    #[error("Invalid timing schema: {0:?}")]
    InvalidTimingSchema(String),

    #[error(transparent)]
    MissingExecutable(#[from] which::Error),

    #[error("Can not set cache directory to an existing path: {0}")]
    CacheDirExists(std::path::PathBuf),

    #[error(transparent)]
    JsonError(#[from] serde_json::Error),

    #[cfg(feature = "opendal")]
    #[error(transparent)]
    OpendalError(Box<opendal::Error>),

    #[error("No period found")]
    NoPeriodFound,

    #[error("No adaption set found")]
    NoAdaptationSetFound,

    #[error("No representation found")]
    NoRepresentationFound,

    #[error(transparent)]
    ChronoParseError(#[from] chrono::ParseError),

    #[error("Invalid date time: {0}")]
    DateTimeParsing(String),

    #[error("{0}")]
    Custom(#[from] Box<dyn std::error::Error + Send + Sync>),
}

#[cfg(feature = "opendal")]
impl From<opendal::Error> for IoriError {
    fn from(err: opendal::Error) -> Self {
        IoriError::OpendalError(Box::new(err))
    }
}

impl From<dash_mpd::DashMpdError> for IoriError {
    fn from(err: dash_mpd::DashMpdError) -> Self {
        IoriError::MpdParseError(Box::new(err))
    }
}

impl From<reqwest::Error> for IoriError {
    fn from(err: reqwest::Error) -> Self {
        IoriError::RequestError(Box::new(err))
    }
}

impl IoriError {
    pub fn is_transient_network_error(&self) -> bool {
        match self {
            Self::RequestError(error) => {
                error.is_timeout() || error.is_connect() || error.is_body() || error.is_decode()
            }
            Self::HttpError(status) => status.is_server_error(),
            Self::IOError(error) => matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
            ),
            _ => false,
        }
    }
}

pub type IoriResult<T> = Result<T, IoriError>;
