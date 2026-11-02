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
    pub(crate) fn create(out_path: &Path) -> Result<Self, String> {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
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
        let file = std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
            tmp,
            final_path: out_path.to_path_buf(),
            committed: false,
        })
    }

    pub(crate) fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .as_mut()
            .expect("writer present until commit/abort")
            .write_all(data)
            .map_err(|e| format!("write {}: {e}", self.final_path.display()))
    }

    pub(crate) fn commit(mut self) -> Result<(), String> {
        let mut w = self.writer.take().expect("writer present");
        w.flush().map_err(|e| format!("flush: {e}"))?;
        drop(w);
        std::fs::rename(&self.tmp, &self.final_path)
            .map_err(|e| format!("rename {}: {e}", self.final_path.display()))?;
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
