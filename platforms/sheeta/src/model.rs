#![allow(dead_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SiteSettings {
    platform_id: String,
    fanclub_site_id: String,
    fanclub_group_id: String,
    pub(crate) api_base_url: String,
}

#[derive(Debug, Deserialize)]
pub struct EqPortalResponse<T> {
    data: T,
}

pub type FcVideoPageResponse = EqPortalResponse<FcVideoPageData>;

impl FcVideoPageResponse {
    pub fn fc_site_id(self) -> i32 {
        self.data.video_page.fanclub_site.id
    }

    pub fn title(&self) -> String {
        self.data.video_page.title.clone()
    }

    /// Request the DVR session when this is a finished live event with VOD
    /// conversion enabled. Ordinary videos and ongoing/upcoming lives use the
    /// default session type.
    pub fn session_broadcast_type(&self) -> Option<&'static str> {
        let video_page = &self.data.video_page;
        let finished_live = video_page.video_type.as_deref() == Some("live")
            && video_page
                .live_finished_at
                .as_deref()
                .is_some_and(|finished_at| !finished_at.trim().is_empty());
        let can_convert_to_vod = video_page.video.as_ref().is_some_and(|video| {
            video.allow_dvr_flg == Some(true) && video.convert_to_vod_flg == Some(true)
        });

        (finished_live && can_convert_to_vod).then_some("dvr")
    }
}

#[derive(Debug, Deserialize)]
pub struct FcVideoPageData {
    video_page: VideoPage,
}

#[derive(Debug, Deserialize)]
pub struct VideoPage {
    title: String,
    description: String,
    fanclub_site: FanclubSite,
    video_tags: Vec<VideoTag>,
    #[serde(rename = "type", default)]
    video_type: Option<String>,
    #[serde(default)]
    live_finished_at: Option<String>,
    #[serde(default)]
    video: Option<VideoSettings>,
}

#[derive(Debug, Deserialize)]
struct VideoSettings {
    #[serde(default)]
    allow_dvr_flg: Option<bool>,
    #[serde(default)]
    convert_to_vod_flg: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct VideoTag {
    id: i32,
    tag: String,
}

#[derive(Debug, Deserialize)]
pub struct FanclubSite {
    id: i32,
}

pub type SessionIdResponse = EqPortalResponse<SessionIdData>;

impl SessionIdResponse {
    pub fn session_id(self) -> String {
        self.data.session_id
    }
}

// {"data":{"session_id":"eeff71a4-5fa3-4f1f-9ced-c2c7894c79b8"}}
#[derive(Debug, Deserialize)]
pub struct SessionIdData {
    session_id: String,
}

pub type FcContentProviderResponse = EqPortalResponse<FcContentProviderData>;

impl FcContentProviderResponse {
    pub fn fc_site_id(self) -> i32 {
        self.data.content_providers.id
    }
}

// {
//     "data": {
//         "content_providers": {
//             "domain": "https://qlover.jp/non",
//             "fanclub_site": {
//                 "id": 744
//             },
//             "id": 744
//         }
//     }
// }
#[derive(Debug, Deserialize)]
pub struct FcContentProviderData {
    content_providers: ContentProvider,
}

#[derive(Debug, Deserialize)]
pub struct ContentProvider {
    domain: String,
    id: i32,
}

#[cfg(test)]
mod tests {
    use super::FcVideoPageResponse;

    #[test]
    fn video_page_response_preserves_japanese_title() {
        let title = "【プレミアムプラン限定おまけパート】【ゲスト：i☆Ris山北早紀・茜屋日海夏】大西亜玖璃のPONPONPON LIVE!!#11";
        let response: FcVideoPageResponse = serde_json::from_value(serde_json::json!({
            "data": {
                "video_page": {
                    "title": title,
                    "description": "",
                    "fanclub_site": { "id": 956 },
                    "video_tags": []
                }
            }
        }))
        .unwrap();

        assert_eq!(response.title(), title);
    }

    #[test]
    fn finished_live_with_vod_conversion_uses_dvr_session() {
        let mut body = serde_json::json!({
            "data": {
                "video_page": {
                    "title": "Archived live",
                    "description": "",
                    "fanclub_site": { "id": 956 },
                    "video_tags": [],
                    "type": "live",
                    "live_finished_at": "2026-09-24 12:30:00",
                    "video": {
                        "allow_dvr_flg": true,
                        "convert_to_vod_flg": true
                    }
                }
            }
        });

        let response: FcVideoPageResponse = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(response.session_broadcast_type(), Some("dvr"));

        body["data"]["video_page"]["live_finished_at"] = serde_json::Value::Null;
        let response: FcVideoPageResponse = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(response.session_broadcast_type(), None);

        body["data"]["video_page"]["live_finished_at"] = serde_json::json!("2026-09-24 12:30:00");
        body["data"]["video_page"]["video"]["allow_dvr_flg"] = serde_json::json!(false);
        let response: FcVideoPageResponse = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(response.session_broadcast_type(), None);

        body["data"]["video_page"]["video"]["allow_dvr_flg"] = serde_json::json!(true);
        body["data"]["video_page"]["type"] = serde_json::json!("vod");
        let response: FcVideoPageResponse = serde_json::from_value(body).unwrap();
        assert_eq!(response.session_broadcast_type(), None);
    }
}
