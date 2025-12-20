pub mod constants;
pub mod model;

use anyhow::Context;
use model::*;
use reqwest::{
    Client, ClientBuilder,
    header::{COOKIE, HeaderMap, HeaderValue, SET_COOKIE},
};

#[derive(Clone)]
pub struct ShowRoomClient {
    client: Client,
    sr_id: String,
}

impl PartialEq for ShowRoomClient {
    fn eq(&self, other: &Self) -> bool {
        self.sr_id.eq(&other.sr_id)
    }
}

async fn get_sr_id() -> anyhow::Result<String> {
    let response = reqwest::get("https://www.showroom-live.com/api/live/onlive_num").await?;
    let cookies = response.headers().get_all(SET_COOKIE);
    for cookie in cookies {
        let Some((kv, _)) = cookie.to_str()?.split_once(';') else {
            continue;
        };
        let Some((key, value)) = kv.split_once('=') else {
            continue;
        };
        if key == "sr_id" {
            return Ok(value.to_string());
        }
    }

    // fallback guest id
    Ok("u9ZYLQddhas3AEWr7t2ohQ-zHaVWmkuVg9IGr5IWtTr6-S2U24EA3e4jgg1nSL0Q".to_string())
}

impl ShowRoomClient {
    /// Key saved from: https://hls-archive-aes.live.showroom-live.com/aes.key
    /// Might be useful for timeshift
    pub const ARCHIVE_KEY: &str = "2a63847146f96dd3a17077f6c72daffb";

    pub async fn new(mut builder: ClientBuilder, sr_id: Option<String>) -> anyhow::Result<Self> {
        let sr_id = match sr_id {
            Some(s) => s,
            None => get_sr_id().await?,
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!(
                "sr_id={sr_id}; uuid=b950e897-c6ab-46bc-828f-fa231a73cf3d; i18n_redirected=ja"
            ))
            .expect("sr_id is not a valid header value"),
        );
        builder = builder.default_headers(headers);

        Ok(Self {
            client: builder.build().unwrap(),
            sr_id,
        })
    }

    pub async fn onlives(&self) -> anyhow::Result<Vec<OnliveRoomInfo>> {
        let data: Onlives = self
            .client
            .get("https://www.showroom-live.com/api/live/onlives")
            .query(&[("skip_serial_code_live", "1")])
            .send()
            .await?
            .json()
            .await?;

        Ok(data.onlives.into_iter().flat_map(|c| c.lives).collect())
    }

    pub async fn room_info(&self, room_slug: &str) -> anyhow::Result<RoomInfo> {
        let data: RoomInfo = self
            .client
            .get(format!(
                "https://public-api.showroom-cdn.com/room/{room_slug}"
            ))
            .send()
            .await?
            .json()
            .await?;

        Ok(data)
    }

    pub async fn room_profile(&self, room_id: u64) -> anyhow::Result<RoomProfile> {
        let data = self
            .client
            .get("https://www.showroom-live.com/api/room/profile")
            .query(&[("room_id", room_id)])
            .send()
            .await?
            .json()
            .await
            .with_context(|| "room profile deserialize")?;

        Ok(data)
    }

    pub async fn live_info(&self, room_id: u64) -> anyhow::Result<LiveInfo> {
        let data = self
            .client
            .get("https://www.showroom-live.com/api/live/live_info")
            .query(&[("room_id", room_id)])
            .send()
            .await?
            .json()
            .await
            .with_context(|| "live info deserialize")?;

        Ok(data)
    }

    pub async fn live_streaming_url(&self, room_id: u64) -> anyhow::Result<LiveStreamlingList> {
        let data = self
            .client
            .get("https://www.showroom-live.com/api/live/streaming_url")
            .query(&[
                ("room_id", room_id.to_string()),
                ("abr_available", "0".to_string()),
            ])
            .send()
            .await?
            .json()
            .await
            .with_context(|| "streaming url json deserialize")?;
        Ok(data)
    }

    pub async fn timeshift_info(
        &self,
        room_url_key: &str,
        view_url_key: &str,
    ) -> anyhow::Result<TimeshiftInfo> {
        // https://www.showroom-live.com/api/timeshift/find?room_url_key=stu48_8th_Empathy_&view_url_key=K86763
        let data = self
            .client
            .get("https://www.showroom-live.com/api/timeshift/find")
            .query(&[
                ("room_url_key", room_url_key),
                ("view_url_key", view_url_key),
            ])
            .send()
            .await?
            .json()
            .await
            .with_context(|| "timeshift info json deserialize")?;
        Ok(data)
    }

    pub async fn timeshift_streaming_url(
        &self,
        room_id: u64,
        live_id: u64,
    ) -> anyhow::Result<TimeshiftStreamingList> {
        let data = self
            .client
            .get("https://www.showroom-live.com/api/timeshift/streaming_url")
            .query(&[("room_id", room_id), ("live_id", live_id)])
            .send()
            .await?
            .json()
            .await
            .with_context(|| "timeshift streaming url json deserialize")?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ShowRoomClient, constants::S46_NAGISA_KOJIMA};

    #[tokio::test]
    async fn test_get_id_by_room_name() {
        let client = ShowRoomClient::new(Default::default(), None).await.unwrap();
        let info = client.room_info(S46_NAGISA_KOJIMA).await.unwrap();
        assert_eq!(info.id, 479510);
    }
}
