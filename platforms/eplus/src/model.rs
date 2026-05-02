use serde::{Deserialize, Serialize};

/// Data extracted from the `var app = {...};` JavaScript variable on the eplus event page.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EplusAppData {
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(alias = "appName")]
    pub app_name: Option<String>,

    #[serde(default)]
    #[serde(alias = "deliveryStatus")]
    pub delivery_status: Option<String>,
    #[serde(default)]
    #[serde(alias = "archiveMode")]
    pub archive_mode: Option<String>,
    #[serde(default)]
    #[serde(alias = "drmMode")]
    pub drm_mode: Option<String>,
    #[serde(default)]
    #[serde(alias = "isPassTicket")]
    pub is_pass_ticket: Option<String>,
}

/// Delivery status of an eplus event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Preparing,
    Started,
    Stopped,
    WaitConfirmArchived,
    ConfirmedArchive,
    Unknown(String),
}

impl DeliveryStatus {
    pub fn from_str(s: &str) -> Self {
        match s {
            "PREPARING" => Self::Preparing,
            "STARTED" => Self::Started,
            "STOPPED" => Self::Stopped,
            "WAIT_CONFIRM_ARCHIVED" => Self::WaitConfirmArchived,
            "CONFIRMED_ARCHIVE" => Self::ConfirmedArchive,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn is_streamable(&self) -> bool {
        matches!(self, Self::Started | Self::ConfirmedArchive)
    }
}

/// Result of extracting data from an eplus event page.
#[derive(Debug, Clone)]
pub struct EplusEventData {
    pub app_id: String,
    pub title: String,
    pub delivery_status: DeliveryStatus,
    pub is_drm: bool,
    pub m3u8_urls: Vec<String>,
    pub stream_session: Option<String>,
    pub cloudfront_cookies: Vec<(String, String)>,
}

/// Pre-login API response from eplus.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FtAuthResponse {
    pub is_success: bool,
    #[serde(default)]
    pub errors: Option<serde_json::Value>,
}
