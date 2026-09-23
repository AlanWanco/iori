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
        command.add_argument(
            "eplus-refresh-interval",
            None,
            "[Eplus] Refresh interval in seconds for cookie updates. Mainly for debugging.",
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

        let Some(playlist_kind) = event_data.playlist_kind(prefer_archive) else {
            match (
                &event_data.delivery_status,
                event_data.archive_mode.as_deref(),
            ) {
                (iori_eplus::model::DeliveryStatus::Preparing, _) => {
                    log::info!("Eplus event is PREPARING; no download will start yet.");
                }
                (iori_eplus::model::DeliveryStatus::Started, _) if prefer_archive => {
                    log::info!(
                        "Eplus event is live, but archive mode was requested; waiting for the archive."
                    );
                }
                (iori_eplus::model::DeliveryStatus::Stopped, Some("ON")) => {
                    log::info!("Eplus event stopped; its archive is not confirmed yet.");
                }
                (iori_eplus::model::DeliveryStatus::Stopped, _) => {
                    log::info!("Eplus event stopped without an archive.");
                    return Ok(InspectResult::NotMatch);
                }
                (iori_eplus::model::DeliveryStatus::WaitConfirmArchived, _) => {
                    log::info!("Eplus archive is awaiting confirmation.");
                }
                (iori_eplus::model::DeliveryStatus::Unknown(_), _) => {
                    log::warn!("Eplus delivery status is not recognized.");
                }
                (iori_eplus::model::DeliveryStatus::ConfirmedArchive, _) => unreachable!(),
                (iori_eplus::model::DeliveryStatus::Started, _) => unreachable!(),
            }
            return Ok(InspectResult::None);
        };

        if event_data.m3u8_urls.is_empty() {
            log::info!("No Eplus playlist candidates are available for the current event status.");
            return Ok(InspectResult::None);
        }

        // Only probe playlists valid for the status-selected live/archive mode.
        let Some(playlist_url) = client
            .select_best_playlist_for_kind(
                &event_data.m3u8_urls,
                playlist_kind == iori_eplus::model::EplusPlaylistKind::Archive,
            )
            .await
        else {
            log::info!(
                "No playable Eplus playlist found for the current status ({} candidates).",
                event_data.m3u8_urls.len()
            );
            return Ok(InspectResult::None);
        };

        // Keep the original CloudFront Set-Cookie attributes. In particular, the
        // Path is bound to /out/v1/<playlist-id>; flattening these cookies to
        // name=value here would create a stale host-only duplicate on download.
        // Also include session cookies needed for the event-page refresh task.
        let mut cookies = event_data.cloudfront_cookies.clone();
        let playlist_cookies = context.http.export_cookies_for_url(&playlist_url);
        let event_cookies = context.http.export_cookies_for_url(url);
        for cookie in playlist_cookies.into_iter().chain(event_cookies) {
            let already_present = cookie.split_once('=').is_some_and(|(name, _)| {
                cookies.iter().any(|existing| {
                    existing
                        .split_once('=')
                        .is_some_and(|(existing_name, _)| existing_name.trim() == name.trim())
                })
            });
            if !already_present {
                cookies.push(cookie);
            }
        }

        let content_type = match playlist_kind {
            iori_eplus::model::EplusPlaylistKind::Live => ContentType::Live,
            iori_eplus::model::EplusPlaylistKind::Archive => ContentType::Archive,
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
