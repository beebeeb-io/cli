//! The write-to-tmp-then-rename helper that publishes a download atomically.

use super::*;

/// Write-to-`.tmp`-then-atomic-rename helper so an interrupted/failed download
/// never leaves a corrupt file at `out_path`.
pub(crate) struct AtomicFile {
    pub(super) writer: Option<std::io::BufWriter<std::fs::File>>,
    pub(super) tmp: std::path::PathBuf,
    pub(super) final_path: std::path::PathBuf,
    pub(super) committed: bool,
}

impl AtomicFile {
    pub(crate) fn create(out_path: &Path) -> Result<Self, DownloadError> {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DownloadError::Other(format!("mkdir {}: {e}", parent.display())))?;
            }
        }
        let tmp = out_path.with_extension(format!(
            "{}bbtmp",
            out_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}."))
                .unwrap_or_default()
        ));
        let file =
            std::fs::File::create(&tmp).map_err(|e| DownloadError::Other(format!("create {}: {e}", tmp.display())))?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
            tmp,
            final_path: out_path.to_path_buf(),
            committed: false,
        })
    }

    pub(crate) fn write_all(&mut self, data: &[u8]) -> Result<(), DownloadError> {
        self.writer
            .as_mut()
            .expect("writer present until commit/abort")
            .write_all(data)
            .map_err(|e| DownloadError::Other(format!("write {}: {e}", self.final_path.display())))
    }

    pub(crate) fn commit(mut self) -> Result<(), DownloadError> {
        let mut w = self.writer.take().expect("writer present");
        w.flush().map_err(|e| DownloadError::Other(format!("flush: {e}")))?;
        drop(w);
        std::fs::rename(&self.tmp, &self.final_path)
            .map_err(|e| DownloadError::Other(format!("rename {}: {e}", self.final_path.display())))?;
        self.committed = true;
        Ok(())
    }

    /// Explicitly abort before falling back to another writer for the same path.
    pub(crate) fn abort(&mut self) {
        self.writer.take();
        let _ = std::fs::remove_file(&self.tmp);
        self.committed = true; // suppress Drop cleanup (already handled)
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            self.writer.take();
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-atomic-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A file already at a temp name (a temp left by a crash, or a file of the
    /// user's own) is never opened or truncated, and the download still lands.
    /// Both names this writer has used are taken: `.bbtmp` through 0.17.2 and
    /// core's `.tmp` since.
    #[test]
    fn a_taken_temp_name_is_never_truncated() {
        let dir = scratch("taken-temp");
        let out = dir.join("out.bin");
        for taken in ["out.bin.bbtmp", "out.bin.tmp"] {
            std::fs::write(dir.join(taken), b"precious").unwrap();
        }

        let mut writer = AtomicFile::create(&out).unwrap();
        writer.write_all(b"downloaded").unwrap();
        writer.commit().unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), b"downloaded");
        for taken in ["out.bin.bbtmp", "out.bin.tmp"] {
            assert_eq!(
                std::fs::read(dir.join(taken)).ok().as_deref(),
                Some(&b"precious"[..]),
                "{taken} was opened: truncated, written or moved"
            );
        }
        assert_eq!(
            names(&dir),
            ["out.bin", "out.bin.bbtmp", "out.bin.tmp"],
            "a temp file was left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A download that is dropped or aborted before its commit removes its own
    /// temp and leaves the existing output as it was.
    #[test]
    fn an_unfinished_download_changes_nothing() {
        let dir = scratch("unfinished");
        let out = dir.join("out.bin");
        std::fs::write(&out, b"old").unwrap();

        let mut dropped = AtomicFile::create(&out).unwrap();
        dropped.write_all(b"half").unwrap();
        drop(dropped);
        assert_eq!(std::fs::read(&out).unwrap(), b"old");
        assert_eq!(names(&dir), ["out.bin"], "the dropped download's temp was left behind");

        let mut aborted = AtomicFile::create(&out).unwrap();
        aborted.write_all(b"half").unwrap();
        aborted.abort();
        drop(aborted);
        assert_eq!(std::fs::read(&out).unwrap(), b"old");
        assert_eq!(names(&dir), ["out.bin"], "the aborted download's temp was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 2408: a download never writes through an 8.3 short name onto a
    /// different existing file. The output name reaches `Report for 2026.pdf`;
    /// the write is refused, naming both, and that file keeps its bytes.
    #[cfg(windows)]
    #[test]
    fn never_writes_through_a_short_name() {
        use crate::short_name_test_support::{Scratch, aliased_file, entries};
        let scratch = Scratch::new("atomic");
        let dir = scratch.path();
        let Some(alias) = aliased_file(dir, "Report for 2026.pdf", b"precious") else {
            return;
        };

        let result = AtomicFile::create(&dir.join(&alias)).and_then(|mut writer| {
            writer.write_all(b"downloaded")?;
            writer.commit()
        });

        let message = result
            .expect_err("a write through a short name must be refused")
            .to_string();
        assert!(
            message.contains(&alias) && message.contains("Report for 2026.pdf"),
            "the refusal names both files: {message}"
        );
        assert_eq!(std::fs::read(dir.join("Report for 2026.pdf")).unwrap(), b"precious");
        assert_eq!(entries(dir), ["Report for 2026.pdf"]);
    }
}
