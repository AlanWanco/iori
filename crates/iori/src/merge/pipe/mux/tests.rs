use super::*;
use crate::cache::memory::MemoryCacheSource;
use tokio::io::AsyncWriteExt;

async fn segment(
    lane: usize,
    sequence: u64,
    data: Vec<u8>,
    cache: Arc<MemoryCacheSource>,
) -> Arc<CachedSegment> {
    let info = SegmentInfo {
        stream_id: lane as u64,
        stream_type: if lane == 0 {
            StreamType::Video
        } else {
            StreamType::Audio
        },
        sequence,
        synchronization_key: Some((0, sequence)),
        file_name: format!("{lane}-{sequence}.mp4"),
        ..Default::default()
    };
    let mut writer = cache.open_writer(&info).await.unwrap().unwrap();
    writer.write_all(&data).await.unwrap();
    writer.shutdown().await.unwrap();
    drop(writer);
    CachedSegment::new(info, cache, false)
}

#[tokio::test]
async fn restart_aligns_by_media_key_not_local_sequence() {
    let cache = Arc::new(MemoryCacheSource::new());
    let video = segment(0, 12, vec![1], cache.clone()).await;
    let audio = segment(1, 13, vec![2], cache.clone()).await;
    let next_video = segment(0, 13, vec![3], cache).await;
    let mut tracks = [Track::default(), Track::default()];
    tracks[0].pending.extend([video, next_video]);
    tracks[1].pending.push_back(audio);
    assert!(align(&mut tracks, true));
    assert_eq!(
        tracks[0].pending.front().unwrap().info.synchronization_key,
        Some((0, 13))
    );
}

#[test]
fn stderr_redacts_output_and_source_urls() {
    let text = "opening rtmps://example.invalid/app/private-key and https://example.invalid/a?session=private";
    let result = redact(text, "rtmps://example.invalid/app/private-key");
    assert!(!result.contains("private"));
    assert!(!result.contains("session="));
}

