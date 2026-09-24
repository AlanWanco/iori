mod source;

pub use source::{SheetaSource, is_sheeta_url};

use anyhow::Context;
use iori_sheeta::client::SheetaClient;
use shiori_plugin::*;
use std::{collections::HashMap, sync::Mutex};

pub struct SheetaPlugin;

impl ShioriPlugin for SheetaPlugin {
    fn name(&self) -> Cow<'static, str> {
        "sheeta".into()
    }

    fn version(&self) -> Cow<'static, str> {
        "0.1.0".into()
    }

    fn description(&self) -> Option<Cow<'static, str>> {
        Some("Extract videos from nicochannel+ based platforms.".into())
    }

    fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
        registry.register_inspector(
            SheetaClient::site_regex("nicochannel.jp"),
            Box::new(SheetaInspector::new(
                "nicochannel+",
                Some("nicochannel.jp".to_string()),
            )),
            PriorityHint::Normal,
        );
        registry.register_inspector(
            SheetaClient::site_regex("qlover.jp"),
            Box::new(SheetaInspector::new(
                "qlover+",
                Some("qlover.jp".to_string()),
            )),
            PriorityHint::Normal,
        );
        registry.register_inspector(
            SheetaClient::wild_regex(),
            Box::new(SheetaInspector::new("sheeta", None)),
            PriorityHint::Low,
        );

        Ok(())
    }
}

struct SheetaInspector {
    name: &'static str,
    host: Option<String>,
    clients: Mutex<HashMap<String, SheetaClient>>,
    fc_site_ids: Mutex<HashMap<(String, String), i32>>,
    video_titles: Mutex<HashMap<(String, String), String>>,
}

impl SheetaInspector {
    fn new(name: &'static str, host: Option<String>) -> Self {
        Self {
            name,
            host,
            clients: Mutex::new(HashMap::new()),
            fc_site_ids: Mutex::new(HashMap::new()),
            video_titles: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Inspect for SheetaInspector {
    fn name(&self) -> Cow<'static, str> {
        self.name.into()
    }

    async fn inspect(
        &self,
        context: &ShioriContext,
        url: &str,
        captures: &Captures,
        args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let host = captures
            .name("host")
            .map(|s| s.as_str())
            .or(self.host.as_deref())
            .with_context(|| "Missing sheeta host")?
            .to_string();
        let video_id = captures
            .name("video_id")
            .with_context(|| "Missing sheeta video id")?
            .as_str()
            .to_string();
        let channel = captures
            .name("channel")
            .map(|capture| capture.as_str().to_string());

        // Cache site settings and channel IDs across --wait retries. Once a
        // session URL exists, wait mode polls that HLS URL instead of revisiting
        // the Qlover page/config endpoints on every retry.
        let client = if let Some(client) = self.clients.lock().unwrap().get(&host).cloned() {
            client
        } else {
            let client = SheetaClient::common(&host, context.http.client()).await?;
            self.clients
                .lock()
                .unwrap()
                .insert(host.clone(), client.clone());
            client
        };
        let fc_site_id = if let Some(channel) = channel.as_deref() {
            let cache_key = (host.clone(), channel.to_string());
            if let Some(site_id) = self.fc_site_ids.lock().unwrap().get(&cache_key).copied() {
                site_id
            } else {
                let site_id = client.get_fc_site_id(channel).await?;
                self.fc_site_ids.lock().unwrap().insert(cache_key, site_id);
                site_id
            }
        } else {
            0
        };

        let session_id = client.get_session_id(fc_site_id, &video_id).await?;
        let video_url = client.get_video_url(&session_id).await;
        if !args.get_boolean("shiori-wait") && !client.probe_video_url(&video_url).await? {
            return Ok(InspectResult::None);
        }

        let title = if args.get_boolean("shiori-skip-title") {
            None
        } else {
            let cache_key = (host.clone(), video_id.clone());
            let cached_title = self.video_titles.lock().unwrap().get(&cache_key).cloned();
            let title = if let Some(title) = cached_title {
                title
            } else {
                let title = match client.get_video_data(fc_site_id, &video_id).await {
                    Ok(video_page) => video_page.title(),
                    Err(_) => {
                        log::warn!(
                            "Failed to fetch the Sheeta video title; falling back to the video ID."
                        );
                        video_id.clone()
                    }
                };
                let title = if title.trim().is_empty() {
                    video_id.clone()
                } else {
                    title
                };
                self.video_titles
                    .lock()
                    .unwrap()
                    .insert(cache_key, title.clone());
                title
            };
            Some(title)
        };

        Ok(InspectResult::Playlist(InspectPlaylist {
            title,
            playlist_url: video_url,
            playlist_type: PlaylistType::HLS,
            headers: vec![
                format!("Referer: {}", client.origin()),
                format!("Origin: {}", client.origin()),
            ],
            source: Some(
                InspectSource::new(host, ContentType::Video)
                    .with_content_id(video_id)
                    .with_original_url(url),
            ),
            ..Default::default()
        }))
    }
}
