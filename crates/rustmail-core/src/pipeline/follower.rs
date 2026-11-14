//! Incremental log reading that survives restarts and log rotation.

use std::io::SeekFrom;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::store::LogPosition;

/// Upper bound on how much is read in one go.
pub const MAX_BATCH_BYTES: usize = 1024 * 1024;

/// Complete lines read from a log, and where reading should continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Complete lines, without their line terminators.
    pub lines: Vec<String>,
    /// Position after the last complete line.
    pub position: LogPosition,
    /// Whether the file was replaced or truncated since `start`.
    pub rotated: bool,
}

/// Where to start reading a file that has not been read before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// From the first byte: ingest the history.
    Beginning,
    /// From the current end: only new lines.
    End,
}

/// Reads the next batch of complete lines from `path`.
///
/// `start` is where the previous call stopped. If the file's inode changed or
/// it became shorter, it was rotated or truncated and reading restarts at the top
/// of the new file. A trailing partial line is left for the next call. Returns
/// `Ok(None)` if the file does not exist yet.
///
/// # Errors
/// Fails on I/O errors other than the file being absent.
pub async fn read_batch(
    path: &Path,
    start: Option<LogPosition>,
    origin: Origin,
    max_bytes: usize,
) -> std::io::Result<Option<Batch>> {
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let meta = file.metadata().await?;
    let (inode, size) = (meta.ino(), meta.len());

    let (offset, rotated) = match start {
        Some(pos) if pos.inode == inode && pos.offset <= size => (pos.offset, false),
        Some(_) => (0, true),
        None => (
            match origin {
                Origin::Beginning => 0,
                Origin::End => size,
            },
            false,
        ),
    };

    file.seek(SeekFrom::Start(offset)).await?;
    let mut buffer = Vec::new();
    file.take(max_bytes as u64).read_to_end(&mut buffer).await?;

    // Keep only whole lines. If the chunk is full and holds no newline at all the
    // line is longer than we will ever parse: skip it rather than stall forever.
    let consumed = match buffer.iter().rposition(|&b| b == b'\n') {
        Some(last) => last + 1,
        None if buffer.len() >= max_bytes => buffer.len(),
        None => 0,
    };
    let lines = buffer[..consumed]
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            String::from_utf8_lossy(line)
                .trim_end_matches('\r')
                .to_owned()
        })
        .collect();
    Ok(Some(Batch {
        lines,
        position: LogPosition {
            inode,
            offset: offset + consumed as u64,
        },
        rotated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_log() -> (tempdir::Dir, std::path::PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.path().join("mail.log");
        (dir, path)
    }

    /// A tiny temp-dir helper so the tests need no extra crate.
    mod tempdir {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                // A counter, not just the clock: tests run in parallel and two of
                // them can read the same nanosecond.
                static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let unique = format!(
                    "rustmail-follower-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                );
                let path = std::env::temp_dir().join(unique);
                std::fs::create_dir_all(&path).expect("create temp dir");
                Self(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        file.write_all(text.as_bytes()).expect("write");
    }

    #[tokio::test]
    async fn a_missing_file_is_not_an_error() {
        let (_dir, path) = temp_log();
        let batch = read_batch(&path, None, Origin::Beginning, 1024).await;
        assert!(matches!(batch, Ok(None)));
    }

    #[tokio::test]
    async fn reads_whole_lines_and_leaves_a_partial_one() {
        let (_dir, path) = temp_log();
        append(&path, "one\ntwo\npart");
        let first = read_batch(&path, None, Origin::Beginning, 1024)
            .await
            .expect("io")
            .expect("file");
        assert_eq!(first.lines, ["one", "two"]);
        assert_eq!(first.position.offset, 8);

        append(&path, "ial\nthree\n");
        let second = read_batch(&path, Some(first.position), Origin::Beginning, 1024)
            .await
            .expect("io")
            .expect("file");
        assert_eq!(second.lines, ["partial", "three"]);
        assert!(!second.rotated);
    }

    #[tokio::test]
    async fn starting_at_the_end_skips_history() {
        let (_dir, path) = temp_log();
        append(&path, "old\n");
        let batch = read_batch(&path, None, Origin::End, 1024)
            .await
            .expect("io")
            .expect("file");
        assert!(batch.lines.is_empty());
        append(&path, "new\n");
        let next = read_batch(&path, Some(batch.position), Origin::End, 1024)
            .await
            .expect("io")
            .expect("file");
        assert_eq!(next.lines, ["new"]);
    }

    #[tokio::test]
    async fn a_truncated_or_replaced_file_restarts_from_the_top() {
        let (_dir, path) = temp_log();
        append(&path, "aaaa\nbbbb\n");
        let first = read_batch(&path, None, Origin::Beginning, 1024)
            .await
            .expect("io")
            .expect("file");
        // Rotation: the old file is moved away and a new, shorter one appears.
        std::fs::rename(&path, path.with_extension("1")).expect("rename");
        append(&path, "c\n");
        let next = read_batch(&path, Some(first.position), Origin::Beginning, 1024)
            .await
            .expect("io")
            .expect("file");
        assert!(next.rotated);
        assert_eq!(next.lines, ["c"]);
    }

    #[tokio::test]
    async fn an_endless_line_is_skipped_instead_of_stalling() {
        let (_dir, path) = temp_log();
        append(&path, &"x".repeat(100));
        append(&path, "\nok\n");
        let first = read_batch(&path, None, Origin::Beginning, 50)
            .await
            .expect("io")
            .expect("file");
        assert!(first.lines.len() <= 1, "no panic, bounded output");
        assert!(first.position.offset > 0, "made progress");
    }

    #[tokio::test]
    async fn carriage_returns_are_stripped() {
        let (_dir, path) = temp_log();
        append(&path, "a\r\nb\r\n");
        let batch = read_batch(&path, None, Origin::Beginning, 1024)
            .await
            .expect("io")
            .expect("file");
        assert_eq!(batch.lines, ["a", "b"]);
    }
}
