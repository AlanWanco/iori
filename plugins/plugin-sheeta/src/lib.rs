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
        _url: &str,
        captures: &Captures,
        _args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let host = captures
            .name("host")
            .map(|s| s.as_str())
            .or(self.host.as_deref())
            .with_context(|| "Missing sheeta host")?;
        let client = SheetaClient::common(host).await?;

        let video_id = captures
            .name("video_id")
            .with_context(|| "Missing sheeta video id")?
            .as_str();

        let session_id = client.get_session_id(0, video_id).await?;
        let video_url = client.get_video_url(&session_id).await;
        Ok(InspectResult::Playlist(InspectPlaylist {
            playlist_url: video_url,
            headers: vec![
                format!("Referer: {}", client.origin()),
                format!("Origin: {}", client.origin()),
            ],
            ..Default::default()
        }))
    }
}
