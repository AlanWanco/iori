use crate::{
    auth::{auth_step1, auth_step2, build_auth_headers, generate_device_info},
    error::RadikoError,
    model::{
        AuthData, DeviceInfo, ProgrammeInfo, ProgrammeResponse, StationInfo, StationListResponse,
        StationRegionResponse, StreamResponse, StreamUrl,
    },
    time::RadikoTime,
};
use reqwest::Client;
use std::collections::HashMap;
use url::Url;

pub struct RadikoClient {
    client: Client,
    device: DeviceInfo,
    auth_cache: HashMap<String, AuthData>,
}

impl RadikoClient {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent(fake_user_agent::get_chrome_rua())
                .build()
                .unwrap(),
            device: generate_device_info(),
            auth_cache: HashMap::new(),
        }
    }

    /// Authenticate for a specific region
    pub async fn authenticate(&mut self, region: &str) -> Result<AuthData, RadikoError> {
        // Check cache first
        if let Some(auth_data) = self.auth_cache.get(region) {
            // Verify if token is still valid
            let response = self
                .client
                .get("https://radiko.jp/v2/api/auth_check")
                .headers(build_auth_headers(auth_data))
                .send()
                .await?;

            if response.status().is_success() {
                let body = response.text().await?;
                if body.trim() == "OK" {
                    return Ok(auth_data.clone());
                }
            }
        }

        // Perform new authentication
        let (auth_token, key_length, key_offset) = auth_step1(&self.client, &self.device).await?;

        let auth_data = auth_step2(
            &self.client,
            &self.device,
            &auth_token,
            key_length,
            key_offset,
            region,
        )
        .await?;

        // Cache the auth data
        self.auth_cache
            .insert(region.to_string(), auth_data.clone());

        Ok(auth_data)
    }

    /// Get station region by station ID
    pub async fn get_station_region(&self, station_id: &str) -> Result<String, RadikoError> {
        let response = self
            .client
            .get("https://radiko.jp/v3/station/region/full.xml")
            .send()
            .await?;

        let text = response.text().await?;
        let region_response: StationRegionResponse = quick_xml::de::from_str(&text)?;

        for region in region_response.regions {
            for station in region.stations {
                if station.id == station_id {
                    return Ok(station.area_id);
                }
            }
        }

        Err(RadikoError::StationNotFound(station_id.to_string()))
    }

    /// Get station metadata
    pub async fn get_station_info(
        &self,
        region: &str,
        station_id: &str,
    ) -> Result<StationInfo, RadikoError> {
        let url = format!("https://radiko.jp/v3/station/list/{}.xml", region);
        let response = self.client.get(&url).send().await?;
        let text = response.text().await?;

        let station_list: StationListResponse = quick_xml::de::from_str(&text)?;

        for station in station_list.stations {
            if station.id == station_id {
                return Ok(StationInfo {
                    id: station.id,
                    name: station.name,
                    ascii_name: station.ascii_name,
                    href: station.href,
                });
            }
        }

        Err(RadikoError::StationNotFound(station_id.to_string()))
    }

    /// Get live streaming URLs for a station
    pub async fn get_live_stream_urls(
        &self,
        station_id: &str,
        auth_data: &AuthData,
    ) -> Result<Vec<StreamUrl>, RadikoError> {
        let device = "pc_html5";
        let url = format!(
            "https://radiko.jp/v3/station/stream/{}/{}.xml",
            device, station_id
        );

        let response = self.client.get(&url).send().await?;
        let text = response.text().await?;

        let stream_response: StreamResponse = quick_xml::de::from_str(&text)?;

        let mut stream_urls = Vec::new();

        for url_item in stream_response.urls {
            // Filter for live streams (timefree=0, areafree=0)
            if url_item.timefree == "0"
                && url_item.areafree == "0"
                && let Some(base_url) = url_item.playlist_create_url
            {
                // Build the complete URL with query parameters
                let mut url = Url::parse(&base_url)
                    .map_err(|e| RadikoError::ParseError(format!("Failed to parse URL: {}", e)))?;
                url.set_query(Some(&format!(
                    "station_id={}&l=15&lsid={}&type=b",
                    station_id, auth_data.user_id
                )));

                stream_urls.push(StreamUrl {
                    url,
                    timefree: false,
                });
            }
        }

        Ok(stream_urls)
    }

    /// Get timefree streaming URLs for a station
    pub async fn get_timefree_stream_urls(
        &self,
        station_id: &str,
        start_time: &RadikoTime,
        end_time: &RadikoTime,
        auth_data: &AuthData,
    ) -> Result<Vec<StreamUrl>, RadikoError> {
        let device = "pc_html5";
        let url = format!(
            "https://radiko.jp/v3/station/stream/{}/{}.xml",
            device, station_id
        );

        let response = self.client.get(&url).send().await?;
        let text = response.text().await?;

        let stream_response: StreamResponse = quick_xml::de::from_str(&text)?;

        let mut stream_urls = Vec::new();

        for url_item in stream_response.urls {
            // Filter for timefree streams (timefree=1, areafree=0)
            if url_item.timefree == "1"
                && url_item.areafree == "0"
                && let Some(base_url) = url_item.playlist_create_url
            {
                // Build the complete URL with query parameters
                let mut url = Url::parse(&base_url)
                    .map_err(|e| RadikoError::ParseError(format!("Failed to parse URL: {}", e)))?;
                url.set_query(Some(&format!(
                    "station_id={}&l=300&lsid={}&type=b&start_at={}&ft={}&end_at={}&to={}",
                    station_id,
                    auth_data.user_id,
                    start_time.timestring(),
                    start_time.timestring(),
                    end_time.timestring(),
                    end_time.timestring()
                )));

                stream_urls.push(StreamUrl {
                    url,
                    timefree: true,
                });
            }
        }

        Ok(stream_urls)
    }

    /// Get programme metadata for a specific time
    pub async fn get_programme_info(
        &self,
        station_id: &str,
        time: &RadikoTime,
    ) -> Result<ProgrammeInfo, RadikoError> {
        let day = time.broadcast_day_string();
        let url = format!(
            "https://api.radiko.jp/program/v4/date/{}/station/{}.json",
            day, station_id
        );

        let response = self.client.get(&url).send().await?;
        let data: ProgrammeResponse = response.json().await?;

        let timestring = time.timestring();

        // Get the first station's programs
        let programmes = data
            .stations
            .into_iter()
            .next()
            .map(|s| s.programs.program)
            .ok_or_else(|| RadikoError::ParseError("No stations found".to_string()))?;

        for prog in programmes {
            if prog.ft <= timestring && timestring < prog.to {
                return Ok(ProgrammeInfo {
                    station_id: station_id.to_string(),
                    title: prog.title,
                    start_time: prog.ft.clone(),
                    end_time: prog.to.clone(),
                    duration: prog.dur.parse().unwrap_or(0),
                    ft: prog.ft,
                    to: prog.to,
                    performer: prog.performer,
                    description: prog.info,
                    img: prog.img,
                });
            }
        }

        Err(RadikoError::ProgramNotAvailable)
    }
}

impl Default for RadikoClient {
    fn default() -> Self {
        Self::new()
    }
}