#[cfg(unix)]
mod process {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // No global PATH/env changes. The fake child consumes real OS video/audio
    // pipes and emits the same progress protocol as FFmpeg. First child exits
    // with unread audio bytes; second must see BOTH streams from byte zero.
    fn fake_ffmpeg(dir: &std::path::Path, mode: &str) -> PathBuf {
        let path = dir.join("fake-ffmpeg");
        let script = format!(
            r#"#!/usr/bin/env python3
import os, pathlib, sys, threading, time
root = pathlib.Path(__file__).parent
mode = {mode:?}
count_file = root / 'starts'
n = int(count_file.read_text()) + 1 if count_file.exists() else 1
count_file.write_text(str(n))
(root / ('pid-' + str(n))).write_text(str(os.getpid()))
if mode == 'always-fail':
    sys.exit(1)
if mode == 'hang':
    time.sleep(120)
    sys.exit(1)
if n == 1 and mode == 'audio-fail':
    os.close(3)
    time.sleep(120)
    sys.exit(1)
if n == 1 and mode in ('restart', 'cancel-backoff'):
    assert os.read(0, 4) == b'VINI'
    assert os.read(3, 4) == b'AINI'
    sys.exit(1)
def read_stream(fd, label):
    data = bytearray()
    while True:
        block = os.read(fd, 65536)
        if not block:
            break
        data.extend(block)
    (root / (label + str(n))).write_bytes(data)
if mode == 'single':
    read_stream(0, 'video')
    print('out_time_us=3000000\nprogress=end', file=sys.stderr, flush=True)
    sys.exit(0)
audio = threading.Thread(target=read_stream, args=(3, 'audio'))
audio.start()
read_stream(0, 'video')
audio.join()
print('out_time_us=3000000\nprogress=end', file=sys.stderr, flush=True)
"#
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn config(program: PathBuf) -> Config {
        Config {
            program,
            output: "rtmp://example.invalid/app/test".into(),
            extra: None,
            has_audio: true,
            readrate: false,
            catchup: false,
            recycle: true,
            buffer: 0,
            retry_delay: Duration::from_millis(10),
        }
    }

    async fn queue_pair(
        sender: &mpsc::UnboundedSender<Message>,
        cache: Arc<MemoryCacheSource>,
    ) -> [Vec<u8>; 2] {
        // Larger than pipe capacity; a partial write must not consume the
        // cached readers permanently or let one track recycle too early.
        let mut v = b"VINI".to_vec();
        v.extend(vec![7; 1024 * 1024]);
        let mut a = b"AINI".to_vec();
        a.extend(vec![8; 1024 * 1024]);
        for (lane, data) in [v.clone(), a.clone()].into_iter().enumerate() {
            sender
                .send((lane as u64, 0, segment(lane, 0, data, cache.clone()).await))
                .unwrap();
        }
        [v, a]
    }

    async fn wait_for_start(dir: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dir.join("starts").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn child_failure_recreates_both_pipes_and_replays_complete_init_and_media() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "restart"));
        let cache = Arc::new(MemoryCacheSource::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let expected = queue_pair(&tx, cache).await;
        drop(tx);
        tokio::time::timeout(
            Duration::from_secs(10),
            run(cfg, rx, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "2"
        );
        assert_eq!(
            std::fs::read(dir.path().join("video2")).unwrap(),
            expected[0]
        );
        assert_eq!(
            std::fs::read(dir.path().join("audio2")).unwrap(),
            expected[1]
        );
    }

    #[tokio::test]
    async fn audio_only_failure_also_restarts_both_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "audio-fail"));
        let cache = Arc::new(MemoryCacheSource::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let expected = queue_pair(&tx, cache).await;
        drop(tx);
        tokio::time::timeout(
            Duration::from_secs(10),
            run(cfg, rx, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "2"
        );
        assert_eq!(
            std::fs::read(dir.path().join("video2")).unwrap(),
            expected[0]
        );
        assert_eq!(
            std::fs::read(dir.path().join("audio2")).unwrap(),
            expected[1]
        );
    }

    #[tokio::test]
    async fn cancellation_kills_blocked_child_without_restart_or_recycling() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "hang"));
        let cache = Arc::new(MemoryCacheSource::new());
        let (tx, rx) = mpsc::unbounded_channel();
        queue_pair(&tx, cache.clone()).await;
        let stop = CancellationToken::new();
        let task = tokio::spawn(run(cfg, rx, stop.clone()));
        wait_for_start(dir.path()).await;
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "1"
        );
        let pid = std::fs::read_to_string(dir.path().join("pid-1")).unwrap();
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(cache.into_inner().lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cancelled_backoff_does_not_spawn_a_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(fake_ffmpeg(dir.path(), "cancel-backoff"));
        cfg.retry_delay = Duration::from_secs(30);
        let cache = Arc::new(MemoryCacheSource::new());
        let (tx, rx) = mpsc::unbounded_channel();
        queue_pair(&tx, cache).await;
        let stop = CancellationToken::new();
        let task = tokio::spawn(run(cfg, rx, stop.clone()));
        wait_for_start(dir.path()).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "1"
        );
    }

    #[tokio::test]
    async fn repeated_failure_is_bounded_and_preserves_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "always-fail"));
        let cache = Arc::new(MemoryCacheSource::new());
        let (tx, rx) = mpsc::unbounded_channel();
        queue_pair(&tx, cache.clone()).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run(cfg, rx, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            (MAX_RESTARTS + 1).to_string()
        );
        assert_eq!(cache.into_inner().lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn file_output_failure_does_not_restart_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(fake_ffmpeg(dir.path(), "always-fail"));
        cfg.output = dir.path().join("out.ts");
        let (tx, rx) = mpsc::unbounded_channel();
        queue_pair(&tx, Arc::new(MemoryCacheSource::new())).await;
        assert!(run(cfg, rx, CancellationToken::new()).await.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "1"
        );
    }

    #[tokio::test]
    async fn eof_closes_both_inputs_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "normal"));
        let (tx, rx) = mpsc::unbounded_channel();
        let expected = queue_pair(&tx, Arc::new(MemoryCacheSource::new())).await;
        drop(tx);
        tokio::time::timeout(
            Duration::from_secs(10),
            run(cfg, rx, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts")).unwrap(),
            "1"
        );
        assert_eq!(
            std::fs::read(dir.path().join("video1")).unwrap(),
            expected[0]
        );
        assert_eq!(
            std::fs::read(dir.path().join("audio1")).unwrap(),
            expected[1]
        );
    }
    #[tokio::test]
    async fn single_input_output_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(fake_ffmpeg(dir.path(), "single"));
        cfg.has_audio = false;
        let (tx, rx) = mpsc::unbounded_channel();
        let data = b"multiplexed TS input".to_vec();
        tx.send((
            0,
            0,
            segment(0, 0, data.clone(), Arc::new(MemoryCacheSource::new())).await,
        ))
        .unwrap();
        drop(tx);
        tokio::time::timeout(
            Duration::from_secs(5),
            run(cfg, rx, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(std::fs::read(dir.path().join("video1")).unwrap(), data);
    }

    #[tokio::test]
    async fn cancelled_before_start_does_not_spawn_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(fake_ffmpeg(dir.path(), "hang"));
        let (tx, rx) = mpsc::unbounded_channel();
        queue_pair(&tx, Arc::new(MemoryCacheSource::new())).await;
        let stop = CancellationToken::new();
        stop.cancel();
        run(cfg, rx, stop).await.unwrap();
        assert!(!dir.path().join("starts").exists());
    }
}
