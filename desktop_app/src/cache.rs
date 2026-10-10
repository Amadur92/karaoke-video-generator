//! Content fingerprints and recoverable writes used by projects and audio caches.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn file_sha256(path: &Path) -> Result<String, String> {
    file_sha256_cancellable(path, &std::sync::atomic::AtomicBool::new(false))
}

pub fn file_sha256_cancellable(
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Операция отменена".into());
        }
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn bytes_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlignmentIdentity {
    pub schema: u32,
    pub algorithm: String,
    pub audio_sha256: String,
    pub lyrics_sha256: String,
    pub model: String,
    pub plain_lines: bool,
    pub verify_lrc: bool,
    pub separate_vocals: bool,
    pub acoustic_text_verification: bool,
}

impl AlignmentIdentity {
    pub fn new(
        audio: &Path,
        lyrics: &str,
        model: &str,
        plain_lines: bool,
        verify_lrc: bool,
        separate_vocals: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            schema: 1,
            algorithm: "alignment-v2".into(),
            audio_sha256: file_sha256(audio)?,
            lyrics_sha256: bytes_sha256(lyrics.as_bytes()),
            model: model.into(),
            plain_lines,
            verify_lrc,
            separate_vocals,
            acoustic_text_verification: true,
        })
    }

    pub fn matches_timings(&self, timings: &Path) -> bool {
        let Some(stem) = timings.file_stem() else {
            return false;
        };
        let report_path =
            timings.with_file_name(format!("{}_alignment_report.json", stem.to_string_lossy()));
        let result = (|| -> Option<bool> {
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(report_path).ok()?).ok()?;
            let stored: Self =
                serde_json::from_value(report.get("cache_identity")?.clone()).ok()?;
            Some(
                stored == *self
                    && report.get("timings_sha256")?.as_str()? == file_sha256(timings).ok()?,
            )
        })();
        result.unwrap_or(false)
    }
}

pub fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.backup",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

pub fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    if std::fs::rename(temporary, destination).is_ok() {
        return Ok(());
    }
    // Windows cannot rename over an existing file. Preserve it until the new
    // file is installed, and restore on failure instead of deleting it first.
    if !destination.is_file() {
        return Err(format!("Не удалось сохранить {}", destination.display()));
    }
    let backup = backup_path(destination);
    if backup.exists() {
        return Err(format!(
            "Обнаружена резервная копия {}; восстановите её перед заменой",
            backup.display()
        ));
    }
    std::fs::rename(destination, &backup).map_err(|e| e.to_string())?;
    if let Err(error) = std::fs::rename(temporary, destination) {
        let restore = std::fs::rename(&backup, destination);
        return Err(format!(
            "Не удалось сохранить файл: {error}; восстановление: {restore:?}"
        ));
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Файл не имеет папки")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprints_track_content_not_length() {
        assert_ne!(bytes_sha256(b"song A"), bytes_sha256(b"song B"));
        assert_eq!(
            bytes_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
    #[test]
    fn failed_replace_preserves_previous_file() {
        let dir = std::env::temp_dir().join(format!("karaoke-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let destination = dir.join("out.mp4");
        std::fs::write(&destination, b"previous").unwrap();
        assert!(replace_file(&dir.join("missing.mp4"), &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
