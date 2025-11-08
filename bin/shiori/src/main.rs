use chrono::Timelike;
use clap::Parser;
use clap_handler::Handler;
use shiori::commands::ShioriArgs;
use std::fmt;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::time::FormatTime;

struct TimeOnly;

impl FormatTime for TimeOnly {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> fmt::Result {
        let now = chrono::Local::now();

        let hours = now.hour();
        let minutes = now.minute();
        let seconds = now.second();
        let millis = now.timestamp_millis() % 1000;

        write!(
            w,
            "{:02}:{:02}:{:02}.{:03}",
            hours, minutes, seconds, millis
        )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_timer(TimeOnly)
        .with_target(false)
        .with_level(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .try_from_env()
                .unwrap_or_else(|_| "info,i18n_embed=off".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    ShioriArgs::parse().run().await
}
