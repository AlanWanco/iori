use iori_showroom::model::RoomProfile;
use serde::Serialize;

#[derive(Serialize)]
pub struct WebhookBody {
    pub event: &'static str,

    pub prefix: String,
    pub profile: RoomProfile,
}
