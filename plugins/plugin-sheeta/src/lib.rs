mod source;

pub use source::{SheetaSource, is_sheeta_url};

use anyhow::Context;
use iori_sheeta::client::SheetaClient;
use shiori_plugin::*;

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
            Box::new(SheetaInspector {
                name: "nicochannel+",
                host: Some("nicochannel.jp".to_string()),
            }),
            PriorityHint::Normal,
        );
        registry.register_inspector(
            SheetaClient::site_regex("qlover.jp"),
            Box::new(SheetaInspector {
                name: "qlover+",
                host: Some("qlover.jp".to_string()),
            }),
            PriorityHint::Normal,
        );
        registry.register_inspector(
            SheetaClient::wild_regex(),
            Box::new(SheetaInspector {
                name: "sheeta",
                host: None,
            }),
            PriorityHint::Low,
        );

        Ok(())
    }
}

struct SheetaInspector {
    name: &'static str,
    host: Option<String>,
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
        _args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let host = captures
            .name("host")
            .map(|s| s.as_str())
            .or(self.host.as_deref())
            .with_context(|| "Missing sheeta host")?;
        let client = SheetaClient::common(host, context.http.client()).await?;

        let video_id = captures
            .name("video_id")
            .with_context(|| "Missing sheeta video id")?
            .as_str();

        let session_id = client.get_session_id(0, video_id).await?;
        let video_url = client.get_video_url(&session_id).await;
        if !client.probe_video_url(&video_url).await? {
            // The session API may return successfully before the CDN publishes
            // the corresponding HLS object. Returning None lets the generic
            // `--wait` inspector loop create a fresh session and try again.
            return Ok(InspectResult::None);
        }

        Ok(InspectResult::Playlist(InspectPlaylist {
            playlist_url: video_url,
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
