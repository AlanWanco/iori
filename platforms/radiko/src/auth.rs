use crate::{
    constants::{ANDROID_VERSIONS, APP_VERSIONS, COORDINATES, FULL_KEY, MODELS},
    error::RadikoError,
    model::{AuthData, DeviceInfo},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rand::prelude::*;
use rand::seq::SliceRandom;
use reqwest::{Client, header::HeaderMap};

/// Generate random device information for authentication
pub fn generate_device_info() -> DeviceInfo {
    let mut rng = rand::thread_rng();

    let version_info = ANDROID_VERSIONS.choose(&mut rng).unwrap();
    let android_version = version_info.version;
    let sdk = version_info.sdk;
    let build = version_info.builds.choose(&mut rng).unwrap();
    let model = MODELS.choose(&mut rng).unwrap();
    let app_version = APP_VERSIONS.choose(&mut rng).unwrap();

    // Generate a random user ID (32 hex characters)
    let user_id: String = (0..32)
        .map(|_| "0123456789abcdef".chars().choose(&mut rng).unwrap())
        .collect();

    DeviceInfo {
        app: "aSmartPhone7a".to_string(),
        app_version: app_version.to_string(),
        device: format!("{}.{}", sdk, model),
        user_id,
        user_agent: format!(
            "Dalvik/2.1.0 (Linux; U; Android {};{}/{})",
            android_version, model, build
        ),
    }
}

/// Get coordinates for a region with random offset
pub fn get_coords(region: &str) -> String {
    let _rng = rand::thread_rng();

    // Extract region number (e.g., "JP13" -> 13)
    let region_num: usize = region
        .strip_prefix("JP")
        .and_then(|s| s.parse().ok())
        .unwrap_or(13) // Default to Tokyo if parsing fails
        - 1; // Convert to 0-based index

    let (lat, long) = COORDINATES
        .get(region_num)
        .unwrap_or(&(35.689488, 139.691706));

    // Add random offset: +/- 0 ~ 0.025 --> 0 ~ 1.5' -> +/- 0 ~ 2.77/2.13km
    let lat_offset = (rand::random::<f64>() / 40.0) * if rand::random() { 1.0 } else { -1.0 };
    let long_offset = (rand::random::<f64>() / 40.0) * if rand::random() { 1.0 } else { -1.0 };

    let lat = lat + lat_offset;
    let long = long + long_offset;

    format!("{:.6},{:.6},gps", lat, long)
}

/// Perform authentication step 1
pub async fn auth_step1(
    client: &Client,
    device: &DeviceInfo,
) -> Result<(String, i64, i64), RadikoError> {
    let response = client
        .get("https://radiko.jp/v2/api/auth1")
        .header("X-Radiko-App", &device.app)
        .header("X-Radiko-App-Version", &device.app_version)
        .header("X-Radiko-Device", &device.device)
        .header("X-Radiko-User", &device.user_id)
        .header("User-Agent", &device.user_agent)
        .send()
        .await?;

    let headers = response.headers();

    let auth_token = headers
        .get("X-Radiko-AuthToken")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| RadikoError::AuthFailed("Missing X-Radiko-AuthToken".to_string()))?
        .to_string();

    let key_length: i64 = headers
        .get("X-Radiko-KeyLength")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| RadikoError::AuthFailed("Missing X-Radiko-KeyLength".to_string()))?;

    let key_offset: i64 = headers
        .get("X-Radiko-KeyOffset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| RadikoError::AuthFailed("Missing X-Radiko-KeyOffset".to_string()))?;

    Ok((auth_token, key_length, key_offset))
}

/// Perform authentication step 2
pub async fn auth_step2(
    client: &Client,
    device: &DeviceInfo,
    auth_token: &str,
    key_length: i64,
    key_offset: i64,
    region: &str,
) -> Result<AuthData, RadikoError> {
    // Extract partial key and encode it
    let start = key_offset as usize;
    let end = (key_offset + key_length) as usize;
    let partial_key = &FULL_KEY[start..end];
    let partial_key_b64 = BASE64.encode(partial_key);

    let coords = get_coords(region);
    let connection = if rand::random() { "wifi" } else { "mobile" };

    let response = client
        .get("https://radiko.jp/v2/api/auth2")
        .header("X-Radiko-App", &device.app)
        .header("X-Radiko-App-Version", &device.app_version)
        .header("X-Radiko-Device", &device.device)
        .header("X-Radiko-User", &device.user_id)
        .header("User-Agent", &device.user_agent)
        .header("X-Radiko-AuthToken", auth_token)
        .header("X-Radiko-Location", &coords)
        .header("X-Radiko-Connection", connection)
        .header("X-Radiko-Partialkey", &partial_key_b64)
        .send()
        .await?;

    let body = response.text().await?;
    let parts: Vec<&str> = body.trim().split(',').collect();

    if parts.len() < 3 {
        return Err(RadikoError::AuthFailed(format!(
            "Invalid auth2 response: {}",
            body
        )));
    }

    let actual_region = parts[0];

    if actual_region != region {
        return Err(RadikoError::RegionMismatch {
            expected: region.to_string(),
            actual: actual_region.to_string(),
        });
    }

    Ok(AuthData {
        auth_token: auth_token.to_string(),
        area_id: actual_region.to_string(),
        user_id: device.user_id.clone(),
    })
}

/// Build authentication headers from AuthData
pub fn build_auth_headers(auth_data: &AuthData) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("X-Radiko-AuthToken", auth_data.auth_token.parse().unwrap());
    headers.insert("X-Radiko-AreaId", auth_data.area_id.parse().unwrap());
    headers
}
