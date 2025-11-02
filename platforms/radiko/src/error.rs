use thiserror::Error;

#[derive(Error, Debug)]
pub enum RadikoError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Region mismatch: expected {expected}, got {actual}")]
    RegionMismatch { expected: String, actual: String },

    #[error("Station not found: {0}")]
    StationNotFound(String),

    #[error("Program not available")]
    ProgramNotAvailable,

    #[error("Program not aired yet")]
    ProgramNotAiredYet,

    #[error("Program no longer available")]
    ProgramExpired,

    #[error("Timefree 30 subscription required")]
    TimeFree30Required,

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error(transparent)]
    QuickXmlError(#[from] quick_xml::DeError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
