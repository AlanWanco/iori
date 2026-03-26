mod error;
#[doc(hidden)]
pub mod m3u8_rs;
mod models;
#[doc(hidden)]
pub mod parse;

pub use error::M3u8ParseError;
pub use models::*;

pub fn parse_playlist_res(input: &[u8]) -> Result<Playlist, M3u8ParseError> {
    let m3u8_rs_result = m3u8_rs::parse_playlist_res(input);
    let quick_m3u8_result = parse::parse_playlist_res(input);

    match (&m3u8_rs_result, &quick_m3u8_result) {
        (Ok(m3u8_rs_playlist), Ok(quick_m3u8_playlist)) => {
            if m3u8_rs_playlist != quick_m3u8_playlist {
                tracing::debug!(
                    "New m3u8 parse engine produced different result, this should not happen.\nold: {:?}\nnew: {:?}\nRaw input: {}",
                    m3u8_rs_playlist,
                    quick_m3u8_playlist,
                    String::from_utf8_lossy(input)
                );
            }
        }
        (Ok(_), Err(quick_m3u8_error)) => {
            tracing::debug!(
                "New m3u8 parse engine produced an error, but the old one passed.\nError: {quick_m3u8_error}\nRaw input: {}",
                String::from_utf8_lossy(input)
            );
        }
        (Err(m3u8_rs_error), Ok(_)) => {
            tracing::debug!(
                "Old m3u8 parse engine produced an error, but the new one passed.\nError: {m3u8_rs_error}\nRaw input: {}",
                String::from_utf8_lossy(input)
            );
        }
        _ => {
            // both errored, treat as normal
        }
    }

    // Always return the m3u8-rs result
    m3u8_rs_result
}
