use shiori_plugin::*;

pub struct ExamplePlugin;

impl ShioriPlugin for ExamplePlugin {
    fn name(&self) -> Cow<'static, str> {
        "example".into()
    }

    fn version(&self) -> Cow<'static, str> {
        "0.1.0".into()
    }

    fn description(&self) -> Option<Cow<'static, str>> {
        Some("Extracts Showroom playlists from the given URL.".into())
    }

    fn arguments(&self, command: &mut dyn InspectorCommand) {
        command.add_argument(
            "example-arg",
            Some("example_arg"),
            "[Example] Your example argument.",
        );
    }

    fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
        registry.register_inspector(
            Regex::new(r"https://example.com/(?<path>.*)").unwrap(),
            Box::new(ExampleInspector),
            PriorityHint::Normal,
        );

        Ok(())
    }
}

struct ExampleInspector;

#[async_trait]
impl Inspect for ExampleInspector {
    fn name(&self) -> Cow<'static, str> {
        "example".into()
    }

    async fn inspect(
        &self,
        _context: &ShioriContext,
        _url: &str,
        _captures: &Captures,
        _args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        Ok(InspectResult::Playlist(InspectPlaylist {
            title: Some("Example Playlist".to_string()),
            playlist_url: "https://example.com/playlist.m3u8".to_string(),
            playlist_type: PlaylistType::HLS,
            ..Default::default()
        }))
    }
}
