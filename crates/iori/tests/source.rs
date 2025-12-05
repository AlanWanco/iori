use futures::{Stream, StreamExt, stream};
use iori::context::IoriContext;
use iori::{
    InitialSegment, IoriError, IoriResult, SegmentFormat, StreamType, StreamingSegment,
    StreamingSource, WriteSegment,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
pub struct TestSegment {
    pub stream_id: u64,
    pub sequence: u64,
    pub file_name: String,
    pub fail_count: Arc<AtomicU8>,
}

impl TestSegment {
    async fn write_data<W>(&self, writer: &mut W) -> IoriResult<()>
    where
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        if self.fail_count.load(Ordering::Relaxed) > 0 {
            self.fail_count.fetch_sub(1, Ordering::Relaxed);
            return Err(IoriError::IOError(std::io::Error::other(
                "Failed to write data",
            )));
        }

        let data = format!("Segment {} from stream {}", self.sequence, self.stream_id);
        writer.write_all(data.as_bytes()).await?;
        Ok(())
    }
}

impl StreamingSegment for TestSegment {
    fn stream_id(&self) -> u64 {
        self.stream_id
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn file_name(&self) -> &str {
        &self.file_name
    }

    fn initial_segment(&self) -> InitialSegment {
        InitialSegment::None
    }

    fn key(&self) -> Option<Arc<iori::decrypt::IoriKey>> {
        None
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn format(&self) -> SegmentFormat {
        SegmentFormat::Mpeg2TS
    }
}

#[derive(Clone)]
pub struct TestSource {
    segments: Vec<TestSegment>,
}

impl TestSource {
    pub fn new(segments: Vec<TestSegment>) -> Self {
        Self { segments }
    }
}

impl StreamingSource for TestSource {
    type Segment = TestSegment;

    async fn segments_stream(
        &self,
        _: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        Ok(Box::pin(stream::once(async { Ok(self.segments.clone()) })))
    }
}

impl WriteSegment for TestSegment {
    async fn write_segment<W>(&self, _: &IoriContext, writer: &mut W) -> IoriResult<()>
    where
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        self.write_data(writer).await
    }
}

#[tokio::test]
async fn test_streaming_source_implementation() {
    let segments = vec![
        TestSegment {
            stream_id: 1,
            sequence: 0,
            file_name: "segment0.ts".to_string(),
            fail_count: Arc::new(AtomicU8::new(0)),
        },
        TestSegment {
            stream_id: 1,
            sequence: 1,
            file_name: "segment1.ts".to_string(),
            fail_count: Arc::new(AtomicU8::new(0)),
        },
    ];

    let context = IoriContext::default();
    let source = TestSource::new(segments.clone());
    let mut stream = source
        .segments_stream(&context)
        .await
        .expect("Failed to get segments stream");

    let mut received_segments: Vec<TestSegment> = Vec::new();
    while let Some(result) = stream.next().await {
        received_segments.extend(result.unwrap());
    }

    assert_eq!(received_segments.len(), segments.len());
    for (received, expected) in received_segments.iter().zip(segments.iter()) {
        assert_eq!(received.stream_id(), expected.stream_id());
        assert_eq!(received.sequence(), expected.sequence());
        assert_eq!(received.file_name(), expected.file_name());
    }
}

#[tokio::test]
async fn test_streaming_source_fetch_segment() {
    let segment = TestSegment {
        stream_id: 1,
        sequence: 0,
        file_name: "segment0.ts".to_string(),
        fail_count: Arc::new(AtomicU8::new(0)),
    };

    let mut writer = Vec::new();
    segment
        .write_segment(&Default::default(), &mut writer)
        .await
        .unwrap();

    let data = String::from_utf8(writer).unwrap();
    assert_eq!(data, "Segment 0 from stream 1");
}
