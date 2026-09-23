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
    pub fn from_api_value(s: &str) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EplusPlaylistKind {
    Live,
    Archive,
}

/// Result of extracting data from an eplus event page.
#[derive(Debug, Clone)]
pub struct EplusEventData {
    pub app_id: String,
    pub title: String,
    pub delivery_status: DeliveryStatus,
    pub archive_mode: Option<String>,
    pub is_drm: bool,
    pub m3u8_urls: Vec<String>,
    pub stream_session: Option<String>,
    pub session_update_url: Option<String>,
    pub cloudfront_cookies: Vec<String>,
}

impl EplusEventData {
    /// Select only a playlist kind that is valid for the event's current lifecycle state.
    pub fn playlist_kind(&self, prefer_archive: bool) -> Option<EplusPlaylistKind> {
        match &self.delivery_status {
            DeliveryStatus::Started if !prefer_archive => Some(EplusPlaylistKind::Live),
            DeliveryStatus::ConfirmedArchive => Some(EplusPlaylistKind::Archive),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeliveryStatus, EplusEventData, EplusPlaylistKind};

    fn event(delivery_status: DeliveryStatus, archive_mode: Option<&str>) -> EplusEventData {
        EplusEventData {
            app_id: "event".to_string(),
            title: "event".to_string(),
            delivery_status,
            archive_mode: archive_mode.map(str::to_string),
            is_drm: false,
            m3u8_urls: Vec::new(),
            stream_session: None,
            session_update_url: None,
            cloudfront_cookies: Vec::new(),
        }
    }

    #[test]
    fn started_event_selects_live_unless_archive_was_requested() {
        let event = event(DeliveryStatus::Started, Some("ON"));
        assert_eq!(event.playlist_kind(false), Some(EplusPlaylistKind::Live));
        assert_eq!(event.playlist_kind(true), None);
    }

    #[test]
    fn stopped_or_unconfirmed_archive_never_selects_live_playlist() {
        let stopped_with_archive = event(DeliveryStatus::Stopped, Some("ON"));
        let stopped_without_archive = event(DeliveryStatus::Stopped, Some("OFF"));
        let pending_confirmation = event(DeliveryStatus::WaitConfirmArchived, Some("ON"));

        assert_eq!(stopped_with_archive.playlist_kind(false), None);
        assert_eq!(stopped_without_archive.playlist_kind(false), None);
        assert_eq!(pending_confirmation.playlist_kind(false), None);
    }

    #[test]
    fn confirmed_archive_selects_archive_playlist() {
        let event = event(DeliveryStatus::ConfirmedArchive, Some("ON"));
        assert_eq!(event.playlist_kind(false), Some(EplusPlaylistKind::Archive));
    }
}

/// Pre-login API response from eplus.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FtAuthResponse {
    pub is_success: bool,
}
