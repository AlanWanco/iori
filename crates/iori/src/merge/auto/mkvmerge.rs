use crate::{
    IoriResult, SegmentFormat, SegmentInfo,
    cache::CacheSource,
    merge::{AutoMergerConcat, AutoMergerMerge},
};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct MkvmergeMerger(PathBuf);

impl MkvmergeMerger {
    pub fn new() -> IoriResult<Self> {
        let mkvmerge = which::which("mkvmerge")?;
        Ok(Self(mkvmerge))
    }
}

fn check_mkvmerge_status(status: std::process::ExitStatus, operation: &str) -> IoriResult<()> {
    match status.code() {
        Some(0) => Ok(()),
        // mkvmerge uses exit status 1 for warnings. It can still produce a
        // usable output file, so only status 2 (or an interrupted process) is
        // a hard failure.
        Some(1) => {
            tracing::warn!("mkvmerge {operation} completed with warnings (status 1)");
            Ok(())
        }
        Some(code) => Err(std::io::Error::other(format!(
            "mkvmerge {operation} failed with exit status {code}"
        ))
        .into()),
        None => Err(std::io::Error::other(format!(
            "mkvmerge {operation} terminated without an exit status: {status}"
        ))
        .into()),
    }
}

async fn remove_temporary_track(path: &Path) -> IoriResult<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    #[cfg(unix)]
    {
        // Some macOS filesystems expose equivalent NFC/NFD names differently:
        // a path can be opened successfully but fail to be removed when its
        // Unicode spelling differs from the directory entry. Resolve the
        // actual entry by device/inode before giving up.
        let metadata = match tokio::fs::metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut entries = match tokio::fs::read_dir(parent).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };

        while let Some(entry) = entries.next_entry().await? {
            let entry_metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if entry_metadata.dev() == metadata.dev() && entry_metadata.ino() == metadata.ino() {
                match tokio::fs::remove_file(entry.path()).await {
                    Ok(()) => {
                        tracing::debug!(
                            "Removed temporary merge track through filesystem alias: {}",
                            entry.path().display()
                        );
                        return Ok(());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    tracing::debug!("Temporary merge track already removed: {}", path.display());
    Ok(())
}

const MAX_MKVMERGE_INPUTS: usize = 128;

async fn run_mkvmerge_concat(
    mkvmerge: &Path,
    inputs: &[PathBuf],
    output_path: &Path,
) -> IoriResult<()> {
    let mut args = vec!["-q".to_string(), "[".to_string()];
    for input in inputs {
        args.push(input.to_string_lossy().to_string());
    }
    args.push("]".to_string());
    args.push("-o".to_string());
    args.push(output_path.to_string_lossy().to_string());

    let mut temp = tempfile::Builder::new().tempfile()?;
    let temp_path = temp.path().to_path_buf();
    temp.write_all(serde_json::to_string(&args)?.as_bytes())?;
    temp.flush()?;

    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = Command::new(mkvmerge)
        .arg(format!("@{}", temp_path.to_string_lossy()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(stdout) = child.stdout.take() {
        let stdout_reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut lines = stdout_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!("[mkvmerge] {}", line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let stderr_reader = BufReader::new(stderr);
        tokio::spawn(async move {
            let mut lines = stderr_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("[mkvmerge] {}", line);
            }
        });
    }

    let status = child.wait().await?;
    check_mkvmerge_status(status, "concatenation")?;

    if !output_path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "mkvmerge concatenation did not create {}",
                output_path.display()
            ),
        )
        .into());
    }

    Ok(())
}

impl AutoMergerConcat for MkvmergeMerger {
    fn format(&self) -> SegmentFormat {
        SegmentFormat::Other("mkv".to_string())
    }

    async fn concat<O>(
        &mut self,
        segments: &[&SegmentInfo],
        cache: &impl CacheSource,
        output_path: O,
    ) -> IoriResult<()>
    where
        O: AsRef<Path> + Send,
    {
        tracing::debug!("Concatenating with mkvmerge...");

        let output_path = output_path.as_ref().to_owned();
        let mut inputs = Vec::with_capacity(segments.len());
        for segment in segments {
            let filename = cache.segment_path(segment).await.ok_or_else(|| {
                std::io::Error::other(format!(
                    "No cache path for stream {} sequence {}",
                    segment.stream_id, segment.sequence
                ))
            })?;
            inputs.push(filename);
        }
        if inputs.is_empty() {
            return Err(std::io::Error::other("Cannot concatenate an empty segment list").into());
        }

        if let Some(parent) = output_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        if inputs.len() <= MAX_MKVMERGE_INPUTS {
            return run_mkvmerge_concat(&self.0, &inputs, &output_path).await;
        }

        tracing::info!(
            "Concatenating {} segments in batches of {} to avoid file descriptor limits.",
            inputs.len(),
            MAX_MKVMERGE_INPUTS
        );
        let parent = output_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temp_dir = tempfile::Builder::new()
            .prefix(".shiori-mkvmerge-")
            .tempdir_in(parent)?;
        let mut chunks = Vec::with_capacity(inputs.len().div_ceil(MAX_MKVMERGE_INPUTS));

        for (index, input_chunk) in inputs.chunks(MAX_MKVMERGE_INPUTS).enumerate() {
            let chunk_path = temp_dir.path().join(format!("chunk-{index:04}.mkv"));
            run_mkvmerge_concat(&self.0, input_chunk, &chunk_path).await?;
            chunks.push(chunk_path);
        }

        run_mkvmerge_concat(&self.0, &chunks, &output_path).await
    }
}

impl AutoMergerMerge for MkvmergeMerger {
    fn format(&self) -> SegmentFormat {
        SegmentFormat::Other("mkv".to_string())
    }

    async fn merge<O>(&mut self, tracks: Vec<PathBuf>, output: O) -> IoriResult<()>
    where
        O: AsRef<Path> + Send,
    {
        use tokio::io::{AsyncBufReadExt, BufReader};

        assert!(tracks.len() > 1);

        let mkvmerge = which::which("mkvmerge")?;
        let mut merge = Command::new(mkvmerge)
            .args(tracks.iter())
            .arg("-o")
            .arg(output.as_ref().with_extension("mkv"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        // Capture and log stdout
        if let Some(stdout) = merge.stdout.take() {
            let stdout_reader = BufReader::new(stdout);
            tokio::spawn(async move {
                let mut lines = stdout_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("[mkvmerge] {}", line);
                }
            });
        }

        // Capture and log stderr
        if let Some(stderr) = merge.stderr.take() {
            let stderr_reader = BufReader::new(stderr);
            tokio::spawn(async move {
                let mut lines = stderr_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!("[mkvmerge] {}", line);
                }
            });
        }

        let status = merge.wait().await?;
        check_mkvmerge_status(status, "stream merge")?;

        // Remove temporary files, including files whose Unicode spelling is
        // normalized differently by the underlying filesystem.
        for track in tracks {
            remove_temporary_track(&track).await?;
        }

        Ok(())
    }
}
