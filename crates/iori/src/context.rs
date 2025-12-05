use std::{path::PathBuf, sync::Arc};

use crate::HttpClient;

#[derive(Clone, Default)]
pub struct IoriContext {
    pub client: HttpClient,
    pub shaka_packager_command: Arc<Option<PathBuf>>,
}

impl IoriContext {
    pub fn new(client: HttpClient, shaka_packager_command: Option<PathBuf>) -> Self {
        Self {
            client,
            shaka_packager_command: Arc::new(shaka_packager_command),
        }
    }
}
