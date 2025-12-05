use std::{path::PathBuf, sync::Arc};

use crate::HttpClient;

#[derive(Clone)]
pub struct IoriContext {
    pub client: HttpClient,
    pub shaka_packager_command: Arc<Option<PathBuf>>,

    pub manifest_retries: u32,
    pub segment_retries: u32,
}

impl Default for IoriContext {
    fn default() -> Self {
        Self {
            client: HttpClient::default(),
            shaka_packager_command: Arc::new(None),
            manifest_retries: 3,
            segment_retries: 5,
        }
    }
}
