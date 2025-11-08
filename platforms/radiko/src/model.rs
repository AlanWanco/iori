use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationInfo {
    pub id: String,
    pub name: String,
    pub ascii_name: String,
    pub href: Option<String>,
}

// XML response structures for deserialization
#[derive(Debug, Deserialize)]
pub struct StationRegionResponse {
    #[serde(rename = "stations", default)]
    pub regions: Vec<StationRegion>,
}

#[derive(Debug, Deserialize)]
pub struct StationRegion {
    #[serde(rename = "station", default)]
    pub stations: Vec<StationRegionItem>,
}

#[derive(Debug, Deserialize)]
pub struct StationRegionItem {
    pub id: String,
    pub area_id: String,
}

#[derive(Debug, Deserialize)]
pub struct StationListResponse {
    #[serde(rename = "station", default)]
    pub stations: Vec<StationListItem>,
}

#[derive(Debug, Deserialize)]
pub struct StationListItem {
    pub id: String,
    pub name: String,
    pub ascii_name: String,
    #[serde(default)]
    pub href: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamResponse {
    #[serde(rename = "url", default)]
    pub urls: Vec<StreamUrlItem>,
}

#[derive(Debug, Deserialize)]
pub struct StreamUrlItem {
    #[serde(rename = "@timefree")]
    pub timefree: String,
    #[serde(rename = "@areafree")]
    pub areafree: String,
    pub playlist_create_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUrl {
    pub url: Url,
    pub timefree: bool,
}

#[derive(Debug, Clone)]
pub struct AuthData {
    pub auth_token: String,
    pub area_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgrammeInfo {
    pub station_id: String,
    pub title: String,
    pub start_time: String,
    pub end_time: String,
    pub duration: u64,
    pub ft: String,
    pub to: String,
    pub performer: Option<String>,
    pub description: Option<String>,
    pub img: Option<String>,
}

// JSON response structures for programme data
#[derive(Debug, Deserialize)]
pub struct ProgrammeResponse {
    pub stations: Vec<ProgrammeStation>,
}

#[derive(Debug, Deserialize)]
pub struct ProgrammeStation {
    pub programs: ProgrammePrograms,
}

#[derive(Debug, Deserialize)]
pub struct ProgrammePrograms {
    pub program: Vec<ProgrammeItem>,
}

#[derive(Debug, Deserialize)]
pub struct ProgrammeItem {
    pub title: String,
    pub ft: String,
    pub to: String,
    pub dur: Option<u64>,
    #[serde(default)]
    pub performer: Option<String>,
    #[serde(default)]
    pub info: Option<String>,
    #[serde(default)]
    pub img: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub app: String,
    pub app_version: String,
    pub device: String,
    pub user_id: String,
    pub user_agent: String,
}
