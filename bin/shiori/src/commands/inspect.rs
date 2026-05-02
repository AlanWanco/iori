use crate::inspect::{
    PluginManager,
    inspectors::{DashPlugin, HlsPlugin, ShortLinkPlugin},
};
use clap::Parser;
use clap_handler::handler;
use fake_user_agent::get_chrome_rua;
use iori::IoriHttp;
use reqwest::Client;
use shiori_plugin::{
    ContentType, InspectPlaylist, InspectSource, InspectorArguments, InspectorCommand,
    PlaylistType, ShioriContext,
};
use shiori_plugin_eplus::EplusPlugin;
use shiori_plugin_gigafile::GigafilePlugin;
use shiori_plugin_niconico::NiconicoPlugin;
use shiori_plugin_radiko::RadikoPlugin;
use shiori_plugin_sheeta::SheetaPlugin;
use shiori_plugin_showroom::ShowroomPlugin;

#[derive(Parser, Clone, Default)]
#[clap(name = "inspect", short_flag = 'S')]
pub struct InspectCommand {
    #[clap(short, long)]
    wait: bool,

    #[clap(flatten)]
    inspector_options: InspectorOptions,

    url: String,
}

pub(crate) fn get_default_external_inspector() -> PluginManager {
    let mut inspector = PluginManager::new();
    inspector
        .add(ShortLinkPlugin)
        .add(ShowroomPlugin)
        .add(NiconicoPlugin)
        .add(SheetaPlugin)
        .add(GigafilePlugin)
        .add(RadikoPlugin)
        .add(EplusPlugin)
        .add(HlsPlugin)
        .add(DashPlugin);

    inspector
}

#[handler(InspectCommand)]
async fn handle_inspect(this: InspectCommand) -> anyhow::Result<()> {
    let context = ShioriContext {
        http: IoriHttp::new(|| Client::builder().user_agent(get_chrome_rua())),
    };
    let (matched_inspector, data) = get_default_external_inspector()
        .wait(this.wait)
        .inspect(&context, &this.url, &this.inspector_options, |c| {
            c.into_iter().next().unwrap()
        })
        .await?;

    eprintln!("Inspector: {matched_inspector}");
    for (index, playlist) in data.iter().enumerate() {
        if index > 0 {
            eprintln!();
        }
        print_playlist_summary(index, playlist);
    }

    Ok(())
}

fn print_playlist_summary(index: usize, playlist: &InspectPlaylist) {
    eprintln!("Playlist {}", index + 1);
    eprintln!("Title: {}", playlist.title.as_deref().unwrap_or("<unknown>"));
    eprintln!("Playlist Type: {}", format_playlist_type(&playlist.playlist_type));
    eprintln!("Playlist URL: {}", playlist.playlist_url);

    if let Some(source) = &playlist.source {
        print_source_summary(source);
    }

    if let Some(streams_hint) = playlist.streams_hint {
        eprintln!("Streams Hint: {streams_hint}");
    }

    if !playlist.headers.is_empty() {
        eprintln!("Headers: {} entries", playlist.headers.len());
    }

    if !playlist.cookies.is_empty() {
        eprintln!("Cookies: {} entries", playlist.cookies.len());
    }

    if playlist.key.is_some() {
        eprintln!("Key: present");
    }

    if playlist.initial_playlist_data.is_some() {
        eprintln!("Initial Playlist Data: present");
    }
}

fn print_source_summary(source: &InspectSource) {
    eprintln!("Platform: {}", source.platform);
    eprintln!("Content Type: {}", format_content_type(&source.content_type));

    if let Some(content_id) = source.content_id.as_deref() {
        eprintln!("Content ID: {content_id}");
    }

    if let Some(channel_id) = source.channel_id.as_deref() {
        eprintln!("Channel ID: {channel_id}");
    }

    if let Some(original_url) = source.original_url.as_deref() {
        eprintln!("Original URL: {original_url}");
    }
}

fn format_playlist_type(playlist_type: &PlaylistType) -> &'static str {
    match playlist_type {
        PlaylistType::HLS => "HLS",
        PlaylistType::DASH => "DASH",
        PlaylistType::RawData => "RawData",
        PlaylistType::Http => "HTTP",
        PlaylistType::RawRemoteSegments(_) => "RawRemoteSegments",
        PlaylistType::Unknown => "Unknown",
    }
}

fn format_content_type(content_type: &ContentType) -> &'static str {
    match content_type {
        ContentType::Live => "Live",
        ContentType::Archive => "Archive",
        ContentType::Video => "Video",
        ContentType::File => "File",
    }
}

#[derive(Clone, Debug, Default)]
pub struct InspectorOptions {
    arg_matches: clap::ArgMatches,
}

impl InspectorOptions {
    pub fn new(arg_matches: clap::ArgMatches) -> Self {
        Self { arg_matches }
    }
}

impl InspectorArguments for InspectorOptions {
    fn get_string(&self, argument: &'static str) -> Option<String> {
        self.arg_matches.get_one::<String>(argument).cloned()
    }

    fn get_boolean(&self, argument: &'static str) -> bool {
        self.arg_matches
            .get_one::<bool>(argument)
            .copied()
            .unwrap_or(false)
    }
}

impl clap::FromArgMatches for InspectorOptions {
    fn from_arg_matches(arg_matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self::new(arg_matches.clone()))
    }

    fn from_arg_matches_mut(arg_matches: &mut clap::ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self::new(arg_matches.clone()))
    }

    fn update_from_arg_matches(
        &mut self,
        arg_matches: &clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        self.update_from_arg_matches_mut(&mut arg_matches.clone())
    }

    fn update_from_arg_matches_mut(
        &mut self,
        arg_matches: &mut clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        self.arg_matches = arg_matches.clone();
        Result::Ok(())
    }
}

impl clap::Args for InspectorOptions {
    fn group_id() -> Option<clap::Id> {
        Some(clap::Id::from("InspectorOptions"))
    }

    fn augment_args<'b>(command: clap::Command) -> clap::Command {
        InspectorOptions::augment_args_for_update(command)
    }

    fn augment_args_for_update<'b>(command: clap::Command) -> clap::Command {
        let inspectors = get_default_external_inspector();
        let mut wrapper = InspectorCommandWrapper::new(command);
        inspectors.add_arguments(&mut wrapper);

        wrapper.into_inner()
    }
}

struct InspectorCommandWrapper(Option<clap::Command>);

impl InspectorCommandWrapper {
    fn new(command: clap::Command) -> Self {
        Self(Some(command))
    }

    fn into_inner(self) -> clap::Command {
        self.0.unwrap()
    }
}

impl InspectorCommand for InspectorCommandWrapper {
    fn add_argument(
        &mut self,
        long: &'static str,
        value_name: Option<&'static str>,
        help: &'static str,
    ) {
        let command = self.0.take().unwrap();
        self.0 = Some(
            command.arg(
                clap::Arg::new(long)
                    .value_name(value_name.unwrap_or(long))
                    .value_parser(clap::value_parser!(String))
                    .action(clap::ArgAction::Set)
                    .long(long)
                    .help(help),
            ),
        );
    }

    fn add_boolean_argument(&mut self, long: &'static str, help: &'static str) {
        let command = self.0.take().unwrap();
        self.0 = Some(
            command.arg(
                clap::Arg::new(long)
                    .value_parser(clap::value_parser!(bool))
                    .action(clap::ArgAction::SetTrue)
                    .long(long)
                    .help(help),
            ),
        );
    }
}
