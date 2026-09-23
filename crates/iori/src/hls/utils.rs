use crate::error::{IoriError, IoriResult};
use iori_hls::{MediaPlaylist, Playlist};
use reqwest::Client;
use reqwest::Url;
use std::time::Duration;

const ACCESS_DENIED_RETRY_DELAY: Duration = Duration::from_secs(1);

fn is_access_denied_playlist_response(status: reqwest::StatusCode, body: &[u8]) -> bool {
    status == reqwest::StatusCode::FORBIDDEN
        || body
            .windows(b"AccessDenied".len())
            .any(|window| window == b"AccessDenied")
}

async fn fetch_playlist(client: &Client, url: &Url, total_retry: u32) -> IoriResult<Playlist> {
    let mut retries = total_retry;
    loop {
        if retries == 0 {
            return Err(IoriError::ManifestFetchError);
        }

        match client.get(url.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                match response.bytes().await {
                    Ok(body) if !status.is_success() => {
                        tracing::warn!("HLS manifest request returned HTTP status {status}.");
                        if is_access_denied_playlist_response(status, &body) {
                            tracing::warn!(
                                "HLS manifest access was denied; retrying after {} ms.",
                                ACCESS_DENIED_RETRY_DELAY.as_millis()
                            );
                            tokio::time::sleep(ACCESS_DENIED_RETRY_DELAY).await;
                        }
                        retries -= 1;
                    }
                    Ok(body) => match iori_hls::parse_playlist_res(&body) {
                        Ok(parsed) => return Ok(parsed),
                        Err(_) => {
                            tracing::warn!("Failed to parse HLS manifest response.");
                            retries -= 1;
                        }
                    },
                    Err(_) => {
                        tracing::warn!("Failed to read HLS manifest response.");
                        retries -= 1;
                    }
                }
            }
            Err(_) => {
                tracing::warn!("Failed to request HLS manifest.");
                retries -= 1;
            }
        }
    }
}

pub async fn load_playlist_with_retry(
    client: &Client,
    url: &Url,
    total_retry: u32,
) -> IoriResult<Playlist> {
    fetch_playlist(client, url, total_retry).await
}

#[async_recursion::async_recursion]
pub async fn load_m3u8(
    client: &Client,
    url: Url,
    total_retry: u32,
) -> IoriResult<(Url, MediaPlaylist)> {
    tracing::debug!("Start fetching HLS manifest.");

    let parsed = fetch_playlist(client, &url, total_retry).await?;
    tracing::debug!("HLS manifest fetched.");

    match parsed {
        Playlist::MasterPlaylist(playlist) => {
            tracing::info!("Master playlist input detected. Auto selecting best quality streams.");
            let mut variants = playlist.variants;
            variants.sort_by(|a, b| {
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
            let variant = variants.first().expect("No variant found");
            let variant_url = url.join(&variant.uri).expect("Invalid variant uri");

            tracing::info!(
                "Selected best HLS stream; bandwidth: {bandwidth}",
                bandwidth = variant.bandwidth
            );
            load_m3u8(client, variant_url, total_retry).await
        }
        Playlist::MediaPlaylist(playlist) => Ok((url, playlist)),
    }
}
