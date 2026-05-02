use iori_eplus::EplusClient;
use shiori_plugin::*;

pub struct EplusPlugin;

impl ShioriPlugin for EplusPlugin {
    fn name(&self) -> Cow<'static, str> {
        "eplus".into()
    }

    fn version(&self) -> Cow<'static, str> {
        "0.1.0".into()
    }

    fn description(&self) -> Option<Cow<'static, str>> {
        Some("Extracts eplus.jp live/archive stream playlists.".into())
    }

    fn arguments(&self, command: &mut dyn InspectorCommand) {
        command.add_argument(
            "eplus-username",
            Some("eplus_username"),
            "[Eplus] Your eplus.jp login ID (email).",
        );
        command.add_argument(
            "eplus-password",
            Some("eplus_password"),
            "[Eplus] Your eplus.jp login password.",
        );
        command.add_boolean_argument(
            "eplus-archive",
            "[Eplus] Prefer archive/VOD stream over live stream.",
        );
    }

    fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
        // Match eplus player pages: https://live.eplus.jp/ex/player?ib=...
        registry.register_inspector(
            Regex::new(r"https://live\.eplus\.jp/ex/player\?ib=(?P<ib>.+)").unwrap(),
            Box::new(EplusInspector),
            PriorityHint::Normal,
        );
        // Match vp pages: https://live.eplus.jp/vp/<id>
        registry.register_inspector(
            Regex::new(r"https://live\.eplus\.jp/vp/(?P<id>[^/?#]+)$").unwrap(),
            Box::new(EplusInspector),
            PriorityHint::Normal,
        );
        // Match direct event page URLs
        registry.register_inspector(
            Regex::new(r"https://live\.eplus\.jp/(?P<path>[^/]+)$").unwrap(),
            Box::new(EplusInspector),
            PriorityHint::Normal,
        );

        Ok(())
    }
}

struct EplusInspector;

#[async_trait]
impl Inspect for EplusInspector {
    fn name(&self) -> Cow<'static, str> {
        "eplus".into()
    }

    async fn inspect(
        &self,
        context: &ShioriContext,
        url: &str,
        _captures: &Captures,
        args: &dyn InspectorArguments,
    ) -> anyhow::Result<InspectResult> {
        let username = args.get_string("eplus-username");
        let password = args.get_string("eplus-password");
        let prefer_archive = args.get_boolean("eplus-archive");

        // Create client using the shared IoriHttp cookie store.
        // After login, session cookies are stored in context.http's cookie jar.
        let client = match (username, password) {
            (Some(user), Some(pass)) => {
                EplusClient::login(context.http.builder(), url, &user, &pass).await?
            }
            _ => {
                log::info!("No eplus credentials provided, attempting anonymous access.");
                EplusClient::new(context.http.builder())?
            }
        };

        // Fetch event data — CloudFront Set-Cookie headers are stored in the shared jar.
        let event_data = client.get_event_data(url).await?;

        if event_data.m3u8_urls.is_empty() {
            log::warn!("No m3u8 URLs found on the page.");
            return Ok(InspectResult::None);
        }

        // Select the best playlist URL
        let Some(playlist_url) = client
            .select_best_playlist(&event_data.m3u8_urls, prefer_archive)
            .await
        else {
            log::info!("Title: {}", event_data.title);
            log::info!("Status: {:?}", event_data.delivery_status);
            log::info!("Playlist Candidates: {}", event_data.m3u8_urls.len());
            log::warn!("Could not determine a valid playlist URL.");
            return Ok(InspectResult::None);
        };

        if event_data.delivery_status == iori_eplus::model::DeliveryStatus::Preparing {
            log::info!(
                "Event is marked PREPARING, but a cookie-authenticated playlist is already available."
            );
        }

        // Export all cookies relevant for:
        // 1. The event page (session cookies for refresh)
        // 2. The CDN domain (CloudFront cookies for segment fetches)
        let mut cookies = context.http.export_cookies_for_url(&playlist_url);
        let event_cookies = context.http.export_cookies_for_url(url);
        for cookie in event_cookies {
            if !cookies.contains(&cookie) {
                cookies.push(cookie);
            }
        }

        let content_type = if playlist_url.contains("stream.live.eplus.jp") {
            ContentType::Live
        } else {
            ContentType::Archive
        };

        Ok(InspectResult::Playlist(InspectPlaylist {
            title: Some(event_data.title),
            playlist_url,
            playlist_type: PlaylistType::HLS,
            cookies,
            source: Some(
                InspectSource::new("eplus", content_type)
                    .with_content_id(event_data.app_id)
                    .with_original_url(url),
            ),
            ..Default::default()
        }))
    }
}
