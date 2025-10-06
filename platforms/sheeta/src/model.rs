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

    pub fn title(self) -> String {
        self.data.video_page.title
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
