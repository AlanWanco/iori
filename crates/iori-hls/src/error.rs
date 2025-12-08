#[derive(Debug, thiserror::Error)]
pub enum M3u8ParseError {
    #[error("failed to read playlist line: {message}")]
    Reader { message: String },
    #[error("invalid playlist: {0}")]
    InvalidPlaylist(String),
}

impl<'a> From<quick_m3u8::error::ReaderBytesError<'a>> for M3u8ParseError {
    fn from(value: quick_m3u8::error::ReaderBytesError<'a>) -> Self {
        let line = String::from_utf8_lossy(value.errored_line).into_owned();
        Self::Reader {
            message: format!("{}, line: {line}", value.error),
        }
    }
}
