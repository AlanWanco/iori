use fake_user_agent::get_chrome_rua;
use iori_gigafile::GigafileClient;
use shiori_plugin::iori::reqwest::header::{CONTENT_DISPOSITION, COOKIE, USER_AGENT};
use shiori_plugin::*;

pub struct GigafilePlugin;

impl ShioriPlugin for GigafilePlugin {
    fn name(&self) -> Cow<'static, str> {
        "gigafile".into()
    }

    fn version(&self) -> Cow<'static, str> {
        "0.1.0".into()
    }

    fn description(&self) -> Option<Cow<'static, str>> {
        Some("Extracts raw download URL from Gigafile.".into())
    }

    fn arguments(&self, command: &mut dyn InspectorCommand) {
        command.add_argument("giga-key", Some("key"), "[Gigafile] Download key");
    }

    fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
        let regex = Regex::new(r"https://(\d+)\.gigafile\.nu/.*").unwrap();
        registry.register_inspector(regex, Box::new(GigafileInspector), PriorityHint::Normal);
        Ok(())
    }
}

struct GigafileInspector;

#[async_trait]
impl Inspect for GigafileInspector {
    fn name(&self) -> Cow<'static, str> {
        "gigafile".into()
    }

    async fn inspect(
        &self,
        context: &ShioriContext,
        url: &str,
        _captures: &Captures,
        args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let key = args.get_string("giga-key");
        let client = GigafileClient::new(context.http.client(), key);
        let (download_url, cookie) = client.get_download_url(url.try_into()?).await?;

        let client = context.http.client();
        let response = client
            .get(&download_url)
            .header(COOKIE, &cookie)
            .header(USER_AGENT, get_chrome_rua())
            .send()
            .await?;
        let filename = response.headers().get(CONTENT_DISPOSITION).and_then(|v| {
            // attachment; filename="<filename>";
            let re = regex::bytes::Regex::new(r#"filename="([^"]+)""#).unwrap();
            let matched = re
                .captures(v.as_bytes())
                .and_then(|c| c.get(1).map(|m| m.as_bytes()))?;
            let filename = String::from_utf8(matched.to_vec()).ok()?;
            Some(filename)
        });
        drop(response);

        Ok(InspectResult::Playlist(InspectPlaylist {
            title: filename,
            playlist_url: download_url,
            playlist_type: PlaylistType::Http,
            headers: vec![format!("Cookie: {cookie}")],
            source: Some(InspectSource::new("gigafile", ContentType::File).with_original_url(url)),
            ..Default::default()
        }))
    }
}
