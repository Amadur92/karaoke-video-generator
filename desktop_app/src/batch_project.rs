use crate::{AppSettings, BatchItem, BatchStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct Project {
    schema: u32,
    pub root: Option<PathBuf>,
    pub items: Vec<BatchItem>,
    pub settings: AppSettings,
}
impl Project {
    pub fn new(root: Option<PathBuf>, items: Vec<BatchItem>, settings: AppSettings) -> Self {
        Self {
            schema: 1,
            root,
            items,
            settings,
        }
    }
    pub fn read(path: &Path) -> Result<Self, String> {
        let mut project: Self = serde_json::from_slice(
            &std::fs::read(path)
                .or_else(|_| std::fs::read(desktop_app::cache::backup_path(path)))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if project.schema != 1 {
            return Err("Неподдерживаемая версия проекта".into());
        }
        for item in &mut project.items {
            if item.duration_ms < 1000 {
                return Err(format!(
                    "Некорректная длительность трека в проекте: {}",
                    item.title
                ));
            }
            item.trim_start_ms = item.trim_start_ms.clamp(0, item.duration_ms - 1000);
            item.trim_end_ms = item
                .trim_end_ms
                .clamp(item.trim_start_ms + 1000, item.duration_ms);
            let max_fade = ((item.trim_end_ms - item.trim_start_ms) / 2).min(30_000) as i32;
            item.fade_in_ms = item.fade_in_ms.clamp(0, max_fade);
            item.fade_out_ms = item.fade_out_ms.clamp(0, max_fade);
            if matches!(
                item.status,
                BatchStatus::Aligning | BatchStatus::Rendering | BatchStatus::ReadyToRender
            ) || (item.status == BatchStatus::Done
                && !item
                    .output_path
                    .as_ref()
                    .is_some_and(|path| Path::new(path).is_file()))
            {
                item.status = BatchStatus::Ready;
                item.progress = 0.0;
            }
        }
        Ok(project)
    }
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec_pretty(self).map_err(|e| e.to_string())
    }
}
pub fn autosave_path() -> PathBuf {
    crate::paths::app_data_dir().join("last-batch-project.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_trim_and_settings_without_resuming_running_processes() {
        let path =
            std::env::temp_dir().join(format!("karaoke-project-{}.json", std::process::id()));
        let item = BatchItem {
            folder: "/songs".into(),
            audio_path: "/songs/audio.wav".into(),
            lyrics_path: "/songs/lyrics.lrc".into(),
            lyrics_override: Some("[00:01.00]Edited lyrics".into()),
            artist: "Artist".into(),
            title: "Title".into(),
            duration_ms: 3000,
            trim_start_ms: 250,
            trim_end_ms: 2750,
            fade_in_ms: 150,
            fade_out_ms: 250,
            status: BatchStatus::Rendering,
            progress: 0.5,
            output_path: None,
            alignment_quality: None,
            audit: None,
            waveform: Some(vec![0.5; 2048]),
        };
        let settings = AppSettings {
            loudness_target_db: 92,
            ..Default::default()
        };
        let project = Project::new(Some("/songs".into()), vec![item], settings);
        desktop_app::cache::atomic_write(&path, &project.bytes().unwrap()).unwrap();
        let restored = Project::read(&path).unwrap();
        assert_eq!(restored.items[0].status, BatchStatus::Ready);
        assert_eq!(restored.items[0].trim_start_ms, 250);
        assert_eq!(restored.items[0].fade_out_ms, 250);
        assert_eq!(
            restored.items[0].lyrics_override.as_deref(),
            Some("[00:01.00]Edited lyrics")
        );
        assert_eq!(restored.settings.loudness_target_db, 92);
        assert!(restored.items[0].waveform.is_none());
        std::fs::remove_file(path).unwrap();
    }
}
