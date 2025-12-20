use reqwest::Client;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct IoriContext {
    pub client: Client,
    pub shaka_packager_command: Arc<Option<PathBuf>>,

    pub manifest_retries: u32,
    pub segment_retries: u32,
}

impl Default for IoriContext {
    fn default() -> Self {
        Self {
            client: Default::default(),
            shaka_packager_command: Arc::new(None),
            manifest_retries: 3,
            segment_retries: 5,
        }
    }
}
