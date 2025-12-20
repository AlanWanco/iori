use futures::{Stream, StreamExt, stream};
use iori::context::IoriContext;
use iori::{
    InitialSegment, IoriError, IoriResult, SegmentFormat, StreamType, StreamingSegment,
    StreamingSource, WriteSegment,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
pub struct TestSegment {
    pub stream_id: u64,
    pub sequence: u64,
    pub file_name: String,
    pub fail_count: Arc<AtomicU8>,
    pub delay: Option<Duration>,
    pub concurrent_counter: Arc<AtomicU32>,
    pub max_concurrent: Arc<AtomicU32>,
}

impl TestSegment {
    pub fn new(stream_id: u64, sequence: u64, file_name: String) -> Self {
        Self {
            stream_id,
            sequence,
            file_name,
            fail_count: Arc::new(AtomicU8::new(0)),
            delay: None,
            concurrent_counter: Arc::new(AtomicU32::new(0)),
            max_concurrent: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn with_fail_count(mut self, fail_count: u8) -> Self {
        self.fail_count = Arc::new(AtomicU8::new(fail_count));
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn with_counters(mut self, counter: Arc<AtomicU32>, max: Arc<AtomicU32>) -> Self {
        self.concurrent_counter = counter;
        self.max_concurrent = max;
        self
    }

    async fn write_data<W>(&self, writer: &mut W) -> IoriResult<()>
    where
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        let current = self.concurrent_counter.fetch_add(1, Ordering::SeqCst) + 1;
        loop {
            let max = self.max_concurrent.load(Ordering::SeqCst);
            if current <= max
                || self
                    .max_concurrent
                    .compare_exchange(max, current, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                break;
            }
        }

        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }

        let res = if self.fail_count.load(Ordering::Relaxed) > 0 {
            self.fail_count.fetch_sub(1, Ordering::Relaxed);
            Err(IoriError::IOError(std::io::Error::other(
                "Failed to write data",
            )))
        } else {
            let data = format!("Segment {} from stream {}", self.sequence, self.stream_id);
            writer.write_all(data.as_bytes()).await?;
            Ok(())
        };

        self.concurrent_counter.fetch_sub(1, Ordering::SeqCst);
        res
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

pub struct TestSource {
    batches: Vec<Vec<TestSegment>>,
}

impl TestSource {
    pub fn new(segments: Vec<TestSegment>) -> Self {
        Self {
            batches: vec![segments],
        }
    }

    pub fn new_with_batches(batches: Vec<Vec<TestSegment>>) -> Self {
        Self { batches }
    }
}

impl StreamingSource for TestSource {
    type Segment = TestSegment;

    async fn segments_stream(
        &self,
        _: &IoriContext,
    ) -> IoriResult<impl Stream<Item = IoriResult<Vec<Self::Segment>>>> {
        Ok(Box::pin(stream::iter(
            self.batches.clone().into_iter().map(Ok),
        )))
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
        TestSegment::new(1, 0, "segment0.ts".to_string()),
        TestSegment::new(1, 1, "segment1.ts".to_string()),
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
    let segment = TestSegment::new(1, 0, "segment0.ts".to_string());

    let mut writer = Vec::new();
    segment
        .write_segment(&Default::default(), &mut writer)
        .await
        .unwrap();

    let data = String::from_utf8(writer).unwrap();
    assert_eq!(data, "Segment 0 from stream 1");
}
