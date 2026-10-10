//! Opt-in, entirely local integration test: synthetic fMP4 -> FFmpeg -> local
//! RTMP listener, force-disconnect the listener, accept a new connection, and
//! verify the second recording has decodable video AND audio.
use super::*;
use crate::cache::memory::MemoryCacheSource;
use tokio::io::AsyncWriteExt;

async fn listener(
    ffmpeg: &std::path::Path,
    url: &str,
    output: &std::path::Path,
    log: &std::path::Path,
) -> Child {
    Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostats",
            "-progress",
            "pipe:2",
            "-listen",
            "1",
            "-i",
            url,
            "-c",
            "copy",
            "-y",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(log).unwrap()))
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn wait_for_progress(log: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            if text.lines().any(|line| {
                line.strip_prefix("out_time_us=")
                    .is_some_and(|time| time.parse::<i64>().is_ok_and(|n| n > 1_000_000))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("local RTMP receiver did not receive media");
}

#[tokio::test]
#[ignore = "requires local FFmpeg (libx264/AAC/RTMP), ffprobe; no external network"]
async fn reconnects_to_local_rtmp_after_receiver_disconnect() {
    let ffmpeg = which::which("ffmpeg").unwrap();
    let ffprobe = which::which("ffprobe").unwrap();
    let dir = tempfile::tempdir().unwrap();
    for (name, input, codec) in [
        ("video", "testsrc2=size=320x180:rate=30", "libx264"),
        ("audio", "sine=frequency=440:sample_rate=48000", "aac"),
    ] {
        let mut command = Command::new(&ffmpeg);
        command.current_dir(dir.path()).args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            input,
            "-t",
            "20",
            "-c",
            codec,
        ]);
        if name == "video" {
            command.args(["-g", "30", "-preset", "ultrafast"]);
        }
        let output = command
            .args([
                "-f",
                "hls",
                "-hls_time",
                "1",
                "-hls_list_size",
                "0",
                "-hls_segment_type",
                "fmp4",
                "-hls_fmp4_init_filename",
                &format!("{name}-init.mp4"),
                "-hls_segment_filename",
                &format!("{name}-%03d.m4s"),
                &format!("{name}.m3u8"),
            ])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "fixture encoding failed");
    }

    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let url = format!("rtmp://127.0.0.1:{port}/live/local-test");
    let first_log = dir.path().join("first.log");
    let second_log = dir.path().join("second.log");
    let second_file = dir.path().join("second.mkv");
    let mut first = listener(&ffmpeg, &url, &dir.path().join("first.mkv"), &first_log).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let cache = Arc::new(MemoryCacheSource::new());
    let (tx, rx) = mpsc::unbounded_channel();
    for sequence in 0..20 {
        for (lane, name) in ["video", "audio"].iter().enumerate() {
            let info = SegmentInfo {
                stream_id: lane as u64,
                sequence,
                synchronization_key: Some((0, sequence)),
                stream_type: if lane == 0 {
                    StreamType::Video
                } else {
                    StreamType::Audio
                },
                file_name: format!("{name}-{sequence}.mp4"),
                ..Default::default()
            };
            let mut data = std::fs::read(dir.path().join(format!("{name}-init.mp4"))).unwrap();
            data.extend(
                std::fs::read(dir.path().join(format!("{name}-{sequence:03}.m4s"))).unwrap(),
            );
            let mut writer = cache.open_writer(&info).await.unwrap().unwrap();
            writer.write_all(&data).await.unwrap();
            writer.shutdown().await.unwrap();
            drop(writer);
            tx.send((
                lane as u64,
                sequence,
                CachedSegment::new(info, cache.clone(), false),
            ))
            .unwrap();
        }
    }
    let stop = CancellationToken::new();
    let config = Config {
        program: ffmpeg.clone(),
        output: url.clone().into(),
        extra: None,
        has_audio: true,
        readrate: true,
        catchup: true,
        recycle: true,
        buffer: 3,
        retry_delay: Duration::from_millis(300),
    };
    // Guard cancels the owner even if a test assertion fails.
    let guard = stop.clone().drop_guard();
    let task = tokio::spawn(run(config, rx, stop));
    wait_for_progress(&first_log).await;
    first.kill().await.unwrap();
    let mut second = listener(&ffmpeg, &url, &second_file, &second_log).await;
    wait_for_progress(&second_log).await;
    drop(tx);
    tokio::time::timeout(Duration::from_secs(40), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), second.wait())
        .await
        .unwrap();
    let probe = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "json",
        ])
        .arg(&second_file)
        .output()
        .await
        .unwrap();
    assert!(probe.status.success());
    let data: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let streams = data["streams"].as_array().unwrap();
    assert!(streams.iter().any(|stream| stream["codec_type"] == "video"));
    assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
    let decode = Command::new(ffmpeg)
        .args(["-v", "error", "-xerror", "-i"])
        .arg(&second_file)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
        .output()
        .await
        .unwrap();
    assert!(
        decode.status.success(),
        "reconnected A/V recording did not decode cleanly"
    );
    drop(guard);
}
