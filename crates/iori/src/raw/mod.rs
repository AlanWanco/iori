use std::{borrow::Cow, path::PathBuf};

use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

use crate::{IoriResult, StreamingSegment, StreamingSource};

mod http;
pub use http::*;

mod segments;
pub use segments::*;

pub struct RawDataSource {
    data: String,
    ext: String,
}

impl RawDataSource {
    pub fn new(data: String, url: String) -> Self {
        let ext = PathBuf::from(url)
            .extension()
            .map(|e| e.to_string_lossy())
            .unwrap_or(Cow::Borrowed("raw"))
            .to_string();

        Self { data, ext }
    }
}

pub struct RawSegment {
    data: String,
    filename: String,
    ext: String,
}

impl RawSegment {
    pub fn new(data: String, ext: String) -> Self {
        Self {
            data,
            filename: format!("data.{ext}"),
            ext,
        }
    }
}

impl StreamingSegment for RawSegment {
    fn stream_id(&self) -> u64 {
        0
    }

    fn sequence(&self) -> u64 {
        0
    }

    fn file_name(&self) -> &str {
        &self.filename
    }

    fn key(&self) -> Option<std::sync::Arc<crate::decrypt::IoriKey>> {
        None
    }

    fn stream_type(&self) -> crate::StreamType {
        crate::StreamType::Unknown
    }

    fn format(&self) -> crate::SegmentFormat {
        crate::SegmentFormat::Raw(Some(self.ext.clone()))
    }
}

impl StreamingSource for RawDataSource {
    type Segment = RawSegment;

    async fn fetch_info(
        &self,
    ) -> IoriResult<mpsc::UnboundedReceiver<IoriResult<Vec<Self::Segment>>>> {
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Ok(vec![RawSegment::new(
            self.data.clone(),
            self.ext.clone(),
        )]))
        .unwrap();
        Ok(rx)
    }

    async fn fetch_segment<W>(&self, segment: &Self::Segment, writer: &mut W) -> IoriResult<()>
    where
        W: AsyncWrite + Unpin + Send + Sync + 'static,
    {
        writer.write_all(segment.data.as_bytes()).await?;
        Ok(())
    }
}
