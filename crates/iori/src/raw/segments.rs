use std::sync::Mutex;

use tokio::{io::AsyncWrite, sync::mpsc};

use crate::{
    ByteRange, HttpClient, IoriResult, RemoteStreamingSegment, StreamingSegment, StreamingSource,
    fetch::fetch_segment,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RawRemoteSegment {
    pub url: reqwest::Url,
    pub filename: String,
    pub range: Option<ByteRange>,

    pub stream_id: u64,
    pub sequence: u64,
}

impl StreamingSegment for RawRemoteSegment {
    fn stream_id(&self) -> u64 {
        self.stream_id
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn file_name(&self) -> &str {
        &self.filename
    }

    fn key(&self) -> Option<std::sync::Arc<crate::decrypt::IoriKey>> {
        // TODO: Support key
        None
    }

    fn r#type(&self) -> crate::SegmentType {
        crate::SegmentType::Subtitle
    }

    fn format(&self) -> crate::SegmentFormat {
        crate::SegmentFormat::from_filename(&self.filename)
    }
}

impl RemoteStreamingSegment for RawRemoteSegment {
    fn url(&self) -> reqwest::Url {
        self.url.clone()
    }

    fn byte_range(&self) -> Option<ByteRange> {
        self.range.clone()
    }
}

pub struct RawRemoteSegmentsSource {
    client: HttpClient,
    segments: Mutex<Vec<RawRemoteSegment>>,
}

impl RawRemoteSegmentsSource {
    pub fn new(client: HttpClient, segments: Vec<RawRemoteSegment>) -> Self {
        Self {
            client,
            segments: Mutex::new(segments),
        }
    }
}

impl StreamingSource for RawRemoteSegmentsSource {
    type Segment = RawRemoteSegment;

    async fn fetch_info(
        &self,
    ) -> IoriResult<mpsc::UnboundedReceiver<IoriResult<Vec<Self::Segment>>>> {
        let segments = self.segments.lock().unwrap().drain(..).collect();

        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Ok(segments)).unwrap();
        Ok(rx)
    }

    async fn fetch_segment<W>(&self, segment: &Self::Segment, writer: &mut W) -> IoriResult<()>
    where
        W: AsyncWrite + Unpin + Send + Sync + 'static,
    {
        fetch_segment(self.client.clone(), segment, writer, None).await
    }
}
