#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Скрывает консоль на Windows в релиз-сборке

mod batch_pipeline;
mod batch_project;
mod ffmpeg;
mod lyrics;
mod paths;
mod pptx_optimizer;
mod workspace_ui;

use desktop_app::{audio_loudness, audio_preview};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

// Шрифты Montserrat вкомпилированы прямо в бинарник — нулевая зависимость от внешних файлов
const MONTSERRAT_REGULAR: &[u8] = include_bytes!("../assets/Montserrat-Regular.ttf");
const MONTSERRAT_BOLD: &[u8] = include_bytes!("../assets/Montserrat-Bold.ttf");
const MONTSERRAT_BLACK: &[u8] = include_bytes!("../assets/Montserrat-Black.ttf");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

fn app_icon() -> egui::IconData {
    let size = 256usize;
    let mut rgba = vec![0u8; size * size * 4];
    let center = size as f32 / 2.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let idx = (y * size + x) * 4;

            if distance <= 116.0 {
                let t = (y as f32 / size as f32).clamp(0.0, 1.0);
                rgba[idx] = (24.0 + 18.0 * t) as u8;
                rgba[idx + 1] = (116.0 + 95.0 * (1.0 - t)) as u8;
                rgba[idx + 2] = (255.0 - 74.0 * t) as u8;
                rgba[idx + 3] = 255;
            }

            if distance > 100.0 && distance <= 116.0 {
                rgba[idx] = 54;
                rgba[idx + 1] = 211;
                rgba[idx + 2] = 153;
                rgba[idx + 3] = 255;
            }
        }
    }

    fn fill_rect(
        rgba: &mut [u8],
        size: usize,
        left: usize,
        top: usize,
        width: usize,
        height: usize,
        color: [u8; 4],
    ) {
        for y in top..(top + height).min(size) {
            for x in left..(left + width).min(size) {
                let idx = (y * size + x) * 4;
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }

    fill_rect(&mut rgba, size, 79, 98, 24, 66, [255, 255, 255, 255]);
    fill_rect(&mut rgba, size, 112, 76, 24, 88, [255, 255, 255, 255]);
    fill_rect(&mut rgba, size, 145, 116, 24, 48, [255, 255, 255, 255]);
    fill_rect(&mut rgba, size, 73, 174, 102, 12, [255, 255, 255, 255]);

    let play = [(112.0, 100.0), (112.0, 156.0), (160.0, 128.0)];
    for y in 92..164 {
        for x in 104..170 {
            let px = x as f32;
            let py = y as f32;
            let area = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
                ((a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1)).abs()) / 2.0
            };
            let whole = area(play[0], play[1], play[2]);
            let a = area((px, py), play[1], play[2]);
            let b = area(play[0], (px, py), play[2]);
            let c = area(play[0], play[1], (px, py));
            if (a + b + c - whole).abs() < 0.6 {
                let idx = (y * size + x) * 4;
                rgba[idx..idx + 4].copy_from_slice(&[12, 14, 18, 255]);
            }
        }
    }

    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}

fn open_in_explorer(path: &std::path::Path) {
    let path_to_open = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(path_to_open)
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(path_to_open)
            .status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(path_to_open)
            .status();
    }
}

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).status();
    }
}

fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(url)
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).status();
    }
}

fn scan_batch_folder(
    root: &Path,
    fade_in_ms: i32,
    fade_out_ms: i32,
    plain_lines: bool,
) -> (Vec<BatchItem>, Vec<String>) {
    let mut folders = Vec::new();
    let mut warnings = Vec::new();

    if root.is_dir() {
        folders.push(root.to_path_buf());
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                }
            }
        }
    }
    folders.sort_by_key(|path| lyrics::folder_sort_key(path));

    let mut items = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            warnings.push(format!(
                "Не удалось прочитать папку {}",
                lyrics::display_file_name(&folder)
            ));
            continue;
        };
        let files: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        let mut audio_files: Vec<PathBuf> = files
            .iter()
            .filter(|path| lyrics::is_audio_file(path))
            .cloned()
            .collect();
        audio_files.sort_by_key(|path| lyrics::display_file_name(path).to_lowercase());
        if audio_files.is_empty() {
            continue;
        }

        for audio_path in audio_files {
            let duration_ms = match ffmpeg::probe_audio_duration_ms(&audio_path.to_string_lossy()) {
                Ok(duration) => duration,
                Err(err) => {
                    warnings.push(format!(
                        "{}: не удалось прочитать длительность ({})",
                        lyrics::display_file_name(&audio_path),
                        err
                    ));
                    continue;
                }
            };
            let stem = audio_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let (artist, title) = lyrics::parse_artist_title_from_stem(&stem);
            let (lyrics_path, status) = match lyrics::find_matching_lyrics(&audio_path, &files) {
                Some(path) => (path, BatchStatus::Ready),
                None => {
                    warnings.push(format!(
                        "{}: найдено аудио, но нет .lrc/.txt",
                        lyrics::display_file_name(&folder)
                    ));
                    (PathBuf::new(), BatchStatus::MissingLyrics)
                }
            };
            let mut item = BatchItem {
                folder: folder.clone(),
                audio_path,
                lyrics_path,
                lyrics_override: None,
                artist,
                title,
                duration_ms,
                trim_start_ms: 0,
                trim_end_ms: duration_ms,
                fade_in_ms,
                fade_out_ms,
                status,
                progress: 0.0,
                output_path: None,
                alignment_quality: None,
                audit: None,
                waveform: None,
            };
            refresh_batch_item_audit(&mut item, plain_lines);
            items.push(item);
        }
    }

    (items, warnings)
}

fn alignment_report_path_for_timings(timings_path: &Path) -> PathBuf {
    let stem = timings_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    timings_path.with_file_name(format!("{}_alignment_report.json", stem))
}

fn suspicious_source_markers(value: &str) -> Vec<&'static str> {
    let lower = value.to_lowercase();
    let markers = [
        ("live", "live"),
        ("concert", "concert"),
        ("remix", "remix"),
        ("cover", "cover"),
        ("karaoke", "karaoke"),
        ("instrumental", "instrumental"),
        ("slowed", "slowed"),
        ("speed up", "speed up"),
        ("sped up", "sped up"),
        ("nightcore", "nightcore"),
        ("version", "version"),
        ("версия", "версия"),
        ("концерт", "концерт"),
        ("кавер", "кавер"),
        ("ремикс", "ремикс"),
    ];
    markers
        .iter()
        .filter_map(|(needle, label)| lower.contains(needle).then_some(*label))
        .collect()
}

fn refresh_batch_item_audit(item: &mut BatchItem, plain_lines: bool) {
    let audit = PackageAudit::from_item(item, plain_lines);
    if let Some(path) = audit.detected_output_path.clone() {
        item.output_path = Some(path.to_string_lossy().to_string());
        if item.status == BatchStatus::Ready && audit.level != AuditLevel::Error {
            item.status = BatchStatus::Done;
        }
    }
    if item.alignment_quality.is_none() {
        item.alignment_quality = audit.detected_alignment.clone();
    }
    item.audit = Some(audit);
}

fn safe_output_filename(value: &str) -> String {
    value.replace("/", "_").replace("\\", "_")
}

fn batch_timings_path(audio_path: &Path) -> PathBuf {
    let stem = audio_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    audio_path.with_file_name(format!("{}_timings.json", stem))
}

fn clamp_trim_window(start: i64, end: i64, duration: i64) -> (i64, i64) {
    let duration = duration.max(0);
    let gap = duration.min(1000);
    let end = end.clamp(gap, duration);
    (start.clamp(0, end - gap), end)
}

fn batch_item_trim_window(item: &BatchItem) -> (i64, i64, bool) {
    let start = item
        .trim_start_ms
        .clamp(0, item.duration_ms.saturating_sub(1000));
    let end = item.trim_end_ms.clamp(start + 1000, item.duration_ms);
    let should_trim =
        start > 0 || end < item.duration_ms || item.fade_in_ms > 0 || item.fade_out_ms > 0;
    (start, end, should_trim)
}

fn batch_timings_path_for_item(item: &BatchItem) -> PathBuf {
    let (_, _, should_trim) = batch_item_trim_window(item);
    if !should_trim {
        return batch_timings_path(&item.audio_path);
    }

    let stem = item
        .audio_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let (start, end, _) = batch_item_trim_window(item);
    item.audio_path.with_file_name(format!(
        "{}_trim_{}-{}_fi{}_fo{}_timings.json",
        stem,
        start.max(0),
        end.max(start + 1000),
        item.fade_in_ms.max(0),
        item.fade_out_ms.max(0)
    ))
}

fn karaoke_output_suffix(plain_lines: bool) -> &'static str {
    if plain_lines {
        "karaoke-lines"
    } else {
        "karaoke-word"
    }
}

fn karaoke_output_filename(artist: &str, title: &str, plain_lines: bool) -> String {
    safe_output_filename(&format!(
        "{} - {} ({}).mp4",
        artist,
        title,
        karaoke_output_suffix(plain_lines)
    ))
}

fn font_display_name(_font: &str) -> &'static str {
    "Montserrat"
}

fn audio_source_label(source: &str) -> &str {
    match source {
        "youtube" => "YouTube",
        "soundcloud" => "SoundCloud",
        "deezer-isrc" => "Deezer",
        "qobuz-isrc" => "Qobuz",
        "jiosaavn" => "JioSaavn",
        "songlink" => "Songlink",
        other => other,
    }
}

fn format_audio_duration(duration_sec: Option<i64>) -> String {
    duration_sec
        .map(|seconds| format!("{}:{:02}", seconds / 60, seconds % 60))
        .unwrap_or_else(|| "—".to_string())
}

fn quality_display_name(quality: &str) -> &'static str {
    match quality {
        "high" => "Высокое · 1352×224 · 256 кбит/с",
        "ultra" => "Мастер · 2704×448 · 320 кбит/с",
        _ => "Стандарт · 1352×224 · 192 кбит/с",
    }
}

fn split_panel_widths(total_width: f32, gap: f32, left_ratio: f32) -> (f32, f32) {
    let usable = (total_width - gap.max(0.0)).max(0.0);
    let left = usable * left_ratio.clamp(0.0, 1.0);
    (left, usable - left)
}

fn normalized_font_choice(_font: &str) -> String {
    "montserrat".to_string()
}

fn batch_output_path(item: &BatchItem, plain_lines: bool) -> PathBuf {
    let artist = if item.artist.trim().is_empty() {
        "Исполнитель"
    } else {
        item.artist.trim()
    };
    let title = if item.title.trim().is_empty() {
        "Песня"
    } else {
        item.title.trim()
    };
    item.folder
        .join(karaoke_output_filename(artist, title, plain_lines))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
struct AppSettings {
    model: String,
    quality: String,
    font: String,
    font_scale: f32,
    color_active: [u8; 3],
    color_inactive: [u8; 3],
    color_bg: [u8; 3],
    inactive_opacity: f32,
    audio_delay_ms: i32,
    normalize_audio: bool,
    loudness_target_db: i32,
    fade_in_ms: i32,
    fade_out_ms: i32,
    artist: String,
    title: String,
    lyrics: String,
    plain_lines: bool,
    verify_lrc_with_whisper: bool,
    separate_vocals_for_word_level: bool,
    batch_overwrite_timings: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model: "base".to_string(),
            quality: "medium".to_string(),
            font: "montserrat".to_string(),
            font_scale: 1.0,
            color_active: [0, 0, 0],
            color_inactive: [180, 185, 195],
            color_bg: [255, 255, 255],
            inactive_opacity: 0.65,
            audio_delay_ms: 0,
            normalize_audio: true,
            loudness_target_db: audio_loudness::DEFAULT_TARGET_DB,
            fade_in_ms: 0,
            fade_out_ms: 0,
            artist: String::new(),
            title: String::new(),
            lyrics: String::new(),
            plain_lines: false,
            verify_lrc_with_whisper: true,
            separate_vocals_for_word_level: false,
            batch_overwrite_timings: false,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
struct CLIProgress {
    progress: f32,
    status: String,
    #[allow(dead_code)]
    done: bool,
    error: Option<String>,
    file: Option<String>,
    #[serde(default)]
    batch_align_index: Option<usize>,
    #[serde(default)]
    alignment_report_file: Option<String>,
    #[serde(default)]
    alignment_score: Option<f32>,
    #[serde(default)]
    alignment_summary: Option<String>,
}

enum ProgressUpdate {
    Progress(CLIProgress),
    BatchProgress {
        index: usize,
        status: BatchStatus,
        progress: f32,
        message: String,
        output_path: Option<String>,
        alignment: Option<AlignmentQuality>,
    },
    RawLog(String),
    Error(String),
    VideoTrimmed(String),
    VideoTrimError(String),
    Finished(bool),
    BatchFinished,
}

enum PptxOptimizeUpdate {
    Log(String),
    Plan {
        total: usize,
        remaining: usize,
    },
    Done {
        index: usize,
        total: usize,
        message: String,
    },
    Summary {
        output_mb: f32,
        mp4_mb: f32,
        mp4_count: usize,
        bad: Option<String>,
    },
    Finished(Result<(), String>),
}

struct VideoFrame {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

struct PreparedAudioPreview {
    source: audio_preview::WavSource,
    start_ms: i64,
    end_ms: i64,
    message: String,
}
struct PreparedVideoPreview {
    path: String,
    asset: audio_preview::Asset,
    width: usize,
    height: usize,
    start_ms: i64,
}
struct AudioLoadUpdate {
    path: String,
    result: Result<i64, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum BatchStatus {
    Ready,
    MissingLyrics,
    Aligning,
    ReadyToRender,
    Rendering,
    Done,
    Error(String),
}

#[derive(Clone, Debug)]
struct AlignmentQuality {
    summary: String,
    score: Option<f32>,
    report_path: Option<String>,
}

impl AlignmentQuality {
    fn from_progress(progress: &CLIProgress) -> Option<Self> {
        let summary = progress
            .alignment_summary
            .clone()
            .or_else(|| progress.alignment_score.map(summary_from_score))?;
        Some(Self {
            summary,
            score: progress.alignment_score,
            report_path: progress.alignment_report_file.clone(),
        })
    }

    fn label(&self) -> &'static str {
        match self.summary.as_str() {
            "ok" => "OK",
            "unverified" => "не проверено по аудио",
            "minor_warnings" => "есть замечания",
            "suspicious" => "проверить",
            "needs_repair" => "нужна правка",
            _ => "качество",
        }
    }

    fn color(&self) -> egui::Color32 {
        match self.summary.as_str() {
            "ok" => egui::Color32::from_rgb(54, 211, 153),
            "unverified" => egui::Color32::from_rgb(246, 181, 70),
            "minor_warnings" => egui::Color32::from_rgb(246, 181, 70),
            "suspicious" => egui::Color32::from_rgb(255, 137, 86),
            "needs_repair" => egui::Color32::from_rgb(255, 100, 100),
            _ => egui::Color32::from_rgb(142, 153, 175),
        }
    }

    fn icon(&self) -> &'static str {
        match self.summary.as_str() {
            "ok" => "✓",
            "unverified" => "?",
            "minor_warnings" => "!",
            "suspicious" => "?",
            "needs_repair" => "!",
            _ => "i",
        }
    }

    fn tooltip(&self) -> String {
        let mut text = format!("Качество таймингов: {}", self.label());
        if let Some(score) = self.score {
            text.push_str(&format!("\nScore: {:.2}", score));
        }
        if let Some(path) = &self.report_path {
            text.push_str(&format!("\nОтчёт: {}", path));
        }
        text
    }

    fn log_text(&self) -> String {
        let mut text = format!("Качество таймингов: {}", self.label());
        if let Some(score) = self.score {
            text.push_str(&format!(" ({:.2})", score));
        }
        if let Some(path) = &self.report_path {
            text.push_str(&format!(" — {}", path));
        }
        text
    }
}

fn summary_from_score(score: f32) -> String {
    if score >= 0.98 {
        "ok"
    } else if score >= 0.85 {
        "minor_warnings"
    } else if score >= 0.55 {
        "suspicious"
    } else {
        "needs_repair"
    }
    .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuditLevel {
    Ok,
    Notice,
    Warning,
    Error,
}

impl AuditLevel {
    fn color(self) -> egui::Color32 {
        match self {
            AuditLevel::Ok => egui::Color32::from_rgb(54, 211, 153),
            AuditLevel::Notice => egui::Color32::from_rgb(142, 153, 175),
            AuditLevel::Warning => egui::Color32::from_rgb(246, 181, 70),
            AuditLevel::Error => egui::Color32::from_rgb(255, 100, 100),
        }
    }

    fn label(self) -> &'static str {
        match self {
            AuditLevel::Ok => "OK",
            AuditLevel::Notice => "не готово",
            AuditLevel::Warning => "проверить",
            AuditLevel::Error => "ошибка",
        }
    }
}

#[derive(Clone, Debug)]
struct PackageAudit {
    level: AuditLevel,
    summary: String,
    issues: Vec<String>,
    detected_output_path: Option<PathBuf>,
    detected_alignment: Option<AlignmentQuality>,
}

impl PackageAudit {
    fn from_item(item: &BatchItem, plain_lines: bool) -> Self {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut notices = Vec::new();
        let mut detected_output_path = None;
        let mut detected_alignment = None;

        if let Some(text) = &item.lyrics_override {
            if text.trim().is_empty() {
                errors.push("нет текста песни".to_string());
            }
        } else if item.status == BatchStatus::MissingLyrics
            || item.lyrics_path.as_os_str().is_empty()
        {
            errors.push("нет файла текста .txt/.lrc".to_string());
        } else if let Ok(meta) = std::fs::metadata(&item.lyrics_path)
            && meta.len() < 16
        {
            warnings.push("файл текста выглядит пустым".to_string());
        }

        if item.duration_ms < 45_000 {
            warnings.push(format!(
                "очень короткое аудио: {}",
                ffmpeg::format_time_ms(item.duration_ms)
            ));
        } else if item.duration_ms > 9 * 60_000 {
            warnings.push(format!(
                "очень длинное аудио: {}",
                ffmpeg::format_time_ms(item.duration_ms)
            ));
        }

        let file_name = lyrics::display_file_name(&item.audio_path);
        let markers = suspicious_source_markers(&file_name);
        if !markers.is_empty() {
            warnings.push(format!(
                "подозрительные слова в имени аудио: {}",
                markers.join(", ")
            ));
        }

        let timings_path = batch_timings_path_for_item(item);
        let report_path = alignment_report_path_for_timings(&timings_path);
        if report_path.exists() {
            match read_alignment_quality_from_report(&report_path) {
                Some(quality) => {
                    match quality.summary.as_str() {
                        "needs_repair" => errors.push("отчёт таймингов: нужна правка".to_string()),
                        "suspicious" => warnings.push("отчёт таймингов: проверить".to_string()),
                        "unverified" => warnings.push(
                            "тайминги проверены только структурно, без сверки с аудио".to_string(),
                        ),
                        "minor_warnings" => {
                            warnings.push("отчёт таймингов: есть мелкие замечания".to_string())
                        }
                        _ => {}
                    }
                    detected_alignment = Some(quality);
                }
                None => warnings.push("отчёт таймингов не удалось прочитать".to_string()),
            }
        } else if timings_path.exists() {
            warnings.push("тайминги есть, но отчёт качества отсутствует".to_string());
        }

        let expected_output = batch_output_path(item, plain_lines);
        if expected_output.exists() {
            detected_output_path = Some(expected_output.clone());
            match ffmpeg::probe_audio_duration_ms(&expected_output.to_string_lossy()) {
                Ok(video_duration_ms) => {
                    let (start, end, should_trim) = batch_item_trim_window(item);
                    let expected_duration_ms = if should_trim {
                        (end - start).max(1)
                    } else {
                        item.duration_ms
                    };
                    let diff_ms = (video_duration_ms - expected_duration_ms).abs();
                    if diff_ms > 2_500 {
                        warnings.push(format!(
                            "длительность видео отличается от аудио на {}",
                            ffmpeg::format_time_ms(diff_ms)
                        ));
                    }
                }
                Err(err) => errors.push(format!("готовое видео не читается: {err}")),
            }
        } else if matches!(item.status, BatchStatus::Done) || item.output_path.is_some() {
            errors.push("статус готово, но видео не найдено".to_string());
        } else {
            notices.push("готовое видео ещё не создано".to_string());
        }

        let (level, issues) = if !errors.is_empty() {
            let mut all = errors;
            all.extend(warnings);
            all.extend(notices);
            (AuditLevel::Error, all)
        } else if !warnings.is_empty() {
            let mut all = warnings;
            all.extend(notices);
            (AuditLevel::Warning, all)
        } else if !notices.is_empty() {
            (AuditLevel::Notice, notices)
        } else {
            (AuditLevel::Ok, Vec::new())
        };

        let summary = if issues.is_empty() {
            "Пакет выглядит готовым".to_string()
        } else {
            issues[0].clone()
        };

        Self {
            level,
            summary,
            issues,
            detected_output_path,
            detected_alignment,
        }
    }

    fn tooltip(&self) -> String {
        if self.issues.is_empty() {
            return self.summary.clone();
        }
        self.issues.join("\n")
    }
}

fn read_alignment_quality_from_report(path: &Path) -> Option<AlignmentQuality> {
    let data = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    let score = value
        .get("score")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let summary = value
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| score.map(summary_from_score))?;
    Some(AlignmentQuality {
        summary,
        score,
        report_path: Some(path.to_string_lossy().to_string()),
    })
}

fn batch_audit_counts(items: &[BatchItem]) -> (usize, usize, usize, usize) {
    let mut ok = 0;
    let mut notice = 0;
    let mut warning = 0;
    let mut error = 0;
    for item in items {
        match item.audit.as_ref().map(|audit| audit.level) {
            Some(AuditLevel::Ok) => ok += 1,
            Some(AuditLevel::Notice) | None => notice += 1,
            Some(AuditLevel::Warning) => warning += 1,
            Some(AuditLevel::Error) => error += 1,
        }
    }
    (ok, notice, warning, error)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BatchItem {
    folder: PathBuf,
    audio_path: PathBuf,
    lyrics_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lyrics_override: Option<String>,
    artist: String,
    title: String,
    duration_ms: i64,
    trim_start_ms: i64,
    trim_end_ms: i64,
    fade_in_ms: i32,
    fade_out_ms: i32,
    status: BatchStatus,
    progress: f32,
    output_path: Option<String>,
    #[serde(skip)]
    alignment_quality: Option<AlignmentQuality>,
    #[serde(skip)]
    audit: Option<PackageAudit>,
    #[serde(skip)]
    waveform: Option<Vec<f32>>,
}

fn waveform_peak(peaks: Option<&[f32]>, index: usize, bars: usize) -> f32 {
    let Some(peaks) = peaks.filter(|p| !p.is_empty()) else {
        return 0.0;
    };
    let start = index * peaks.len() / bars;
    let end = ((index + 1) * peaks.len() / bars)
        .max(start + 1)
        .min(peaks.len());
    peaks[start.min(peaks.len() - 1)..end]
        .iter()
        .copied()
        .fold(0.0, f32::max)
}

fn batch_trim_timeline_static_ui(
    ui: &mut egui::Ui,
    item: &BatchItem,
    accent: egui::Color32,
    success: egui::Color32,
    muted: egui::Color32,
) {
    let duration_ms = item.duration_ms.max(1000);
    let start_ms = item
        .trim_start_ms
        .clamp(0, duration_ms.saturating_sub(1000));
    let end_ms = item.trim_end_ms.clamp(start_ms + 1000, duration_ms);
    let desired_size = egui::vec2(ui.available_width(), 86.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let track_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.center().y - 16.0),
        egui::pos2(rect.right() - 8.0, rect.center().y + 18.0),
    );
    let track_width = track_rect.width().max(1.0);
    let to_x = |ms: i64| track_rect.left() + (ms as f32 / duration_ms as f32) * track_width;

    let painter = ui.painter();
    painter.rect_filled(track_rect, 4.0, egui::Color32::from_rgb(20, 28, 34));

    let selected_rect = egui::Rect::from_min_max(
        egui::pos2(to_x(start_ms), track_rect.top()),
        egui::pos2(to_x(end_ms), track_rect.bottom()),
    );
    let center_y = track_rect.center().y;
    let max_amp = track_rect.height() * 0.42;
    let bar_count = (track_width / 5.0).round().clamp(18.0, 220.0) as usize;
    for i in 0..bar_count {
        let ratio = if bar_count > 1 {
            i as f32 / (bar_count - 1) as f32
        } else {
            0.0
        };
        let x = track_rect.left() + ratio * track_width;
        let wave = waveform_peak(item.waveform.as_deref(), i, bar_count);
        let amp = max_amp * wave;
        let waveform_color = if selected_rect.contains(egui::pos2(x, center_y)) {
            egui::Color32::from_rgb(72, 222, 226)
        } else {
            egui::Color32::from_rgb(43, 94, 103)
        };
        painter.line_segment(
            [egui::pos2(x, center_y - amp), egui::pos2(x, center_y + amp)],
            egui::Stroke::new(2.0_f32, waveform_color),
        );
    }

    painter.rect_filled(
        selected_rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(13, 120, 128, 58),
    );
    painter.rect_stroke(
        selected_rect,
        4.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(85, 225, 231)),
    );

    let start_x = to_x(start_ms);
    let end_x = to_x(end_ms);
    let draw_label = |x: f32, y: f32, value: String, color: egui::Color32| {
        let width = (value.chars().count() as f32 * 6.2 + 12.0).max(42.0);
        let label_rect = egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(width, 18.0));
        painter.rect_filled(label_rect, 5.0, egui::Color32::from_rgb(28, 33, 43));
        painter.rect_stroke(label_rect, 5.0, egui::Stroke::new(1.0_f32, color));
        painter.text(
            label_rect.center(),
            egui::Align2::CENTER_CENTER,
            value,
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(238, 241, 247),
        );
    };
    draw_label(
        start_x,
        track_rect.top() - 14.0,
        ffmpeg::format_time_ms(start_ms),
        success,
    );
    draw_label(
        end_x,
        track_rect.top() - 14.0,
        ffmpeg::format_time_ms(end_ms),
        success,
    );

    painter.line_segment(
        [
            egui::pos2(track_rect.left(), track_rect.bottom() + 10.0),
            egui::pos2(track_rect.right(), track_rect.bottom() + 10.0),
        ],
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(39, 47, 60)),
    );
    for tick in 0..=4 {
        let x = track_rect.left() + track_width * tick as f32 / 4.0;
        painter.line_segment(
            [
                egui::pos2(x, track_rect.bottom() + 14.0),
                egui::pos2(x, track_rect.bottom() + 18.0),
            ],
            egui::Stroke::new(1.0_f32, muted),
        );
    }
    painter.circle_filled(
        egui::pos2(start_x, track_rect.bottom() + 22.0),
        10.0,
        accent,
    );
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ActiveTab {
    SingleTrack,
    Batch,
    Downloader,
    PptxOptimizer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackStatus {
    #[default]
    Pending,
    Downloading,
    Success,
    Warning,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackItem {
    pub pos: usize,
    pub artist: String,
    pub title: String,
    #[serde(default = "default_true")]
    pub selected: bool,
    #[serde(skip)]
    pub status: TrackStatus,
    #[serde(default, skip_serializing)]
    pub quality_note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AudioSearchReport {
    track: AudioSearchTrack,
    providers: Vec<AudioProviderRun>,
    selected: Option<AudioSearchCandidate>,
    candidates: Vec<AudioSearchCandidate>,
    safe_mode: AudioSafeMode,
}

#[derive(Debug, Clone, Deserialize)]
struct AudioSearchTrack {
    title: String,
    artists: Vec<String>,
    duration_sec: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct AudioProviderRun {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AudioSafeMode {
    download_performed: bool,
    direct_media_url_requested: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AudioSearchCandidate {
    rank: usize,
    score: f32,
    source: String,
    title: String,
    artists: Vec<String>,
    duration_sec: Option<i64>,
    public_url: Option<String>,
    public_url_ok: Option<bool>,
    flags: Vec<String>,
}

fn default_true() -> bool {
    true
}

struct BatchScanResult {
    root: Option<PathBuf>,
    items: Vec<BatchItem>,
    warnings: Vec<String>,
    folder_count: usize,
    first_folder_name: String,
}

fn find_main_pptx_in_package(folder: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(folder).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_pptx = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("pptx"))
            .unwrap_or(false);
        if !is_pptx {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if name.starts_with("~$")
            || name.contains("оптимизирован")
            || name.contains("optimized")
            || name.ends_with(".tmp")
        {
            continue;
        }
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        candidates.push((size, path));
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.into_iter().map(|(_, path)| path).next()
}

fn find_pptx_replacement_media_dirs(folder: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::<(i32, PathBuf)>::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if !name.contains("трек") {
            continue;
        }
        let mp4_count = std::fs::read_dir(&path)
            .ok()
            .map(|files| {
                files
                    .flatten()
                    .filter(|file| {
                        let file_name = file.file_name().to_string_lossy().to_lowercase();
                        file_name.starts_with("media") && file_name.ends_with(".mp4")
                    })
                    .count()
            })
            .unwrap_or(0);
        if mp4_count > 0 {
            let priority = if name.contains("исправ") { 1 } else { 0 };
            candidates.push((priority, path));
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    candidates.into_iter().map(|(_, path)| path).collect()
}

fn optimized_pptx_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    input.with_file_name(format!("{stem} - оптимизированная 1352.pptx"))
}

fn trimmed_video_output_path(input: &Path, start_ms: i64, end_ms: i64) -> PathBuf {
    let stem = input
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let start = ffmpeg::format_time_ms(start_ms).replace(':', "-");
    let end = ffmpeg::format_time_ms(end_ms).replace(':', "-");
    let mut candidate = input.with_file_name(format!("{stem} - cut {start}-{end}.mp4"));
    let mut index = 2;
    while candidate.exists() {
        candidate = input.with_file_name(format!("{stem} - cut {start}-{end} ({index}).mp4"));
        index += 1;
    }
    candidate
}

fn format_file_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1024.0 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{:.0} MB", mb)
    }
}

struct KaraokeApp {
    audio_path: Option<String>,
    audio_duration_ms: Option<i64>,
    trim_start_ms: i64,
    trim_end_ms: i64,
    trim_playhead_ms: i64,
    fade_in_ms: i32,
    fade_out_ms: i32,
    trim_status: String,
    preview_stream: Option<rodio::OutputStream>,
    preview_sink: Option<rodio::Sink>,
    preview_started_at: Option<Instant>,
    preview_started_ms: i64,
    preview_end_ms: i64,
    artist: String,
    title: String,
    lyrics: String,
    model: String,
    quality: String,
    font: String,
    font_scale: f32,

    // Цвета в формате RGB [r, g, b]
    color_active: [u8; 3],
    color_inactive: [u8; 3],
    color_bg: [u8; 3],
    inactive_opacity: f32,

    // Сдвиг аудио (мс)
    audio_delay_ms: i32,
    normalize_audio: bool,
    loudness_target_db: i32,

    // Статус выполнения
    is_generating: bool,
    progress: f32,
    status_text: String,
    log_output: String,

    // Канал получения прогресса из фонового потока
    rx: Option<Receiver<ProgressUpdate>>,
    audio_rx: Option<Receiver<AudioLoadUpdate>>,
    audio_asset: Option<audio_preview::Asset>,
    waveform_path: Option<String>,
    waveform_stamp: Option<(u64, Option<std::time::SystemTime>)>,
    waveform_cancel: Arc<AtomicBool>,
    waveform_rx: Option<Receiver<Result<audio_preview::Asset, String>>>,
    prepared_preview_rx: Option<Receiver<Result<PreparedAudioPreview, String>>>,
    preview_cancel: Arc<AtomicBool>,
    preview_normalized: bool,
    prepared_video_rx: Option<Receiver<Result<PreparedVideoPreview, String>>>,
    video_prepare_cancel: Arc<AtomicBool>,
    video_asset: Option<(String, audio_preview::Asset)>,

    // Путь к сгенерированному видео-файлу
    generated_file: Option<String>,
    video_rx: Option<Receiver<VideoFrame>>,
    video_stop: Option<Arc<AtomicBool>>,
    video_texture: Option<egui::TextureHandle>,
    video_status: String,
    video_stream: Option<rodio::OutputStream>,
    video_sink: Option<rodio::Sink>,
    video_duration_ms: i64,
    video_position_ms: i64,
    video_started_at: Option<Instant>,
    video_started_ms: i64,
    plain_lines: bool,
    verify_lrc_with_whisper: bool,
    separate_vocals_for_word_level: bool,
    batch_overwrite_timings: bool,
    batch_root: Option<PathBuf>,
    batch_items: Vec<BatchItem>,
    batch_running: bool,
    batch_stop_requested: bool,
    batch_current_index: Option<usize>,
    batch_selected_index: Option<usize>,
    batch_status_text: String,
    batch_is_scanning: bool,
    batch_scan_rx: Option<Receiver<BatchScanResult>>,
    batch_single_mode: bool,
    batch_cancel: Arc<AtomicBool>,
    batch_stop_after: Arc<AtomicBool>,
    active_tab: ActiveTab,
    ui_frame_preview: workspace_ui::FramePreview,
    ui_batch_filter: workspace_ui::BatchFilter,
    ui_batch_search: String,
    ui_lyrics_source: bool,
    ui_advanced_open: bool,
    ui_logs_open: bool,
    ui_batch_editing: Option<usize>,
    last_autosave: Instant,
    saved_settings: Vec<u8>,
    saved_project: Vec<u8>,
    dl_mode_excel: bool,
    dl_track_query: String,
    dl_excel_path: Option<PathBuf>,
    dl_output_dir: Option<PathBuf>,
    dl_limit_candidates: usize,
    dl_format: String,
    dl_is_running: bool,
    dl_status_text: String,
    dl_log_output: String,
    dl_rx: Option<Receiver<String>>,
    dl_tracks: Vec<TrackItem>,
    dl_is_parsing_excel: bool,
    dl_parse_rx: Option<Receiver<Result<Vec<TrackItem>, String>>>,
    dl_overwrite: bool,
    dl_max_workers: usize,
    dl_stop_requested: bool,
    dl_search_only: bool,
    dl_search_result: Option<AudioSearchReport>,
    dl_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    batch_start_time: Option<Instant>,
    dl_start_time: Option<Instant>,
    pptx_package_dir: Option<PathBuf>,
    pptx_input_path: Option<PathBuf>,
    pptx_output_path: Option<PathBuf>,
    pptx_is_running: bool,
    pptx_progress: f32,
    pptx_status_text: String,
    pptx_log_output: String,
    pptx_rx: Option<Receiver<PptxOptimizeUpdate>>,
    pptx_start_time: Option<Instant>,
}

impl KaraokeApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 1. Настройка космической темной темы
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = 16.0.into();
        visuals.widgets.active.rounding = 10.0.into();
        visuals.widgets.hovered.rounding = 10.0.into();
        visuals.widgets.inactive.rounding = 10.0.into();
        visuals.widgets.open.rounding = 10.0.into();

        visuals.selection.bg_fill = egui::Color32::from_rgb(37, 99, 235);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 232, 240));
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

        visuals.extreme_bg_color = egui::Color32::from_rgb(11, 15, 25);
        visuals.window_fill = egui::Color32::from_rgb(11, 15, 25);
        visuals.panel_fill = egui::Color32::from_rgb(11, 15, 25);

        cc.egui_ctx.set_visuals(visuals);

        // 2. Шрифты Montserrat — встроены прямо в бинарник
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "montserrat_regular".to_owned(),
            egui::FontData::from_static(MONTSERRAT_REGULAR),
        );
        fonts.font_data.insert(
            "montserrat_bold".to_owned(),
            egui::FontData::from_static(MONTSERRAT_BOLD),
        );
        fonts.font_data.insert(
            "montserrat_black".to_owned(),
            egui::FontData::from_static(MONTSERRAT_BLACK),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "montserrat_regular".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(1, "montserrat_bold".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(2, "montserrat_black".to_owned());
        fonts.families.insert(
            egui::FontFamily::Name("karaoke_preview".into()),
            vec![
                "montserrat_black".into(),
                "montserrat_bold".into(),
                "montserrat_regular".into(),
            ],
        );
        fonts.families.insert(
            egui::FontFamily::Name("karaoke_ui_bold".into()),
            vec!["montserrat_bold".into(), "montserrat_regular".into()],
        );
        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.style_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size = egui::vec2(30.0, 30.0);
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });

        // 3. Загрузка сохраненных настроек
        let restored_project = batch_project::Project::read(&batch_project::autosave_path()).ok();
        let settings: AppSettings = std::fs::read_to_string(paths::settings_path())
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default();

        let restored_count = restored_project
            .as_ref()
            .map(|p| p.items.len())
            .unwrap_or(0);
        Self {
            audio_path: None,
            audio_duration_ms: None,
            trim_start_ms: 0,
            trim_end_ms: 0,
            trim_playhead_ms: 0,
            fade_in_ms: settings.fade_in_ms,
            fade_out_ms: settings.fade_out_ms,
            trim_status: String::new(),
            preview_stream: None,
            preview_sink: None,
            preview_started_at: None,
            preview_started_ms: 0,
            preview_end_ms: 0,
            artist: settings.artist,
            title: settings.title,
            lyrics: settings.lyrics,
            model: settings.model,
            quality: settings.quality,
            font: normalized_font_choice(&settings.font),
            font_scale: settings.font_scale.clamp(0.7, 1.3),
            color_active: settings.color_active,
            color_inactive: settings.color_inactive,
            color_bg: settings.color_bg,
            inactive_opacity: settings.inactive_opacity.clamp(0.0, 1.0),
            audio_delay_ms: settings.audio_delay_ms,
            normalize_audio: settings.normalize_audio,
            loudness_target_db: settings
                .loudness_target_db
                .clamp(audio_loudness::MIN_TARGET_DB, audio_loudness::MAX_TARGET_DB),
            is_generating: false,
            progress: 0.0,
            status_text: "Готов к работе".to_string(),
            log_output: String::new(),
            rx: None,
            audio_rx: None,
            audio_asset: None,
            waveform_path: None,
            waveform_stamp: None,
            waveform_cancel: Arc::new(AtomicBool::new(false)),
            waveform_rx: None,
            prepared_preview_rx: None,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            preview_normalized: false,
            prepared_video_rx: None,
            video_prepare_cancel: Arc::new(AtomicBool::new(false)),
            video_asset: None,
            generated_file: None,
            video_rx: None,
            video_stop: None,
            video_texture: None,
            video_status: String::new(),
            video_stream: None,
            video_sink: None,
            video_duration_ms: 0,
            video_position_ms: 0,
            video_started_at: None,
            video_started_ms: 0,
            plain_lines: settings.plain_lines,
            verify_lrc_with_whisper: settings.verify_lrc_with_whisper,
            separate_vocals_for_word_level: settings.separate_vocals_for_word_level,
            batch_overwrite_timings: settings.batch_overwrite_timings,
            batch_root: restored_project.as_ref().and_then(|p| p.root.clone()),
            batch_items: restored_project.map(|p| p.items).unwrap_or_default(),
            batch_running: false,
            batch_stop_requested: false,
            batch_current_index: None,
            batch_selected_index: None,
            batch_status_text: if restored_count > 0 {
                format!("Восстановлено {restored_count} треков. Можно продолжить пакет.")
            } else {
                String::new()
            },
            batch_is_scanning: false,
            batch_scan_rx: None,
            batch_single_mode: false,
            batch_cancel: Arc::new(AtomicBool::new(false)),
            batch_stop_after: Arc::new(AtomicBool::new(false)),
            active_tab: ActiveTab::SingleTrack,
            ui_frame_preview: Default::default(),
            ui_batch_filter: workspace_ui::BatchFilter::All,
            ui_batch_search: String::new(),
            ui_lyrics_source: false,
            ui_advanced_open: false,
            ui_logs_open: false,
            ui_batch_editing: None,
            last_autosave: Instant::now(),
            saved_settings: Vec::new(),
            saved_project: Vec::new(),
            dl_mode_excel: false,
            dl_track_query: String::new(),
            dl_excel_path: None,
            dl_output_dir: None,
            dl_limit_candidates: 5,
            dl_format: "mp3".to_string(),
            dl_is_running: false,
            dl_status_text: "Готов".to_string(),
            dl_log_output: String::new(),
            dl_rx: None,
            dl_tracks: Vec::new(),
            dl_is_parsing_excel: false,
            dl_parse_rx: None,
            dl_overwrite: false,
            dl_max_workers: 2,
            dl_stop_requested: false,
            dl_search_only: false,
            dl_search_result: None,
            dl_child: std::sync::Arc::new(std::sync::Mutex::new(None)),
            batch_start_time: None,
            dl_start_time: None,
            pptx_package_dir: None,
            pptx_input_path: None,
            pptx_output_path: None,
            pptx_is_running: false,
            pptx_progress: 0.0,
            pptx_status_text: "Готов".to_string(),
            pptx_log_output: String::new(),
            pptx_rx: None,
            pptx_start_time: None,
        }
    }

    fn settings_snapshot(&self) -> AppSettings {
        AppSettings {
            model: self.model.clone(),
            quality: self.quality.clone(),
            font: self.font.clone(),
            font_scale: self.font_scale,
            color_active: self.color_active,
            color_inactive: self.color_inactive,
            color_bg: self.color_bg,
            inactive_opacity: self.inactive_opacity,
            audio_delay_ms: self.audio_delay_ms,
            normalize_audio: self.normalize_audio,
            loudness_target_db: self.loudness_target_db,
            fade_in_ms: self.fade_in_ms,
            fade_out_ms: self.fade_out_ms,
            artist: self.artist.clone(),
            title: self.title.clone(),
            lyrics: self.lyrics.clone(),
            plain_lines: self.plain_lines,
            verify_lrc_with_whisper: self.verify_lrc_with_whisper,
            separate_vocals_for_word_level: self.separate_vocals_for_word_level,
            batch_overwrite_timings: self.batch_overwrite_timings,
        }
    }

    fn persist_state(&mut self) {
        let settings = self.settings_snapshot();
        if let Ok(bytes) = serde_json::to_vec_pretty(&settings)
            && bytes != self.saved_settings
        {
            match desktop_app::cache::atomic_write(&paths::settings_path(), &bytes) {
                Ok(()) => self.saved_settings = bytes,
                Err(error) => paths::debug_log(format!("Настройки не сохранены: {error}")),
            }
        }
        if !self.batch_items.is_empty() || !self.saved_project.is_empty() {
            let project = batch_project::Project::new(
                self.batch_root.clone(),
                self.batch_items.clone(),
                settings,
            );
            if let Ok(bytes) = project.bytes()
                && bytes != self.saved_project
            {
                match desktop_app::cache::atomic_write(&batch_project::autosave_path(), &bytes) {
                    Ok(()) => self.saved_project = bytes,
                    Err(error) => paths::debug_log(format!("Проект не сохранён: {error}")),
                }
            }
        }
    }

    fn open_batch_project(&mut self, path: &Path) {
        self.ui_batch_editing = None;
        self.ui_frame_preview.line_time_ms = None;
        match batch_project::Project::read(path) {
            Ok(project) => {
                self.stop_preview();
                self.batch_root = project.root;
                self.batch_items = project.items;
                self.batch_selected_index = None;
                self.batch_current_index = None;
                let s = project.settings;
                self.model = s.model;
                self.quality = s.quality;
                self.font = normalized_font_choice(&s.font);
                self.font_scale = s.font_scale.clamp(0.7, 1.3);
                self.color_active = s.color_active;
                self.color_inactive = s.color_inactive;
                self.color_bg = s.color_bg;
                self.inactive_opacity = s.inactive_opacity.clamp(0.0, 1.0);
                self.audio_delay_ms = s.audio_delay_ms;
                self.normalize_audio = s.normalize_audio;
                self.loudness_target_db = s
                    .loudness_target_db
                    .clamp(audio_loudness::MIN_TARGET_DB, audio_loudness::MAX_TARGET_DB);
                self.plain_lines = s.plain_lines;
                self.verify_lrc_with_whisper = s.verify_lrc_with_whisper;
                self.separate_vocals_for_word_level = s.separate_vocals_for_word_level;
                self.batch_overwrite_timings = s.batch_overwrite_timings;
                self.batch_status_text =
                    format!("Восстановлен проект: {} треков", self.batch_items.len());
                self.persist_state();
            }
            Err(error) => self.batch_status_text = format!("Не удалось открыть проект: {error}"),
        }
    }

    fn is_preview_playing(&self) -> bool {
        self.prepared_preview_rx.is_some()
            || self
                .preview_sink
                .as_ref()
                .map(|sink| !sink.empty())
                .unwrap_or(false)
    }

    fn stop_preview(&mut self) {
        self.preview_cancel.store(true, Ordering::Relaxed);
        self.prepared_preview_rx = None;
        if let Some(sink) = self.preview_sink.take() {
            sink.stop();
        }
        self.preview_stream = None;
        self.preview_started_at = None;
        self.preview_started_ms = 0;
        self.preview_end_ms = 0;
    }

    fn play_audio_preview(&mut self, ready: PreparedAudioPreview) -> Result<(), String> {
        self.stop_preview();
        let (stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| format!("Не удалось открыть аудиовыход: {}", e))?;
        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| format!("Не удалось создать аудио-плеер: {}", e))?;

        sink.append(ready.source);
        sink.play();

        self.preview_stream = Some(stream);
        self.preview_sink = Some(sink);
        self.preview_started_at = Some(Instant::now());
        self.preview_started_ms = ready.start_ms;
        self.preview_end_ms = ready.end_ms;
        self.trim_status = ready.message;
        Ok(())
    }

    fn sync_preview_playhead(&mut self, ctx: &egui::Context) {
        let Some(started_at) = self.preview_started_at else {
            return;
        };

        let elapsed_ms = started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        self.trim_playhead_ms = (self.preview_started_ms + elapsed_ms).min(self.preview_end_ms);

        let finished = self
            .preview_sink
            .as_ref()
            .map(|sink| sink.empty())
            .unwrap_or(true)
            || self.trim_playhead_ms >= self.preview_end_ms;

        if finished {
            self.trim_playhead_ms = self.preview_end_ms;
            if let Some(sink) = self.preview_sink.take() {
                sink.stop();
            }
            self.preview_stream = None;
            self.preview_started_at = None;
            self.preview_started_ms = 0;
            self.preview_end_ms = 0;
        } else {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn is_video_playing(&self) -> bool {
        self.video_started_at.is_some()
            && self
                .video_sink
                .as_ref()
                .map(|sink| !sink.empty())
                .unwrap_or(false)
    }

    fn halt_video_preview(&mut self) {
        self.video_prepare_cancel.store(true, Ordering::Relaxed);
        self.prepared_video_rx = None;
        if let Some(stop) = self.video_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(sink) = self.video_sink.take() {
            sink.stop();
        }
        self.video_stream = None;
        self.video_rx = None;
        self.video_started_at = None;
        self.video_started_ms = 0;
    }

    fn stop_video_preview(&mut self) {
        self.halt_video_preview();
        self.video_position_ms = 0;
        self.video_status = "Предпросмотр видео остановлен.".to_string();
    }

    fn pause_video_preview(&mut self) {
        if let Some(started_at) = self.video_started_at {
            let elapsed_ms = started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
            self.video_position_ms = (self.video_started_ms + elapsed_ms)
                .min(self.video_duration_ms.max(self.video_started_ms));
        }
        self.halt_video_preview();
        self.video_status = format!(
            "Пауза на {}.",
            ffmpeg::format_time_ms(self.video_position_ms.min(self.video_duration_ms))
        );
    }

    fn start_video_preview(&mut self, path: &str, ctx: &egui::Context) -> Result<(), String> {
        self.halt_video_preview();
        self.video_status = "Готовим встроенный предпросмотр...".to_string();
        let path = path.to_string();
        let cached = self
            .video_asset
            .as_ref()
            .filter(|(name, _)| name == &path)
            .map(|(_, asset)| asset.clone());
        let requested_position = self.video_position_ms;
        self.video_prepare_cancel = Arc::new(AtomicBool::new(false));
        let cancelled = self.video_prepare_cancel.clone();
        let (tx, rx) = channel();
        self.prepared_video_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let asset = match cached {
                    Some(asset) => asset,
                    None => audio_preview::prepare(
                        Path::new(&path),
                        &paths::tool_path("ffmpeg"),
                        cancelled.clone(),
                    )?,
                };
                let (width, height) = ffmpeg::preview_video_size(&path)?;
                let start_ms = if requested_position >= asset.duration_ms - 250 {
                    0
                } else {
                    requested_position.clamp(0, asset.duration_ms)
                };
                Ok(PreparedVideoPreview {
                    path,
                    asset,
                    width,
                    height,
                    start_ms,
                })
            })();
            if !cancelled.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        });
        Ok(())
    }

    fn play_video_ready(
        &mut self,
        ready: PreparedVideoPreview,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let PreparedVideoPreview {
            path,
            asset,
            width,
            height,
            start_ms,
        } = ready;
        let source = audio_preview::WavSource::new(
            &asset.path,
            (0, asset.duration_ms),
            start_ms,
            (0, 0),
            0.0,
        )?;
        self.video_duration_ms = asset.duration_ms;
        self.video_position_ms = start_ms;
        self.video_asset = Some((path.clone(), asset));
        let (stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| format!("Не удалось открыть аудиовыход: {}", e))?;
        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| format!("Не удалось создать аудио-плеер: {}", e))?;

        let (tx, rx) = std::sync::mpsc::sync_channel::<VideoFrame>(2);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let ctx_thread = ctx.clone();
        let path = path.to_string();

        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(paths::tool_path("ffmpeg"));
            ffmpeg::hide_subprocess_window(&mut cmd);
            let mut child = match cmd
                .arg("-v")
                .arg("error")
                .arg("-ss")
                .arg(format!("{:.3}", start_ms.max(0) as f64 / 1000.0))
                .arg("-i")
                .arg(path)
                .arg("-vf")
                .arg(format!("fps=24,scale={}:{}:flags=bicubic", width, height))
                .arg("-an")
                .arg("-pix_fmt")
                .arg("rgba")
                .arg("-f")
                .arg("rawvideo")
                .arg("pipe:1")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => return,
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill();
                let _ = child.wait();
                return;
            };

            let frame_len = width.saturating_mul(height).saturating_mul(4);
            let started_at = Instant::now();
            let mut frame_index = 0u32;

            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }

                let mut pixels = vec![0u8; frame_len];
                if stdout.read_exact(&mut pixels).is_err() {
                    break;
                }

                let target_at = started_at + Duration::from_secs_f64(frame_index as f64 / 24.0);
                let now = Instant::now();
                if target_at > now {
                    std::thread::sleep(target_at - now);
                }

                if matches!(
                    tx.try_send(VideoFrame {
                        width,
                        height,
                        pixels
                    }),
                    Err(std::sync::mpsc::TrySendError::Disconnected(_))
                ) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                ctx_thread.request_repaint();
                frame_index = frame_index.saturating_add(1);
            }

            let _ = child.wait();
            ctx_thread.request_repaint();
        });

        sink.append(source);
        sink.play();

        self.video_stream = Some(stream);
        self.video_sink = Some(sink);
        self.video_rx = Some(rx);
        self.video_stop = Some(stop);
        self.video_started_at = Some(Instant::now());
        self.video_started_ms = start_ms;
        self.video_status = "Видео воспроизводится внутри приложения.".to_string();
        Ok(())
    }

    fn generated_video_trim_bounds(&self) -> Option<(i64, i64, i64, bool)> {
        let duration_ms = self.video_duration_ms.max(0);
        if duration_ms < 1000 {
            return None;
        }

        let start_ms = self
            .trim_start_ms
            .clamp(0, duration_ms.saturating_sub(1000));
        let requested_end_ms = if self.trim_end_ms > 0 {
            self.trim_end_ms
        } else {
            duration_ms
        };
        let end_ms = requested_end_ms.clamp(start_ms + 1000, duration_ms);
        let is_trimmed = start_ms > 250 || end_ms < duration_ms.saturating_sub(250);
        Some((start_ms, end_ms, duration_ms, is_trimmed))
    }

    fn start_generated_video_trim(&mut self, ctx: egui::Context) {
        if self.is_generating {
            return;
        }

        let Some(input) = self.generated_file.clone() else {
            self.status_text = "Сначала сгенерируйте видео.".to_string();
            return;
        };
        let input_path = PathBuf::from(&input);
        if !input_path.exists() {
            self.status_text = "Готовый видеофайл не найден.".to_string();
            self.log_output
                .push_str(&format!("❌ Видео не найдено: {input}\n"));
            return;
        }

        if self.video_duration_ms <= 0 {
            self.video_duration_ms = ffmpeg::probe_audio_duration_ms(&input).unwrap_or(0);
        }
        let Some((start_ms, end_ms, _duration_ms, is_trimmed)) = self.generated_video_trim_bounds()
        else {
            self.status_text = "Не удалось определить длительность видео.".to_string();
            self.log_output
                .push_str("❌ Не удалось определить длительность готового видео.\n");
            return;
        };
        if !is_trimmed {
            self.status_text = "Выберите фрагмент для обрезки.".to_string();
            self.log_output
                .push_str("ℹ️ Для обрезки готового видео сдвиньте начало или конец фрагмента.\n");
            return;
        }

        let output_path = trimmed_video_output_path(&input_path, start_ms, end_ms);
        self.stop_video_preview();
        self.video_texture = None;
        self.is_generating = true;
        self.progress = 0.1;
        self.status_text = "Обрезаем готовое видео...".to_string();
        self.log_output.push_str(&format!(
            "✂️ Обрезка MP4 без нового рендера: {} - {} → {}\n",
            ffmpeg::format_time_ms(start_ms),
            ffmpeg::format_time_ms(end_ms),
            output_path.to_string_lossy()
        ));

        let (tx, rx) = channel::<ProgressUpdate>();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let result = ffmpeg::render_trimmed_video(&input, start_ms, end_ms, &output_path);
            match result {
                Ok(()) => {
                    let _ = tx.send(ProgressUpdate::VideoTrimmed(
                        output_path.to_string_lossy().to_string(),
                    ));
                }
                Err(err) => {
                    let _ = tx.send(ProgressUpdate::VideoTrimError(err));
                }
            }
            ctx.request_repaint();
        });
    }

    fn sync_video_preview(&mut self, ctx: &egui::Context) {
        if let Some(started_at) = self.video_started_at {
            let elapsed_ms = started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
            self.video_position_ms = (self.video_started_ms + elapsed_ms)
                .min(self.video_duration_ms.max(self.video_started_ms));
        }

        if let Some(rx) = &self.video_rx {
            let mut latest = None;
            while let Ok(frame) = rx.try_recv() {
                latest = Some(frame);
            }
            if let Some(frame) = latest {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width, frame.height],
                    &frame.pixels,
                );
                let texture = self.video_texture.get_or_insert_with(|| {
                    ctx.load_texture("video_preview", image.clone(), egui::TextureOptions::LINEAR)
                });
                texture.set(image, egui::TextureOptions::LINEAR);
                ctx.request_repaint();
            }
        }

        if let Some(sink) = &self.video_sink
            && !sink.empty()
            && (self.video_duration_ms <= 0 || self.video_position_ms < self.video_duration_ms)
        {
            ctx.request_repaint_after(Duration::from_millis(33));
            return;
        }

        if self.video_stop.is_some() && self.video_sink.as_ref().map(|s| s.empty()).unwrap_or(true)
        {
            self.video_stop = None;
            self.video_stream = None;
            self.video_sink = None;
            self.video_rx = None;
            self.video_started_at = None;
            self.video_started_ms = 0;
            if self.video_duration_ms > 0 {
                self.video_position_ms = self.video_duration_ms;
            }
            self.video_status = "Предпросмотр видео завершен.".to_string();
        }
    }

    fn set_audio_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.ui_batch_editing = None;
        self.ui_frame_preview.line_time_ms = None;
        self.stop_preview();
        let path_str = path.to_string_lossy().to_string();
        self.audio_path = Some(path_str.clone());
        self.audio_duration_ms = None;
        self.trim_start_ms = 0;
        self.trim_end_ms = 0;
        self.trim_playhead_ms = 0;
        self.trim_status = "Читаем аудио...".to_string();
        self.generated_file = None;

        let file_name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if file_name.contains(" - ") {
            let parts: Vec<&str> = file_name.splitn(2, " - ").collect();
            self.artist = parts[0].trim().to_string();
            self.title = parts[1].trim().to_string();
        } else {
            self.title = file_name.trim().to_string();
            self.artist = String::new();
        }

        // Автопоиск файла с текстом рядом с аудиофайлом (.txt или .lrc)
        let mut found_lyrics = None;
        if let Some(parent) = path.parent() {
            let txt_path = parent.join(format!("{}.txt", file_name));
            let lrc_path = parent.join(format!("{}.lrc", file_name));
            let txt_upper = parent.join(format!("{}.TXT", file_name));
            let lrc_upper = parent.join(format!("{}.LRC", file_name));

            if lrc_path.exists() {
                found_lyrics = std::fs::read_to_string(lrc_path).ok();
            } else if lrc_upper.exists() {
                found_lyrics = std::fs::read_to_string(lrc_upper).ok();
            } else if txt_path.exists() {
                found_lyrics = std::fs::read_to_string(txt_path).ok();
            } else if txt_upper.exists() {
                found_lyrics = std::fs::read_to_string(txt_upper).ok();
            }
        }

        if let Some(lyrics_content) = found_lyrics {
            self.lyrics = lyrics_content;
        }

        let (tx, rx) = channel::<AudioLoadUpdate>();
        self.audio_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = ffmpeg::probe_audio_duration_ms(&path_str);
            let _ = tx.send(AudioLoadUpdate {
                path: path_str,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn set_lyrics_file(&mut self, path: PathBuf) {
        match lyrics::read_lyrics_file(&path) {
            Ok(lyrics) => {
                let file_name = lyrics::display_file_name(&path);
                let line_count = lyrics
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count();
                self.lyrics = lyrics;
                self.status_text = format!("Текст загружен: {}", file_name);
                self.log_output.push_str(&format!(
                    "📄 Загружен текст: {} ({} строк)\n",
                    file_name, line_count
                ));
            }
            Err(err) => {
                self.status_text = "Не удалось загрузить текст".to_string();
                self.log_output.push_str(&format!("❌ {}\n", err));
            }
        }
    }

    fn handle_dropped_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.is_generating {
            return;
        }

        if self.active_tab == ActiveTab::PptxOptimizer {
            if path.is_dir() {
                self.select_pptx_package_folder(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("pptx"))
                .unwrap_or(false)
            {
                self.pptx_package_dir = path.parent().map(|parent| parent.to_path_buf());
                self.pptx_input_path = Some(path.clone());
                self.pptx_output_path = Some(optimized_pptx_output_path(&path));
                self.pptx_status_text =
                    format!("Выбрана презентация: {}", lyrics::display_file_name(&path));
            }
            return;
        }

        if path.is_dir() {
            self.load_batch_folder(path, ctx);
        } else if lyrics::is_audio_file(&path) {
            self.set_audio_file(path, ctx);
        } else if lyrics::is_lyrics_file(&path) {
            self.set_lyrics_file(path);
        } else if path
            .extension()
            .map(|ext| ext.to_string_lossy().to_string().to_lowercase())
            .as_deref()
            == Some("xlsx")
        {
            self.dl_excel_path = Some(path.clone());
            self.dl_mode_excel = true;
            self.active_tab = ActiveTab::Downloader;
            self.start_parsing_excel(path, ctx.clone());
        } else {
            self.status_text = "Файл не поддерживается".to_string();
            self.log_output.push_str(&format!(
                "⚠️ Файл {} не принят. Можно перетащить .mp3, .txt, .lrc или .xlsx.\n",
                lyrics::display_file_name(&path)
            ));
        }
    }

    fn load_batch_folder(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.batch_is_scanning {
            return;
        }
        self.batch_is_scanning = true;
        self.batch_status_text = "Сканирование папки...".to_string();

        let (tx, rx) = channel::<BatchScanResult>();
        self.batch_scan_rx = Some(rx);

        let fade_in = self.fade_in_ms;
        let fade_out = self.fade_out_ms;
        let plain_lines = self.plain_lines;
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let (items, warnings) = scan_batch_folder(&path, fade_in, fade_out, plain_lines);
            let first_folder_name = lyrics::display_file_name(&path);
            let _ = tx.send(BatchScanResult {
                root: Some(path),
                items,
                warnings,
                folder_count: 1,
                first_folder_name,
            });
            ctx_clone.request_repaint();
        });
    }

    fn load_batch_folders(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if self.batch_is_scanning {
            return;
        }
        self.batch_is_scanning = true;
        self.batch_status_text = "Сканирование папок...".to_string();

        let (tx, rx) = channel::<BatchScanResult>();
        self.batch_scan_rx = Some(rx);

        let fade_in = self.fade_in_ms;
        let fade_out = self.fade_out_ms;
        let plain_lines = self.plain_lines;
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let mut items = Vec::new();
            let mut warnings = Vec::new();
            for path in &paths {
                let (mut folder_items, mut folder_warnings) =
                    scan_batch_folder(path, fade_in, fade_out, plain_lines);
                items.append(&mut folder_items);
                warnings.append(&mut folder_warnings);
            }
            items.sort_by_key(|item| lyrics::folder_sort_key(&item.folder));

            let first_folder_name = paths
                .first()
                .map(|p| lyrics::display_file_name(p))
                .unwrap_or_default();

            let _ = tx.send(BatchScanResult {
                root: paths.first().cloned(),
                items,
                warnings,
                folder_count: paths.len(),
                first_folder_name,
            });
            ctx_clone.request_repaint();
        });
    }

    fn select_pptx_package_folder(&mut self, path: PathBuf) {
        self.pptx_package_dir = Some(path.clone());
        self.pptx_input_path = find_main_pptx_in_package(&path);
        self.pptx_output_path = self
            .pptx_input_path
            .as_deref()
            .map(optimized_pptx_output_path);
        self.pptx_progress = 0.0;
        self.pptx_status_text = match &self.pptx_input_path {
            Some(input) => format!("Найдена презентация: {}", lyrics::display_file_name(input)),
            None => "В папке не найден основной .pptx".to_string(),
        };
        self.pptx_log_output.push_str(&format!(
            "📁 Папка презентации: {}\n",
            path.to_string_lossy()
        ));
        if let Some(input) = &self.pptx_input_path {
            let size = std::fs::metadata(input)
                .map(|meta| format_file_size(meta.len()))
                .unwrap_or_else(|_| "?".to_string());
            self.pptx_log_output.push_str(&format!(
                "📊 Исходная презентация: {} ({})\n",
                input.to_string_lossy(),
                size
            ));
        }
    }

    fn start_pptx_optimization(&mut self, ctx: egui::Context) {
        if self.pptx_is_running {
            return;
        }
        let Some(input) = self.pptx_input_path.clone() else {
            self.pptx_status_text = "Сначала выберите папку с презентацией".to_string();
            return;
        };
        let output = self
            .pptx_output_path
            .clone()
            .unwrap_or_else(|| optimized_pptx_output_path(&input));
        let work_dir = output
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("_pptx_optimize_1352");

        let (tx, rx) = channel::<PptxOptimizeUpdate>();
        self.pptx_rx = Some(rx);
        self.pptx_is_running = true;
        self.pptx_progress = 0.0;
        self.pptx_status_text = "Оптимизация презентации...".to_string();
        self.pptx_start_time = Some(Instant::now());
        self.pptx_log_output.push_str(&format!(
            "🚀 Оптимизация PPTX: {}\n",
            input.to_string_lossy()
        ));
        let replacement_media_dirs = input
            .parent()
            .map(find_pptx_replacement_media_dirs)
            .unwrap_or_default();
        if !replacement_media_dirs.is_empty() {
            self.pptx_log_output.push_str(&format!(
                "⚡ Быстрая замена из папок: {}\n",
                replacement_media_dirs
                    .iter()
                    .map(|path| lyrics::display_file_name(path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        std::thread::spawn(move || {
            let config = pptx_optimizer::OptimizerConfig {
                input,
                output,
                work_dir,
                replacement_media_dirs,
                width: 1352,
                crf: 20,
                preset: "veryfast".to_string(),
                audio_bitrate: "96k".to_string(),
                workers: 2,
            };
            let tx_events = tx.clone();
            let ctx_events = ctx.clone();
            let result = pptx_optimizer::optimize_pptx(config, move |event| {
                let update = match event {
                    pptx_optimizer::OptimizerEvent::Plan { total, remaining } => {
                        PptxOptimizeUpdate::Plan { total, remaining }
                    }
                    pptx_optimizer::OptimizerEvent::Done {
                        index,
                        total,
                        message,
                    } => PptxOptimizeUpdate::Done {
                        index,
                        total,
                        message,
                    },
                    pptx_optimizer::OptimizerEvent::Log(line) => PptxOptimizeUpdate::Log(line),
                    pptx_optimizer::OptimizerEvent::Summary(summary) => {
                        PptxOptimizeUpdate::Summary {
                            output_mb: summary.output_mb,
                            mp4_mb: summary.mp4_mb,
                            mp4_count: summary.mp4_count,
                            bad: None,
                        }
                    }
                };
                let _ = tx_events.send(update);
                ctx_events.request_repaint();
            });
            let _ = tx.send(PptxOptimizeUpdate::Finished(result));
            ctx.request_repaint();
        });
    }

    fn apply_batch_item_to_single_state(&mut self, index: usize) -> Result<(), String> {
        self.ui_frame_preview.line_time_ms = None;
        let item = self
            .batch_items
            .get(index)
            .cloned()
            .ok_or_else(|| "Задание не найдено".to_string())?;
        let lyrics = if let Some(text) = item.lyrics_override.clone() {
            text
        } else if item.status == BatchStatus::MissingLyrics
            || item.lyrics_path.as_os_str().is_empty()
        {
            String::new()
        } else {
            lyrics::read_lyrics_file(&item.lyrics_path)?
        };

        self.stop_preview();
        self.stop_video_preview();
        self.audio_path = Some(item.audio_path.to_string_lossy().to_string());
        self.audio_duration_ms = Some(item.duration_ms);
        self.trim_start_ms = item
            .trim_start_ms
            .clamp(0, item.duration_ms.saturating_sub(1000));
        self.trim_end_ms = item
            .trim_end_ms
            .clamp(self.trim_start_ms + 1000, item.duration_ms);
        self.trim_playhead_ms = self.trim_start_ms;
        self.fade_in_ms = item.fade_in_ms;
        self.fade_out_ms = item.fade_out_ms;
        self.trim_status = format!("Длительность: {}", ffmpeg::format_time_ms(item.duration_ms));
        self.artist = item.artist;
        self.title = item.title;
        self.lyrics = lyrics;
        self.generated_file = item.output_path.filter(|path| Path::new(path).is_file());
        self.video_texture = None;
        self.video_asset = None;
        self.video_position_ms = 0;
        self.video_duration_ms = 0;
        Ok(())
    }

    fn start_parsing_excel(&mut self, path: PathBuf, ctx: egui::Context) {
        self.dl_is_parsing_excel = true;
        self.dl_tracks.clear();
        self.dl_status_text = "Чтение файла Excel...".to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        self.dl_parse_rx = Some(rx);

        let worker_path = match paths::find_worker() {
            Some(w) => w,
            None => {
                let _ = tx.send(Err(
                    "Не удалось найти исполняемый файл воркера (karaoke_worker)".to_string(),
                ));
                ctx.request_repaint();
                return;
            }
        };

        std::thread::spawn(move || {
            let mut cmd = if paths::is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
            cmd.arg("parse-sheet").arg(&path);

            ffmpeg::hide_subprocess_window(&mut cmd);

            let output = match cmd.output() {
                Ok(out) => out,
                Err(e) => {
                    let _ = tx.send(Err(format!("Не удалось запустить python3: {}", e)));
                    ctx.request_repaint();
                    return;
                }
            };

            if !output.status.success() {
                let err_msg = String::from_utf8_lossy(&output.stderr).to_string();
                let _ = tx.send(Err(format!("Ошибка парсинга таблицы: {}", err_msg)));
                ctx.request_repaint();
                return;
            }

            let stdout_str = String::from_utf8_lossy(&output.stdout);
            match serde_json::from_str::<Vec<TrackItem>>(&stdout_str) {
                Ok(tracks) => {
                    let _ = tx.send(Ok(tracks));
                }
                Err(e) => {
                    let _ = tx.send(Err(format!(
                        "Ошибка декодирования JSON: {}, вывод: {}",
                        e, stdout_str
                    )));
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_download(&mut self, ctx: &egui::Context, continue_only: bool, search_only: bool) {
        if self.dl_is_running {
            return;
        }

        self.dl_stop_requested = false;
        self.dl_search_only = search_only;
        if search_only {
            self.dl_search_result = None;
        }
        self.dl_start_time = Some(Instant::now());
        let mode_excel = self.dl_mode_excel;
        let query = self.dl_track_query.trim().to_string();
        let excel_path = self.dl_excel_path.clone();
        let output_dir = self.dl_output_dir.clone();
        let limit = self.dl_limit_candidates;
        let format = self.dl_format.clone();
        let overwrite = self.dl_overwrite;
        let max_workers = self.dl_max_workers;

        let output_path = output_dir.unwrap_or_default();
        if !search_only && output_path.as_os_str().is_empty() {
            self.dl_status_text = "Ошибка: не выбрана папка сохранения".to_string();
            return;
        }

        let mut tracks_file_path = None;
        if mode_excel {
            let selected_tracks: Vec<TrackItem> = self
                .dl_tracks
                .iter()
                .filter(|t| {
                    if continue_only {
                        t.selected
                            && t.status != TrackStatus::Success
                            && t.status != TrackStatus::Warning
                            && t.status != TrackStatus::Skipped
                    } else {
                        t.selected
                    }
                })
                .cloned()
                .collect();
            self.dl_log_output.push_str(&format!(
                "📁 Всего треков в списке: {}, выбрано для скачивания: {}\n",
                self.dl_tracks.len(),
                selected_tracks.len()
            ));
            if selected_tracks.is_empty() {
                self.dl_status_text = "Ошибка: не выбрано ни одного трека для загрузки".to_string();
                return;
            }

            for t in &mut self.dl_tracks {
                if continue_only {
                    if t.selected
                        && t.status != TrackStatus::Success
                        && t.status != TrackStatus::Warning
                        && t.status != TrackStatus::Skipped
                    {
                        t.status = TrackStatus::Pending;
                        t.quality_note = None;
                    }
                } else {
                    if t.selected {
                        t.status = TrackStatus::Pending;
                        t.quality_note = None;
                    }
                }
            }

            let project_id = excel_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let temp_path = std::env::temp_dir().join(format!("tracks_{}.json", project_id));
            match serde_json::to_string_pretty(&selected_tracks) {
                Ok(json_str) => {
                    if let Err(e) = std::fs::write(&temp_path, json_str) {
                        self.dl_status_text = format!("Ошибка записи временного файла: {}", e);
                        return;
                    }
                    tracks_file_path = Some(temp_path);
                }
                Err(e) => {
                    self.dl_status_text = format!("Ошибка сериализации треков: {}", e);
                    return;
                }
            }
        }

        let worker_path = match paths::find_worker() {
            Some(w) => w,
            None => {
                self.dl_status_text =
                    "Ошибка: не удалось найти исполняемый файл воркера (karaoke_worker)"
                        .to_string();
                return;
            }
        };

        self.dl_is_running = true;
        self.dl_status_text = if search_only {
            "Ищу и проверяю варианты...".to_string()
        } else {
            "Запуск процесса загрузки...".to_string()
        };
        self.dl_log_output.push_str(if search_only {
            "🔎 Запуск безопасного поиска без загрузки...\n"
        } else {
            "🚀 Запуск загрузчика...\n"
        });

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        self.dl_rx = Some(rx);

        let ctx_clone = ctx.clone();
        let dl_child_clone = self.dl_child.clone();

        std::thread::spawn(move || {
            let mut cmd = if paths::is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");

            if mode_excel {
                let xlsx_path = match excel_path {
                    Some(p) => p,
                    None => {
                        let _ = tx.send("❌ Ошибка: не выбран файл Excel".to_string());
                        let _ = tx.send("___FINISHED_FAILURE___".to_string());
                        ctx_clone.request_repaint();
                        return;
                    }
                };

                let project_id = xlsx_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                cmd.arg("batch")
                    .arg(&xlsx_path)
                    .arg(&project_id)
                    .arg("--output")
                    .arg(&output_path)
                    .arg("--limit")
                    .arg(limit.to_string())
                    .arg("--format")
                    .arg(&format);

                if let Some(ref json_path) = tracks_file_path {
                    cmd.arg("--tracks-file").arg(json_path);
                }

                if overwrite {
                    cmd.arg("--overwrite");
                }

                cmd.arg("--workers").arg(max_workers.to_string());

                let _ = tx.send(format!(
                    "📁 Пакетный режим.\nФайл: {}\nПроект: {}\nПапка сохранения: {}",
                    xlsx_path.to_string_lossy(),
                    project_id,
                    output_path.to_string_lossy()
                ));
            } else if search_only {
                cmd.arg("resolve")
                    .arg(&query)
                    .arg("--limit")
                    .arg(limit.to_string())
                    .arg("--json");

                let _ = tx.send(format!(
                    "🔎 Поиск и проверка без загрузки.\nЗапрос: {}",
                    query
                ));
            } else {
                cmd.arg("download")
                    .arg(&query)
                    .arg("--output")
                    .arg(&output_path)
                    .arg("--limit")
                    .arg(limit.to_string())
                    .arg("--format")
                    .arg(&format);

                let _ = tx.send(format!(
                    "🎵 Загрузка лучшего проверенного варианта.\nТрек: {}\nПапка сохранения: {}",
                    query,
                    output_path.to_string_lossy()
                ));
            }

            ffmpeg::hide_subprocess_window(&mut cmd);

            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(format!("❌ Не удалось запустить python3: {}", e));
                    let _ = tx.send("___FINISHED_FAILURE___".to_string());
                    ctx_clone.request_repaint();
                    return;
                }
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            if let Ok(mut guard) = dl_child_clone.lock() {
                *guard = Some(child);
            }

            let tx_err = tx.clone();
            let ctx_err = ctx_clone.clone();
            let stderr_thread = std::thread::spawn(move || {
                if let Some(err) = stderr {
                    use std::io::{BufRead, BufReader};
                    let reader = BufReader::new(err);
                    for l in reader.lines().map_while(Result::ok) {
                        let _ = tx_err.send(l);
                        ctx_err.request_repaint();
                    }
                }
            });

            if let Some(out) = stdout {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(out);
                for l in reader.lines().map_while(Result::ok) {
                    let _ = tx.send(l);
                    ctx_clone.request_repaint();
                }
            }

            let _ = stderr_thread.join();

            let mut child_opt = None;
            if let Ok(mut guard) = dl_child_clone.lock() {
                child_opt = guard.take();
            }

            let success = if let Some(mut child) = child_opt {
                let status = child.wait();
                status.map(|s| s.success()).unwrap_or(false)
            } else {
                false
            };

            if success {
                let _ = tx.send("___FINISHED_SUCCESS___".to_string());
            } else {
                let _ = tx.send("___FINISHED_FAILURE___".to_string());
            }

            if let Some(ref json_path) = tracks_file_path {
                let _ = std::fs::remove_file(json_path);
            }

            ctx_clone.request_repaint();
        });
    }

    fn request_stop_download(&mut self) {
        if !self.dl_is_running {
            return;
        }
        self.dl_stop_requested = true;
        self.dl_status_text = "Остановка загрузки...".to_string();
        self.dl_log_output
            .push_str("⚠️ Остановка загрузки пользователем...\n");
        if let Ok(mut guard) = self.dl_child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn start_batch_queue(&mut self, ctx: &egui::Context) {
        if self.batch_items.is_empty() || self.is_generating {
            return;
        }
        for item in &mut self.batch_items {
            if item.status == BatchStatus::MissingLyrics {
                continue;
            }
            item.progress = 0.0;
            item.output_path = None;
            item.status = BatchStatus::Ready;
            if self.batch_overwrite_timings {
                let timings_path = batch_timings_path_for_item(item);
                let _ = std::fs::remove_file(&timings_path);
                let default_timings_path = batch_timings_path(&item.audio_path);
                if default_timings_path != timings_path {
                    let _ = std::fs::remove_file(default_timings_path);
                }
            }
        }
        self.start_batch_pipeline(ctx, false);
    }

    fn continue_batch_queue(&mut self, ctx: &egui::Context) {
        if self.batch_items.is_empty() || self.is_generating || self.batch_running {
            return;
        }
        for item in &mut self.batch_items {
            if item.status == BatchStatus::Done || item.status == BatchStatus::MissingLyrics {
                continue;
            }
            item.progress = 0.0;
            item.output_path = None;
            item.status = if batch_timings_path_for_item(item).exists() {
                BatchStatus::ReadyToRender
            } else {
                BatchStatus::Ready
            };
        }
        self.start_batch_pipeline(ctx, true);
    }

    fn start_batch_pipeline(&mut self, ctx: &egui::Context, resume: bool) {
        if self.batch_items.is_empty() || self.is_generating || self.batch_running {
            return;
        }
        let Some(worker) = paths::find_worker() else {
            self.batch_status_text = "Не найден worker".into();
            return;
        };
        let Some(renderer) = paths::find_rust_renderer() else {
            self.batch_status_text = "Не найден Rust-рендерер".into();
            return;
        };
        let engine = match paths::render_engine() {
            Ok(engine) => engine,
            Err(message) => {
                self.batch_status_text = message;
                return;
            }
        };
        self.batch_cancel.store(false, Ordering::Release);
        self.batch_stop_after.store(false, Ordering::Release);
        self.batch_stop_requested = false;
        let stop_file = paths::temp_dir().join("batch/stop_after_current");
        let _ = std::fs::remove_file(&stop_file);
        self.batch_running = true;
        self.batch_start_time = Some(Instant::now());
        self.batch_current_index = None;
        self.batch_status_text = if resume {
            "Продолжение пакета"
        } else {
            "Подготовка пакета"
        }
        .into();
        self.log_output.push_str(&format!(
            "Batch: движок {engine}; один align-процесс и один рендерер\n"
        ));
        let config = batch_pipeline::Config {
            items: self.batch_items.clone(),
            settings: self.settings_snapshot(),
            worker,
            renderer,
            engine,
            stop_after: self.batch_stop_after.clone(),
            cancel: self.batch_cancel.clone(),
            stop_file,
        };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || batch_pipeline::run(config, tx, ctx));
    }

    fn request_stop_batch(&mut self) {
        self.batch_stop_requested = true;
        self.batch_stop_after.store(true, Ordering::Release);
        let stop_file = paths::temp_dir().join("batch/stop_after_current");
        if let Some(parent) = stop_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(stop_file, b"stop") {
            self.log_output.push_str(&format!(
                "Не удалось уведомить worker об остановке: {error}\n"
            ));
        }
        self.batch_status_text = "Завершаем текущие операции, новые задания не начнутся".into();
    }

    fn cancel_batch_now(&mut self) {
        self.batch_stop_requested = true;
        self.batch_cancel.store(true, Ordering::Release);
        self.batch_status_text = "Отмена текущих операций...".into();
    }

    fn start_next_batch_item(&mut self, ctx: &egui::Context) {
        if self.batch_stop_requested {
            self.batch_running = false;
            self.batch_current_index = None;
            self.batch_status_text = "Batch остановлен.".to_string();
            return;
        }

        let Some(index) = self
            .batch_items
            .iter()
            .position(|item| item.status == BatchStatus::Ready)
        else {
            self.batch_running = false;
            self.batch_current_index = None;
            let done = self
                .batch_items
                .iter()
                .filter(|item| item.status == BatchStatus::Done)
                .count();
            let failed = self
                .batch_items
                .iter()
                .filter(|item| matches!(item.status, BatchStatus::Error(_)))
                .count();
            self.batch_status_text = format!("Batch завершен: готово {}, ошибок {}", done, failed);
            self.log_output.push_str(&format!(
                "✅ Batch завершен: готово {}, ошибок {}\n",
                done, failed
            ));
            return;
        };

        if let Some(item) = self.batch_items.get_mut(index) {
            item.status = BatchStatus::Rendering;
            item.progress = 0.0;
        }
        self.batch_current_index = Some(index);

        if let Err(err) = self.apply_batch_item_to_single_state(index) {
            if let Some(item) = self.batch_items.get_mut(index) {
                item.status = BatchStatus::Error(err.clone());
            }
            self.log_output
                .push_str(&format!("❌ Batch: {} — {}\n", index + 1, err));
            self.batch_current_index = None;
            self.start_next_batch_item(ctx);
            return;
        }

        let item_name = self
            .batch_items
            .get(index)
            .map(|item| format!("{} - {}", item.artist, item.title))
            .unwrap_or_else(|| format!("Задание {}", index + 1));
        self.batch_status_text = format!(
            "Batch: {}/{} — {}",
            index + 1,
            self.batch_items.len(),
            item_name
        );
        self.log_output.push_str(&format!(
            "\n▶️ Batch {}/{}: {}\n",
            index + 1,
            self.batch_items.len(),
            item_name
        ));
        self.start_generation(ctx.clone());
    }

    fn finish_batch_current(&mut self, success: bool, error: Option<String>, ctx: &egui::Context) {
        let Some(index) = self.batch_current_index.take() else {
            return;
        };
        if let Some(item) = self.batch_items.get_mut(index) {
            item.progress = if success { 1.0 } else { item.progress };
            item.output_path = self.generated_file.clone();
            item.status = if success {
                BatchStatus::Done
            } else {
                BatchStatus::Error(
                    error.unwrap_or_else(|| "Процесс завершился с ошибкой".to_string()),
                )
            };
            refresh_batch_item_audit(item, self.plain_lines);
        }
        self.is_generating = false;
        if self.batch_single_mode {
            self.batch_running = false;
            self.batch_single_mode = false;
            self.batch_status_text = "Генерация выбранного трека завершена.".to_string();
        } else {
            self.start_next_batch_item(ctx);
        }
    }

    fn start_single_batch_item(&mut self, index: usize, ctx: &egui::Context) {
        if self.batch_items.is_empty() || self.is_generating || self.batch_running {
            return;
        }
        if index >= self.batch_items.len() {
            return;
        }

        if let Some(item) = self.batch_items.get_mut(index) {
            item.status = BatchStatus::Rendering;
            item.progress = 0.0;
        }

        self.batch_running = true;
        self.batch_single_mode = true;
        self.batch_stop_requested = false;
        self.batch_current_index = Some(index);
        self.progress = 0.0;

        if let Err(err) = self.apply_batch_item_to_single_state(index) {
            if let Some(item) = self.batch_items.get_mut(index) {
                item.status = BatchStatus::Error(err.clone());
            }
            self.log_output
                .push_str(&format!("❌ Batch (выбранный): {} — {}\n", index + 1, err));
            self.batch_current_index = None;
            self.batch_running = false;
            self.batch_single_mode = false;
            return;
        }

        let item_name = self
            .batch_items
            .get(index)
            .map(|item| format!("{} - {}", item.artist, item.title))
            .unwrap_or_else(|| format!("Задание {}", index + 1));

        self.batch_status_text = format!("Генерация выбранного: {}", item_name);
        self.log_output.push_str(&format!(
            "\n🚀 Запуск генерации выбранного трека: {}\n",
            item_name
        ));
        self.batch_start_time = Some(Instant::now());
        self.start_generation(ctx.clone());
    }

    fn clamped_trim_bounds(&self) -> Option<(i64, i64)> {
        let duration = self.audio_duration_ms?;
        Some(clamp_trim_window(
            self.trim_start_ms,
            self.trim_end_ms,
            duration,
        ))
    }

    fn normalize_trim_state(&mut self) {
        if let Some(duration) = self.audio_duration_ms {
            let min_gap = duration.clamp(0, 1000);
            (self.trim_start_ms, self.trim_end_ms) =
                clamp_trim_window(self.trim_start_ms, self.trim_end_ms, duration);
            let selected_ms = (self.trim_end_ms - self.trim_start_ms).max(min_gap);
            let max_fade_ms = (selected_ms / 2).min(30_000) as i32;
            self.fade_in_ms = self.fade_in_ms.clamp(0, max_fade_ms);
            self.fade_out_ms = self.fade_out_ms.clamp(0, max_fade_ms);
            self.trim_playhead_ms = self
                .trim_playhead_ms
                .clamp(self.trim_start_ms, self.trim_end_ms);
        }
    }

    fn loudness_controls(&mut self, ui: &mut egui::Ui) {
        let before = (self.normalize_audio, self.loudness_target_db);
        ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.normalize_audio, "Выравнивать громкость")
                    .on_hover_text("Цель ReplayGain для готового видео, с защитой от перегрузки");
                if ui.button("Сброс").clicked() {
                    self.loudness_target_db = audio_loudness::DEFAULT_TARGET_DB;
                }
            });
            ui.add_enabled_ui(self.normalize_audio, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = (ui.available_width() - 85.0).max(80.0);
                    ui.add(
                        egui::Slider::new(
                            &mut self.loudness_target_db,
                            audio_loudness::MIN_TARGET_DB..=audio_loudness::MAX_TARGET_DB,
                        )
                        .show_value(false),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.loudness_target_db)
                            .range(audio_loudness::MIN_TARGET_DB..=audio_loudness::MAX_TARGET_DB)
                            .suffix(" дБ"),
                    );
                });
            });
            ui.label(
                egui::RichText::new("Цель ReplayGain · усиление ограничено пиками")
                    .size(12.0)
                    .weak(),
            );
        });
        if before != (self.normalize_audio, self.loudness_target_db) {
            self.stop_preview();
        }
    }

    fn preview_trimmed_audio(&mut self, ctx: &egui::Context) {
        let Some(audio_path) = self.audio_path.clone() else {
            return;
        };
        let Some((start, end)) = self.clamped_trim_bounds() else {
            return;
        };
        self.normalize_trim_state();
        let play_start = self.trim_playhead_ms.clamp(start, end.saturating_sub(500));
        self.stop_preview();
        self.trim_status = "Готовим звук в фоне...".into();
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        let cancelled = self.preview_cancel.clone();
        let cached = self.audio_asset.clone();
        let fades = (self.fade_in_ms, self.fade_out_ms);
        let normalized = self.preview_normalized && self.normalize_audio;
        let target = self.loudness_target_db;
        let (tx, rx) = channel();
        self.prepared_preview_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let ffmpeg_path = paths::tool_path("ffmpeg");
                let asset = match cached {
                    Some(asset) => asset,
                    None => audio_preview::prepare(
                        Path::new(&audio_path),
                        &ffmpeg_path,
                        cancelled.clone(),
                    )?,
                };
                let (source, message) = if normalized {
                    let key = desktop_app::cache::bytes_sha256(
                        format!("{}:{start}:{end}:{}:{}", asset.identity, fades.0, fades.1)
                            .as_bytes(),
                    );
                    let trim_path = asset.path.with_file_name(format!("normalized-{key}.wav"));
                    if !trim_path.exists() {
                        ffmpeg::render_trimmed_audio_cancellable(
                            &asset.path.to_string_lossy(),
                            start,
                            end,
                            fades.0 as i64,
                            fades.1 as i64,
                            &trim_path,
                            cancelled.clone(),
                        )?;
                    }
                    let plan = audio_loudness::normalization_plan_cancellable(
                        &ffmpeg_path,
                        &trim_path,
                        target,
                        cancelled.clone(),
                    )?;
                    let limited = if plan.applied_db < plan.requested_db - 0.01 {
                        " · ограничено пиками"
                    } else {
                        ""
                    };
                    (
                        audio_preview::WavSource::new(
                            &trim_path,
                            (0, end - start),
                            play_start - start,
                            (0, 0),
                            plan.applied_db,
                        )?,
                        format!(
                            "Цель {target} дБ · поправка {:+.2} дБ{limited}",
                            plan.applied_db
                        ),
                    )
                } else {
                    (
                        audio_preview::WavSource::new(
                            &asset.path,
                            (start, end),
                            play_start,
                            fades,
                            0.0,
                        )?,
                        "Прослушивается исходная громкость".into(),
                    )
                };
                Ok(PreparedAudioPreview {
                    source,
                    start_ms: play_start,
                    end_ms: end,
                    message,
                })
            })();
            if !cancelled.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        });
    }

    fn poll_previews(&mut self, ctx: &egui::Context) {
        let stamp = self
            .audio_path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|m| (m.len(), m.modified().ok()));
        if self.waveform_path != self.audio_path || self.waveform_stamp != stamp {
            self.waveform_stamp = stamp;
            self.waveform_cancel.store(true, Ordering::Relaxed);
            self.audio_asset = None;
            self.waveform_path = self.audio_path.clone();
            self.waveform_rx = None;
            self.stop_preview();
            if let Some(path) = self.audio_path.clone() {
                self.waveform_cancel = Arc::new(AtomicBool::new(false));
                let cancelled = self.waveform_cancel.clone();
                let (tx, rx) = channel();
                self.waveform_rx = Some(rx);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = audio_preview::prepare(
                        Path::new(&path),
                        &paths::tool_path("ffmpeg"),
                        cancelled.clone(),
                    );
                    if !cancelled.load(Ordering::Relaxed) {
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    }
                });
            }
        }
        if let Some(result) = self.waveform_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.waveform_rx = None;
            match result {
                Ok(asset) => {
                    for item in &mut self.batch_items {
                        if Some(item.audio_path.to_string_lossy().as_ref())
                            == self.audio_path.as_deref()
                        {
                            item.waveform = Some(asset.peaks.clone());
                        }
                    }
                    self.audio_asset = Some(asset);
                }
                Err(error) => {
                    self.trim_status = error;
                }
            }
        }
        if let Some(result) = self
            .prepared_preview_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.prepared_preview_rx = None;
            if let Err(error) = result.and_then(|ready| self.play_audio_preview(ready)) {
                self.trim_status = error;
            }
        }
        if let Some(result) = self
            .prepared_video_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.prepared_video_rx = None;
            if let Err(error) = result.and_then(|ready| self.play_video_ready(ready, ctx)) {
                self.video_status = error;
            }
        }
    }

    fn trim_timeline_ui(
        &mut self,
        ui: &mut egui::Ui,
        duration_ms: i64,
        accent: egui::Color32,
        _success: egui::Color32,
        muted: egui::Color32,
    ) {
        self.normalize_trim_state();
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(
                ui.available_width(),
                if ui.available_width() < 360.0 {
                    84.0
                } else {
                    108.0
                },
            ),
            egui::Sense::click_and_drag(),
        );
        let track = egui::Rect::from_min_max(
            rect.min + egui::vec2(7.0, 12.0),
            rect.max - egui::vec2(7.0, 25.0),
        );
        let to_x = |ms: i64| track.left() + (ms as f32 / duration_ms.max(1) as f32) * track.width();
        let start_x = to_x(self.trim_start_ms);
        let end_x = to_x(self.trim_end_ms);
        let drag_id = response.id.with("timeline_target");
        if !self.is_generating
            && !self.batch_running
            && (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.ui_frame_preview.line_time_ms = None;
            if self.is_preview_playing() {
                self.stop_preview();
            }
            let nearest = if (pointer.x - start_x).abs().min((pointer.x - end_x).abs()) <= 14.0 {
                if (pointer.x - start_x).abs() <= (pointer.x - end_x).abs() {
                    0
                } else {
                    1
                }
            } else {
                2
            };
            if response.drag_started() || response.clicked() {
                ui.data_mut(|data| data.insert_temp(drag_id, nearest));
            }
            let target = ui
                .data(|data| data.get_temp::<i32>(drag_id))
                .unwrap_or(nearest);
            let ms = (((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0)
                * duration_ms as f32)
                .round() as i64;
            match target {
                0 => self.trim_start_ms = ms.min(self.trim_end_ms - 1000).max(0),
                1 => self.trim_end_ms = ms.max(self.trim_start_ms + 1000).min(duration_ms),
                _ => self.trim_playhead_ms = ms.clamp(self.trim_start_ms, self.trim_end_ms),
            }
            self.normalize_trim_state();
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove::<i32>(drag_id));
        }
        response.on_hover_text("Тяните границы фрагмента; нажмите на волну для перемотки. Точное время — в полях ниже.");
        let painter = ui.painter();
        let selected = egui::Rect::from_min_max(
            egui::pos2(to_x(self.trim_start_ms), track.top()),
            egui::pos2(to_x(self.trim_end_ms), track.bottom()),
        );
        painter.rect_filled(selected, 3.0, accent.gamma_multiply(0.10));
        let peaks = self
            .audio_asset
            .as_ref()
            .map(|asset| asset.peaks.as_slice());
        let peak_scale = peaks
            .map(|peaks| peaks.iter().copied().fold(0.0f32, f32::max))
            .unwrap_or(1.0)
            .max(0.02);
        let bars = (track.width() / 4.0).clamp(18.0, 250.0) as usize;
        for i in 0..bars {
            let x = track.left() + i as f32 / (bars - 1) as f32 * track.width();
            let amplitude =
                (waveform_peak(peaks, i, bars) / peak_scale).min(1.0) * track.height() * 0.45;
            let color = if x >= selected.left() && x <= selected.right() {
                accent
            } else {
                muted.gamma_multiply(0.50)
            };
            painter.line_segment(
                [
                    egui::pos2(x, track.center().y - amplitude.max(0.5)),
                    egui::pos2(x, track.center().y + amplitude.max(0.5)),
                ],
                egui::Stroke::new(1.7_f32, color),
            );
        }
        for x in [selected.left(), selected.right()] {
            painter.line_segment(
                [egui::pos2(x, track.top()), egui::pos2(x, track.bottom())],
                egui::Stroke::new(2.0_f32, accent),
            );
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(x, track.top()), egui::vec2(7.0, 9.0)),
                2.0,
                accent,
            );
        }
        for (fade, left) in [(self.fade_in_ms, true), (self.fade_out_ms, false)] {
            if fade > 0 {
                let inner = if left {
                    to_x(self.trim_start_ms + fade as i64)
                } else {
                    to_x(self.trim_end_ms - fade as i64)
                };
                let outer = if left {
                    selected.left()
                } else {
                    selected.right()
                };
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(outer, track.top()),
                        egui::pos2(inner, track.top()),
                        egui::pos2(outer, track.bottom()),
                    ],
                    egui::Color32::from_white_alpha(30),
                    egui::Stroke::NONE,
                ));
            }
        }
        let cursor = to_x(self.trim_playhead_ms);
        painter.line_segment(
            [
                egui::pos2(cursor, track.top() - 5.0),
                egui::pos2(cursor, track.bottom()),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        );
        for i in 0..=4 {
            let x = track.left() + track.width() * i as f32 / 4.0;
            painter.text(
                egui::pos2(x, rect.bottom() - 8.0),
                if i == 0 {
                    egui::Align2::LEFT_CENTER
                } else if i == 4 {
                    egui::Align2::RIGHT_CENTER
                } else {
                    egui::Align2::CENTER_CENTER
                },
                ffmpeg::format_time_ms(duration_ms * i / 4),
                egui::FontId::proportional(10.0),
                muted,
            );
        }
    }

    fn start_generation(&mut self, ctx: egui::Context) {
        let audio_path = match &self.audio_path {
            Some(p) => p.clone(),
            None => return,
        };
        self.stop_video_preview();
        paths::clear_bundled_runtime_quarantine();

        let worker_path = match paths::find_worker() {
            Some(p) => p,
            None => {
                self.log_output
                    .push_str("❌ Не найден генератор karaoke_worker.\n");
                self.log_output.push_str(
                    "Для dev-режима нужен worker/karaoke_worker.py, для релиза — bundled worker рядом с приложением.\n",
                );
                return;
            }
        };
        let rust_renderer_path = paths::find_rust_renderer();

        self.is_generating = true;
        self.progress = 0.0;
        self.status_text = "Запуск CLI-генерации...".to_string();
        if self.batch_running {
            self.log_output
                .push_str("🚀 Инициализация фонового процесса...\n");
        } else {
            self.log_output = "🚀 Инициализация фонового процесса...\n".to_string();
        }
        self.generated_file = None;

        let artist = self.artist.trim().to_string();
        let title = self.title.trim().to_string();
        let lyrics = self.lyrics.clone();
        let model = self.model.clone();
        let quality = self.quality.clone();
        let font = normalized_font_choice(&self.font);
        let font_scale = self.font_scale;
        let active_color = self.color_active;
        let inactive_color = self.color_inactive;
        let bg_color = self.color_bg;
        let inactive_opacity = self.inactive_opacity.clamp(0.0, 1.0);
        let audio_delay_seconds = self.audio_delay_ms as f32 / 1000.0;
        let normalize_audio = self.normalize_audio;
        let loudness_target_db = self.loudness_target_db;
        let fade_in_ms = self.fade_in_ms;
        let fade_out_ms = self.fade_out_ms;
        let plain_lines = self.plain_lines;
        let verify_lrc_with_whisper = self.verify_lrc_with_whisper;
        let separate_vocals_for_word_level = self.separate_vocals_for_word_level;
        let trim_bounds = self.clamped_trim_bounds();
        let temp = paths::temp_dir();
        let default_exports = paths::exports_dir();
        let output_dir = self
            .batch_current_index
            .and_then(|index| self.batch_items.get(index))
            .map(|item| item.folder.clone())
            .unwrap_or(default_exports);
        let uploads = paths::upload_dir();
        let worker_path_for_log = worker_path.to_string_lossy().to_string();
        let render_engine = match paths::render_engine() {
            Ok(engine) => engine,
            Err(error) => {
                self.is_generating = false;
                self.status_text = error;
                return;
            }
        };
        let use_rust_renderer = rust_renderer_path.is_some();
        let renderer_path_for_log = rust_renderer_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());

        let (tx, rx) = channel::<ProgressUpdate>();
        self.rx = Some(rx);

        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&temp);
            let temp_lyrics_path = temp.join("temp_lyrics.txt");

            let active_hex = format!(
                "#{:02X}{:02X}{:02X}",
                active_color[0], active_color[1], active_color[2]
            );
            let inactive_hex = format!(
                "#{:02X}{:02X}{:02X}",
                inactive_color[0], inactive_color[1], inactive_color[2]
            );
            let bg_hex = format!("#{:02X}{:02X}{:02X}", bg_color[0], bg_color[1], bg_color[2]);
            let output_artist = if artist.is_empty() {
                "Исполнитель".to_string()
            } else {
                artist.clone()
            };
            let output_title = if title.is_empty() {
                "Песня".to_string()
            } else {
                title.clone()
            };
            let clean_filename =
                karaoke_output_filename(&output_artist, &output_title, plain_lines);
            let _ = std::fs::create_dir_all(&output_dir);
            let output_mp4_path = output_dir.join(&clean_filename);
            let timings_output_path = temp.join("karaoke_timings_final.json");
            let _ = std::fs::remove_file(&timings_output_path);

            let render_mode = renderer_path_for_log
                .as_ref()
                .map(|path| format!("Rust renderer: {}", path))
                .unwrap_or_else(|| "Python renderer: Rust-бинарник не найден".to_string());
            let _ = tx.send(ProgressUpdate::RawLog(format!(
                "📝 Временные файлы подготовлены. Запуск worker: {}\n{}",
                worker_path_for_log, render_mode
            )));
            paths::debug_log(format!(
                "[karaoke-ui] worker={} renderer={:?} model={} plain_lines={}",
                worker_path_for_log, renderer_path_for_log, model, plain_lines
            ));
            ctx.request_repaint();

            let worker_audio_path = if let Some((start, end)) = trim_bounds {
                let should_trim = start > 0
                    || ffmpeg::probe_audio_duration_ms(&audio_path)
                        .map(|duration| end < duration)
                        .unwrap_or(false)
                    || fade_in_ms > 0
                    || fade_out_ms > 0;

                if should_trim {
                    let trimmed_path = temp.join("karaoke_trimmed_generation.wav");
                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                        "✂️ Подготовка аудио: {} - {}, восхождение {}, затухание {}",
                        ffmpeg::format_time_ms(start),
                        ffmpeg::format_time_ms(end),
                        ffmpeg::format_time_ms(fade_in_ms as i64),
                        ffmpeg::format_time_ms(fade_out_ms as i64)
                    )));
                    ctx.request_repaint();

                    match ffmpeg::render_trimmed_audio(
                        &audio_path,
                        start,
                        end,
                        fade_in_ms as i64,
                        fade_out_ms as i64,
                        &trimmed_path,
                    ) {
                        Ok(()) => trimmed_path.to_string_lossy().to_string(),
                        Err(err) => {
                            let _ = tx.send(ProgressUpdate::Error(err));
                            ctx.request_repaint();
                            return;
                        }
                    }
                } else {
                    audio_path.clone()
                }
            } else {
                audio_path.clone()
            };

            let lyrics = if let Some((start, end)) = trim_bounds {
                lyrics::shift_lrc_for_trim(&lyrics, start, end - start)
            } else {
                lyrics
            };
            if let Err(e) = std::fs::write(&temp_lyrics_path, &lyrics) {
                let _ = tx.send(ProgressUpdate::Error(format!(
                    "Не удалось записать временный файл текста: {}",
                    e
                )));
                return;
            }

            let mut cmd = if paths::is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            ffmpeg::hide_subprocess_window(&mut cmd);

            if let Some(bin_dir) = paths::bundled_bin_dir() {
                let old_path = std::env::var_os("PATH").unwrap_or_default();
                let mut paths = vec![bin_dir];
                paths.extend(std::env::split_paths(&old_path));
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("PATH", joined);
                }
            }

            cmd.env("KARAOKE_EXPORT_DIR", &output_dir)
                .env("KARAOKE_UPLOAD_DIR", &uploads)
                .env("PYTHONUTF8", "1")
                .env("PYTHONIOENCODING", "utf-8")
                .arg("--cli")
                .arg("--audio")
                .arg(&worker_audio_path)
                .arg("--artist")
                .arg(&output_artist)
                .arg("--title")
                .arg(&output_title)
                .arg("--lyrics-file")
                .arg(temp_lyrics_path.to_string_lossy().as_ref())
                .arg("--model")
                .arg(&model)
                .arg("--quality")
                .arg(&quality)
                .arg("--font")
                .arg(&font)
                .arg("--font-scale")
                .arg(font_scale.to_string())
                .arg("--color-active")
                .arg(&active_hex)
                .arg("--color-inactive")
                .arg(&inactive_hex)
                .arg("--color-bg")
                .arg(&bg_hex)
                .arg("--audio-delay")
                .arg(audio_delay_seconds.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            cmd.arg("--loudness-target-db")
                .arg(loudness_target_db.to_string());
            if !normalize_audio {
                cmd.arg("--no-normalize-audio");
            }
            if plain_lines {
                cmd.arg("--plain-lines");
            }
            if verify_lrc_with_whisper {
                cmd.arg("--verify-lrc-with-whisper");
            }
            if separate_vocals_for_word_level && !plain_lines {
                cmd.arg("--separate-vocals-for-alignment");
            }

            if use_rust_renderer {
                cmd.arg("--timings-only")
                    .arg("--timings-output")
                    .arg(timings_output_path.to_string_lossy().as_ref());
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    paths::debug_log(format!("[karaoke-ui] worker spawn failed: {e}"));
                    let _ = tx.send(ProgressUpdate::Error(format!(
                        "Ошибка запуска worker: {}",
                        e
                    )));
                    ctx.request_repaint();
                    return;
                }
            };

            // Считываем stderr параллельно в отдельном потоке (во избежание взаимной блокировки буферов)
            let tx_err = tx.clone();
            let ctx_err = ctx.clone();
            let stderr_handle = child.stderr.take().map(|stderr| {
                std::thread::spawn(move || {
                    use std::io::{BufRead, BufReader};
                    let reader = BufReader::new(stderr);
                    for line_str in reader.lines().map_while(Result::ok) {
                        let trimmed = line_str.trim();
                        if !trimmed.is_empty() && !ffmpeg::is_ffmpeg_noise(trimmed) {
                            let _ =
                                tx_err.send(ProgressUpdate::RawLog(format!("[LOG] {}", trimmed)));
                            ctx_err.request_repaint();
                        }
                    }
                })
            });

            // Потоковое чтение stdout (вывод прогресса в JSON)
            if let Some(stdout) = child.stdout.take() {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(stdout);
                let exports_str = output_dir.to_string_lossy().to_string();
                for line_str in reader.lines().map_while(Result::ok) {
                    let trimmed = line_str.trim();
                    if trimmed.starts_with('{')
                        && trimmed.ends_with('}')
                        && let Ok(mut update) = serde_json::from_str::<CLIProgress>(trimmed)
                    {
                        if use_rust_renderer {
                            update.progress = (update.progress * 0.55).clamp(0.0, 0.55);
                        }
                        // Конвертируем относительное имя файла в полный путь
                        if let Some(ref filename) = update.file {
                            if Path::new(filename).is_absolute() {
                                update.file = Some(filename.clone());
                            } else {
                                let full = format!("{}/{}", exports_str, filename);
                                update.file = Some(full);
                            }
                        }
                        let _ = tx.send(ProgressUpdate::Progress(update));
                        ctx.request_repaint();
                        continue;
                    }
                    if !trimmed.is_empty() {
                        let _ = tx.send(ProgressUpdate::RawLog(trimmed.to_string()));
                        ctx.request_repaint();
                    }
                }
            }

            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }

            let status = child.wait();
            let mut success = status.map(|s| s.success()).unwrap_or(false);
            paths::debug_log(format!("[karaoke-ui] worker finished success={success}"));

            if success && let Some(renderer_path) = rust_renderer_path {
                let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                    progress: 0.55,
                    status: "Rust-рендер: сборка видео...".to_string(),
                    done: false,
                    error: None,
                    file: None,
                    batch_align_index: None,
                    alignment_report_file: None,
                    alignment_score: None,
                    alignment_summary: None,
                }));
                let _ = tx.send(ProgressUpdate::RawLog(format!(
                    "🎬 Rust-рендер: {}",
                    output_mp4_path.to_string_lossy()
                )));
                let render_duration_ms = ffmpeg::probe_audio_duration_ms(&worker_audio_path)
                    .unwrap_or(0)
                    .max(1);
                paths::debug_log(format!(
                    "[karaoke-ui] render start renderer={} output={} plain_lines={}",
                    renderer_path.to_string_lossy(),
                    output_mp4_path.to_string_lossy(),
                    plain_lines
                ));
                ctx.request_repaint();

                let mut render_cmd = std::process::Command::new(&renderer_path);
                ffmpeg::hide_subprocess_window(&mut render_cmd);
                if let Some(bin_dir) = paths::bundled_bin_dir() {
                    let old_path = std::env::var_os("PATH").unwrap_or_default();
                    let mut paths = vec![bin_dir];
                    paths.extend(std::env::split_paths(&old_path));
                    if let Ok(joined) = std::env::join_paths(paths) {
                        render_cmd.env("PATH", joined);
                    }
                }

                render_cmd
                    .arg("--timings")
                    .arg(&timings_output_path)
                    .arg("--audio")
                    .arg(&worker_audio_path)
                    .arg("--output")
                    .arg(&output_mp4_path)
                    .arg("--quality")
                    .arg(&quality)
                    .arg("--font")
                    .arg(&font)
                    .arg("--font-scale")
                    .arg(font_scale.to_string())
                    .arg("--color-active")
                    .arg(&active_hex)
                    .arg("--color-inactive")
                    .arg(&inactive_hex)
                    .arg("--color-bg")
                    .arg(&bg_hex)
                    .arg("--inactive-opacity")
                    .arg(inactive_opacity.to_string())
                    .arg("--engine")
                    .arg(render_engine.clone())
                    .arg("--audio-delay")
                    .arg(audio_delay_seconds.to_string())
                    .arg("--loudness-target-db")
                    .arg(loudness_target_db.to_string())
                    .arg("--ffmpeg")
                    .arg(paths::tool_path("ffmpeg"))
                    .arg("--ffprobe")
                    .arg(paths::tool_path("ffprobe"))
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped());

                if !normalize_audio {
                    render_cmd.arg("--no-normalize-audio");
                }
                if plain_lines {
                    render_cmd.arg("--plain-lines");
                }

                match render_cmd.spawn() {
                    Ok(mut render_child) => {
                        let mut render_error_tail: std::collections::VecDeque<String> =
                            std::collections::VecDeque::new();
                        if let Some(stderr) = render_child.stderr.take() {
                            use std::io::{BufRead, BufReader};
                            let reader = BufReader::new(stderr);
                            for line in reader.lines().map_while(Result::ok) {
                                let trimmed = line.trim();
                                if let Some(time_ms) = ffmpeg::parse_ffmpeg_time_ms(trimmed) {
                                    let value = (time_ms as f32 / render_duration_ms as f32)
                                        .clamp(0.0, 1.0);
                                    let mapped = 0.55 + value * 0.43;
                                    let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                                        progress: mapped.clamp(0.55, 0.98),
                                        status: format!(
                                            "Rust-рендер: {}%",
                                            (value * 100.0).round() as i32
                                        ),
                                        done: false,
                                        error: None,
                                        file: None,
                                        batch_align_index: None,
                                        alignment_report_file: None,
                                        alignment_score: None,
                                        alignment_summary: None,
                                    }));
                                    ctx.request_repaint();
                                    continue;
                                }
                                if let Some(percent) = trimmed.strip_prefix("render ") {
                                    let number = percent.trim_end_matches('%');
                                    if let Ok(value) = number.parse::<f32>() {
                                        let mapped = 0.55 + (value / 100.0) * 0.43;
                                        let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                                            progress: mapped.clamp(0.55, 0.98),
                                            status: format!(
                                                "Rust-рендер: {:.0}%",
                                                value.clamp(0.0, 100.0)
                                            ),
                                            done: false,
                                            error: None,
                                            file: None,
                                            batch_align_index: None,
                                            alignment_report_file: None,
                                            alignment_score: None,
                                            alignment_summary: None,
                                        }));
                                        ctx.request_repaint();
                                        continue;
                                    }
                                }
                                if ffmpeg::is_ffmpeg_progress_key(trimmed) {
                                    continue;
                                }
                                if !trimmed.is_empty() {
                                    if render_error_tail.len() >= 8 {
                                        render_error_tail.pop_front();
                                    }
                                    render_error_tail.push_back(trimmed.to_string());
                                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                                        "[Rust] {}",
                                        trimmed
                                    )));
                                    ctx.request_repaint();
                                }
                            }
                        }

                        success = render_child
                            .wait()
                            .map(|status| status.success())
                            .unwrap_or(false);
                        paths::debug_log(format!("[karaoke-ui] render finished success={success}"));
                        if success {
                            let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                                progress: 1.0,
                                status: "Видео собрано Rust-рендером.".to_string(),
                                done: true,
                                error: None,
                                file: Some(output_mp4_path.to_string_lossy().to_string()),
                                batch_align_index: None,
                                alignment_report_file: None,
                                alignment_score: None,
                                alignment_summary: None,
                            }));
                        } else {
                            let tail = render_error_tail.into_iter().collect::<Vec<_>>().join("\n");
                            let message = if tail.trim().is_empty() {
                                "Rust-рендер завершился с ошибкой.".to_string()
                            } else {
                                format!("Rust-рендер завершился с ошибкой:\n{}", tail)
                            };
                            let _ = tx.send(ProgressUpdate::Error(message));
                        }
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        success = false;
                        paths::debug_log(format!("[karaoke-ui] render spawn failed: {e}"));
                        let _ = tx.send(ProgressUpdate::Error(format!(
                            "Ошибка запуска Rust-рендера: {}",
                            e
                        )));
                        ctx.request_repaint();
                    }
                }
            }

            let _ = std::fs::remove_file(temp.join("temp_lyrics.txt"));

            let _ = tx.send(ProgressUpdate::Finished(success));
            ctx.request_repaint();
        });
    }
}

impl eframe::App for KaraokeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_previews(ctx);
        self.sync_preview_playhead(ctx);
        self.sync_video_preview(ctx);

        // Единая современная темная тема для всех стандартных виджетов egui.
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = 10.0.into();
        visuals.widgets.active.rounding = 8.0.into();
        visuals.widgets.hovered.rounding = 8.0.into();
        visuals.widgets.inactive.rounding = 8.0.into();
        visuals.widgets.open.rounding = 8.0.into();

        visuals.selection.bg_fill = egui::Color32::from_rgb(76, 121, 195);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

        visuals.extreme_bg_color = egui::Color32::from_rgb(17, 22, 30);
        visuals.window_fill = egui::Color32::from_rgb(17, 22, 30);
        visuals.panel_fill = egui::Color32::from_rgb(17, 22, 30);
        visuals.faint_bg_color = egui::Color32::from_rgb(24, 28, 36);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(28, 33, 43);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(39, 46, 60);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(76, 121, 195);

        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 229, 236));
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(196, 202, 214));

        visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(32, 41, 54);
        visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(42, 55, 73);
        visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(44, 64, 94);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(43, 52, 66));
        ctx.set_visuals(visuals);

        // Опрос фоновой загрузки аудио: ffprobe не должен подвешивать интерфейс.
        loop {
            let update = self.audio_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(update) = update else {
                break;
            };

            if self.audio_path.as_deref() == Some(update.path.as_str()) {
                match update.result {
                    Ok(duration_ms) => {
                        self.audio_duration_ms = Some(duration_ms);
                        self.trim_start_ms = 0;
                        self.trim_end_ms = duration_ms;
                        self.trim_playhead_ms = 0;
                        self.trim_status =
                            format!("Длительность: {}", ffmpeg::format_time_ms(duration_ms));
                    }
                    Err(err) => {
                        self.audio_duration_ms = None;
                        self.trim_start_ms = 0;
                        self.trim_end_ms = 0;
                        self.trim_playhead_ms = 0;
                        self.trim_status = format!("Не удалось прочитать длительность: {}", err);
                    }
                }
            }

            self.audio_rx = None;
        }

        loop {
            let update = self.pptx_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(update) = update else {
                break;
            };
            match update {
                PptxOptimizeUpdate::Log(line) => {
                    self.pptx_log_output.push_str(&format!("{line}\n"));
                }
                PptxOptimizeUpdate::Plan { total, remaining } => {
                    self.pptx_progress = if total == 0 {
                        0.0
                    } else {
                        (total.saturating_sub(remaining)) as f32 / total as f32
                    };
                    self.pptx_status_text =
                        format!("Найдено видео: {}, к оптимизации: {}", total, remaining);
                    self.pptx_log_output.push_str(&format!(
                        "📦 Найдено встроенных MP4: {}, к оптимизации: {}\n",
                        total, remaining
                    ));
                }
                PptxOptimizeUpdate::Done {
                    index,
                    total,
                    message,
                } => {
                    self.pptx_progress = if total == 0 {
                        0.0
                    } else {
                        index as f32 / total as f32
                    };
                    self.pptx_status_text = format!("Оптимизация видео: {}/{}", index, total);
                    self.pptx_log_output.push_str(&format!("{message}\n"));
                }
                PptxOptimizeUpdate::Summary {
                    output_mb,
                    mp4_mb,
                    mp4_count,
                    bad,
                } => {
                    self.pptx_progress = 1.0;
                    if bad.is_none() {
                        self.pptx_status_text = format!(
                            "Готово: {:.0} MB, видео: {} шт. / {:.0} MB",
                            output_mb, mp4_count, mp4_mb
                        );
                    } else {
                        self.pptx_status_text =
                            format!("Файл собран, но проверка сообщила: {:?}", bad);
                    }
                    self.pptx_log_output.push_str(&format!(
                        "✅ Итог: {:.1} MB, видео: {} шт. / {:.1} MB, проверка: {:?}\n",
                        output_mb, mp4_count, mp4_mb, bad
                    ));
                }
                PptxOptimizeUpdate::Finished(result) => {
                    self.pptx_is_running = false;
                    self.pptx_rx = None;
                    match result {
                        Ok(()) => {
                            self.pptx_progress = 1.0;
                            if let Some(output) = &self.pptx_output_path {
                                if let Ok(meta) = std::fs::metadata(output) {
                                    self.pptx_status_text =
                                        format!("Готово: {}", format_file_size(meta.len()));
                                }
                                self.pptx_log_output.push_str(&format!(
                                    "🎉 Сохранено: {}\n",
                                    output.to_string_lossy()
                                ));
                            }
                        }
                        Err(err) => {
                            self.pptx_status_text = "Ошибка оптимизации".to_string();
                            self.pptx_log_output.push_str(&format!("❌ {err}\n"));
                        }
                    }
                }
            }
        }

        // Опрос прогресса из фонового канала
        loop {
            let update = self.rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(update) = update else {
                break;
            };

            match update {
                ProgressUpdate::Progress(prog) => {
                    let alignment = AlignmentQuality::from_progress(&prog);
                    self.progress = prog.progress;
                    self.status_text = prog.status;
                    if let Some(index) = self.batch_current_index
                        && let Some(item) = self.batch_items.get_mut(index)
                    {
                        item.progress = prog.progress;
                        if let Some(alignment) = alignment.clone() {
                            item.alignment_quality = Some(alignment);
                        }
                    }
                    if let Some(alignment) = alignment {
                        self.log_output
                            .push_str(&format!("Качество: {}\n", alignment.log_text()));
                    }
                    if let Some(err) = &prog.error {
                        paths::debug_log(format!("[karaoke-ui] Progress error: {}", err));
                        self.log_output
                            .push_str(&format!("❌ Ошибка ИИ: {}\n", err));
                    }
                    if let Some(full_path) = prog.file {
                        self.log_output
                            .push_str(&format!("🎉 Успешно сохранено: {}\n", full_path));
                        self.stop_video_preview();
                        self.video_texture = None;
                        self.video_status = String::new();
                        self.video_duration_ms =
                            ffmpeg::probe_audio_duration_ms(&full_path).unwrap_or(0);
                        self.video_position_ms = 0;
                        self.video_asset = None;
                        self.generated_file = Some(full_path);
                        if let Some(index) = self.batch_current_index
                            && let Some(item) = self.batch_items.get_mut(index)
                        {
                            item.output_path = self.generated_file.clone();
                        }
                    }
                }
                ProgressUpdate::BatchProgress {
                    index,
                    status,
                    progress,
                    message,
                    output_path,
                    alignment,
                } => {
                    self.progress = progress;
                    self.status_text = message.clone();
                    self.batch_status_text = format!(
                        "Batch: {}/{} — {}",
                        index + 1,
                        self.batch_items.len(),
                        message
                    );
                    self.batch_current_index = Some(index);
                    if let Some(item) = self.batch_items.get_mut(index) {
                        item.status = status;
                        item.progress = progress;
                        if let Some(alignment) = alignment.clone() {
                            item.alignment_quality = Some(alignment);
                        }
                        if output_path.is_some() {
                            item.output_path = output_path.clone();
                        }
                        refresh_batch_item_audit(item, self.plain_lines);
                    }
                    if let Some(alignment) = alignment {
                        self.log_output.push_str(&format!(
                            "Batch #{} качество: {}\n",
                            index + 1,
                            alignment.log_text()
                        ));
                    }
                    if let Some(path) = output_path {
                        self.stop_video_preview();
                        self.video_asset = None;
                        self.video_duration_ms = 0;
                        self.generated_file = Some(path.clone());
                        self.log_output
                            .push_str(&format!("🎉 Batch сохранено: {}\n", path));
                    }
                }
                ProgressUpdate::RawLog(log) => {
                    paths::debug_log(format!("[worker-raw] {}", log));
                    self.log_output.push_str(&format!("{}\n", log));
                }
                ProgressUpdate::Error(err) => {
                    paths::debug_log(format!("[worker-error] {}", err));
                    if self.batch_current_index.is_some() {
                        self.log_output.push_str(&format!("❌ Ошибка: {}\n", err));
                        self.finish_batch_current(false, Some(err), ctx);
                        continue;
                    }
                    self.is_generating = false;
                    self.status_text = "Ошибка".to_string();
                    self.log_output.push_str(&format!("❌ Ошибка: {}\n", err));
                }
                ProgressUpdate::VideoTrimmed(path) => {
                    self.is_generating = false;
                    self.progress = 1.0;
                    self.status_text = "Видео обрезано.".to_string();
                    self.stop_video_preview();
                    self.video_texture = None;
                    self.video_status = String::new();
                    self.video_duration_ms = ffmpeg::probe_audio_duration_ms(&path).unwrap_or(0);
                    self.video_position_ms = 0;
                    self.generated_file = Some(path.clone());
                    self.log_output
                        .push_str(&format!("✅ Обрезанное видео сохранено: {path}\n"));
                }
                ProgressUpdate::VideoTrimError(err) => {
                    paths::debug_log(format!("[video-trim-error] {}", err));
                    self.is_generating = false;
                    self.status_text = "Ошибка обрезки видео".to_string();
                    self.log_output
                        .push_str(&format!("❌ Ошибка обрезки видео: {}\n", err));
                }
                ProgressUpdate::Finished(success) => {
                    if self.batch_current_index.is_some() {
                        if success {
                            self.log_output.push_str("✅ Batch item завершен.\n");
                        } else {
                            self.log_output
                                .push_str("❌ Batch item завершился с ошибкой.\n");
                        }
                        self.finish_batch_current(success, None, ctx);
                        continue;
                    }
                    self.is_generating = false;
                    if success {
                        self.progress = 1.0;
                        self.status_text = "Генерация успешно завершена!".to_string();
                        self.log_output.push_str("✅ Процесс успешно завершен.\n");
                    } else {
                        self.status_text = "Завершено с ошибкой".to_string();
                        self.log_output
                            .push_str("❌ Процесс завершился с кодом ошибки.\n");
                    }
                }
                ProgressUpdate::BatchFinished => {
                    let completion = if self.batch_cancel.load(Ordering::Acquire) {
                        "Пакет отменён"
                    } else if self.batch_stop_requested {
                        "Пакет остановлен"
                    } else {
                        "Пакет завершён"
                    };
                    for item in &mut self.batch_items {
                        if matches!(
                            item.status,
                            BatchStatus::Aligning
                                | BatchStatus::Rendering
                                | BatchStatus::ReadyToRender
                        ) {
                            item.status = BatchStatus::Ready;
                            item.progress = 0.0;
                        }
                    }
                    self.batch_running = false;
                    self.batch_current_index = None;
                    self.batch_stop_requested = false;
                    self.batch_cancel.store(false, Ordering::Relaxed);
                    self.is_generating = false;
                    let done = self
                        .batch_items
                        .iter()
                        .filter(|item| item.status == BatchStatus::Done)
                        .count();
                    let missing = self
                        .batch_items
                        .iter()
                        .filter(|item| item.status == BatchStatus::MissingLyrics)
                        .count();
                    let failed = self
                        .batch_items
                        .iter()
                        .filter(|item| matches!(item.status, BatchStatus::Error(_)))
                        .count();
                    self.batch_status_text = format!(
                        "{completion}: готово {}, ошибок {}, без текста {}",
                        done, failed, missing
                    );
                    self.log_output.push_str(&format!(
                        "{completion}: готово {}, ошибок {}, без текста {}\n",
                        done, failed, missing
                    ));
                }
            }
        }

        // Опрос фонового канала сканирования папок (Batch)
        if let Some(rx) = &self.batch_scan_rx
            && let Ok(res) = rx.try_recv()
        {
            self.batch_is_scanning = false;
            self.batch_scan_rx = None;

            self.batch_root = res.root;
            self.batch_items = res.items;
            self.batch_running = false;
            self.batch_stop_requested = false;
            self.batch_current_index = None;
            self.batch_selected_index = if self.batch_items.is_empty() {
                None
            } else {
                Some(0)
            };

            let total_items = self.batch_items.len();
            let missing_lyrics = self
                .batch_items
                .iter()
                .filter(|item| item.status == BatchStatus::MissingLyrics)
                .count();
            let processable_items = total_items.saturating_sub(missing_lyrics);
            let (audit_ok, audit_notice, audit_warning, audit_error) =
                batch_audit_counts(&self.batch_items);

            if res.folder_count == 1 {
                self.batch_status_text = format!(
                    "Найдено {} треков в {} · к обработке {} · без текста {} · проверить {} · ошибок аудита {}",
                    total_items,
                    res.first_folder_name,
                    processable_items,
                    missing_lyrics,
                    audit_warning,
                    audit_error
                );
                self.log_output.push_str(&format!(
                    "📁 Batch-папка: {}\n",
                    self.batch_root
                        .as_ref()
                        .map(|r| r.to_string_lossy().to_string())
                        .unwrap_or_default()
                ));
            } else {
                self.batch_status_text = format!(
                    "Найдено {} треков в {} папках · к обработке {} · без текста {} · проверить {} · ошибок аудита {}",
                    total_items,
                    res.folder_count,
                    processable_items,
                    missing_lyrics,
                    audit_warning,
                    audit_error
                );
                self.log_output.push_str(&format!(
                        "📁 Batch: перетащено папок: {}, найдено треков: {}, к обработке: {}, без текста: {}\n",
                        res.folder_count,
                        total_items,
                        processable_items,
                        missing_lyrics
                    ));
            }

            self.log_output.push_str(&format!(
                "🎵 Найдено треков: {}, к обработке: {}, без текста: {}, аудит OK: {}, не готово: {}, проверить: {}, ошибок: {}\n",
                total_items,
                processable_items,
                missing_lyrics,
                audit_ok,
                audit_notice,
                audit_warning,
                audit_error
            ));
            for warning in res.warnings {
                self.log_output.push_str(&format!("⚠️ {}\n", warning));
            }
        }

        // Опрос фонового канала парсинга Excel
        if let Some(rx) = &self.dl_parse_rx
            && let Ok(res) = rx.try_recv()
        {
            self.dl_is_parsing_excel = false;
            self.dl_parse_rx = None;
            match res {
                Ok(tracks) => {
                    self.dl_tracks = tracks;
                    self.dl_status_text =
                        format!("Загружено треков из файла: {}", self.dl_tracks.len());
                }
                Err(err) => {
                    self.dl_status_text = format!("Ошибка: {}", err);
                    self.dl_log_output.push_str(&format!("❌ {}\n", err));
                }
            }
        }

        // Опрос фонового канала загрузчика
        loop {
            let update = self.dl_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(log_line) = update else {
                break;
            };
            if self.dl_search_only
                && let Ok(report) = serde_json::from_str::<AudioSearchReport>(&log_line)
            {
                self.dl_search_result = Some(report);
                self.dl_status_text = "Результаты получены и проверены".to_string();
            } else if log_line == "___FINISHED_SUCCESS___" {
                self.dl_is_running = false;
                self.dl_status_text = if self.dl_search_only {
                    "Поиск и проверка завершены".to_string()
                } else {
                    "Загрузка успешно завершена!".to_string()
                };
                self.dl_log_output.push_str(if self.dl_search_only {
                    "\n✅ Поиск и проверка завершены. Файлы не загружались.\n"
                } else {
                    "\n🎉 Загрузка успешно завершена!\n"
                });
            } else if log_line == "___FINISHED_FAILURE___" {
                self.dl_is_running = false;
                if self.dl_stop_requested {
                    self.dl_status_text = "Загрузка остановлена пользователем".to_string();
                    self.dl_log_output
                        .push_str("\n🛑 Загрузка остановлена пользователем.\n");
                    self.dl_stop_requested = false;
                } else {
                    self.dl_status_text = if self.dl_search_only {
                        "Поиск завершился с ошибкой".to_string()
                    } else {
                        "Загрузка завершилась с ошибкой".to_string()
                    };
                    self.dl_log_output.push_str(if self.dl_search_only {
                        "\n❌ Поиск прерван из-за ошибки.\n"
                    } else {
                        "\n❌ Загрузка прервана из-за ошибки.\n"
                    });
                }
            } else {
                parse_log_line_and_update_status(&log_line, &mut self.dl_tracks);
                self.dl_log_output.push_str(&format!("{}\n", log_line));
            }
        }

        // Drag-and-drop файлов прямо на окно приложения: аудио и текст песни.
        let dropped_paths: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        let dropped_dirs: Vec<PathBuf> = dropped_paths
            .iter()
            .filter(|path| path.is_dir())
            .cloned()
            .collect();
        if dropped_dirs.len() > 1
            && self.active_tab != ActiveTab::PptxOptimizer
            && !self.is_generating
            && !self.batch_running
        {
            self.load_batch_folders(dropped_dirs, ctx);
            self.active_tab = ActiveTab::Batch;
        } else {
            for path in dropped_paths {
                self.handle_dropped_file(path, ctx);
            }
        }

        if matches!(self.active_tab, ActiveTab::SingleTrack | ActiveTab::Batch) {
            egui::TopBottomPanel::bottom("workspace_actions_v2")
                .exact_height(if self.active_tab == ActiveTab::Batch {
                    94.0
                } else {
                    78.0
                })
                .resizable(false)
                .frame(
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(23, 30, 40))
                        .inner_margin(12.0)
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(43, 52, 66),
                        )),
                )
                .show(ctx, |ui| {
                    if self.active_tab == ActiveTab::Batch {
                        self.batch_footer_ui(ui, ctx);
                    } else {
                        self.single_footer_ui(ui, ctx);
                    }
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(17, 22, 30));

            let card_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(19, 23, 31))
                .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(38, 44, 56)))
                .rounding(10.0)
                .inner_margin(18.0);

            let drop_zone_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(15, 18, 24))
                .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(54, 65, 82)))
                .rounding(8.0).inner_margin(18.0);
            let log_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(10, 12, 17))
                .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(34, 40, 52)))
                .rounding(8.0)
                .inner_margin(12.0);

            let muted = egui::Color32::from_rgb(145, 154, 171);
            let text = egui::Color32::from_rgb(238, 241, 247);
            let accent = egui::Color32::from_rgb(88, 166, 255);
            let success = egui::Color32::from_rgb(54, 211, 153);

            self.workspace_header(ui);
            ui.add_space(10.0);

            if self.active_tab == ActiveTab::Batch {
                self.batch_workspace_ui(ui, ctx);
            } else if self.active_tab == ActiveTab::Downloader {
                let batch_margin = 14.0;
                let content_width = ui.available_width() - batch_margin * 2.0;
                let col_height = ui.available_height() - 20.0;

                ui.horizontal(|ui| {
                    ui.add_space(batch_margin);

                    if self.dl_mode_excel {
                        // ПАКЕТНЫЙ РЕЖИМ (Две колонки)
                        let spacing = 18.0;
                        let col_width = (content_width - spacing) / 2.0;

                        ui.horizontal(|ui| {
                            // Левая колонка: настройки и логи
                            ui.vertical(|ui| {
                                ui.set_width(col_width);
                                ui.set_height(col_height);
                                card_frame.show(ui, |ui| {
                                    ui.set_min_width(col_width - 36.0);
                                    ui.set_min_height(col_height - 36.0);

                                    ui.label(
                                        egui::RichText::new("Настройки загрузчика")
                                            .strong()
                                            .size(18.0)
                                            .color(text),
                                    );
                                    ui.add_space(14.0);

                                    // Выбор режима
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("Режим:").strong());
                                        if ui.selectable_label(false, "Поиск трека").clicked() && !self.dl_is_running {
                                            self.dl_mode_excel = false;
                                        }
                                        if ui.selectable_label(true, "Пакетный (Excel)").clicked() && !self.dl_is_running {
                                            self.dl_mode_excel = true;
                                        }
                                    });
                                    ui.add_space(12.0);

                                    // Excel файл
                                    ui.horizontal(|ui| {
                                        ui.label("Файл Excel (.xlsx):");
                                        if ui.button("Выбрать файл").clicked() && !self.dl_is_running
                                            && let Some(path) = rfd::FileDialog::new()
                                                .add_filter("Excel Files", &["xlsx"])
                                                .pick_file()
                                            {
                                                self.dl_excel_path = Some(path.clone());
                                                self.start_parsing_excel(path, ctx.clone());
                                            }
                                        if let Some(path) = &self.dl_excel_path {
                                            ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                                        } else {
                                            ui.label("Файл не выбран");
                                        }
                                    });
                                    ui.add_space(10.0);

                                    // Папка сохранения
                                    ui.horizontal(|ui| {
                                        ui.label("Папка для сохранения:");
                                        if ui.button("Выбрать папку").clicked() && !self.dl_is_running
                                            && let Some(path) = rfd::FileDialog::new().pick_folder() {
                                                self.dl_output_dir = Some(path);
                                            }
                                        if let Some(path) = &self.dl_output_dir {
                                            ui.label(path.to_string_lossy());
                                        } else {
                                            ui.label("Папка не выбрана");
                                        }
                                    });
                                    ui.add_space(12.0);

                                    // Дополнительные параметры
                                    ui.horizontal(|ui| {
                                        ui.label("Формат:");
                                        egui::ComboBox::from_id_salt("dl_format_excel")
                                            .selected_text(&self.dl_format)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut self.dl_format, "mp3".to_string(), "MP3");
                                                ui.selectable_value(&mut self.dl_format, "flac".to_string(), "FLAC");
                                                ui.selectable_value(&mut self.dl_format, "m4a".to_string(), "M4A");
                                            });

                                        ui.add_space(20.0);
                                        ui.label("Лимит поиска:");
                                        ui.add(egui::Slider::new(&mut self.dl_limit_candidates, 1..=10));
                                    });
                                    ui.add_space(8.0);

                                    ui.horizontal(|ui| {
                                        ui.label("Потоков:");
                                        ui.add(egui::Slider::new(&mut self.dl_max_workers, 1..=5));

                                        ui.add_space(20.0);
                                        ui.checkbox(&mut self.dl_overwrite, "Перезаписывать существующие файлы");
                                    });
                                    ui.add_space(16.0);

                                    // Кнопки
                                    ui.horizontal(|ui| {
                                        if self.dl_is_running {
                                            if ui.button(egui::RichText::new("⏹ Остановить загрузку").strong()).clicked() {
                                                self.request_stop_download();
                                            }
                                        } else {
                                            let can_start = self.dl_excel_path.is_some()
                                                && self.dl_output_dir.is_some()
                                                && !self.dl_is_parsing_excel;

                                            if ui.add_enabled(
                                                can_start,
                                                egui::Button::new(egui::RichText::new("▶ Начать заново").strong()).fill(egui::Color32::from_rgb(76, 121, 195))
                                            ).clicked() {
                                                self.start_download(ctx, false, false);
                                            }

                                            let has_incomplete = self.dl_tracks.iter().any(|t| {
                                                t.selected && t.status != TrackStatus::Success && t.status != TrackStatus::Warning && t.status != TrackStatus::Skipped
                                            });
                                            let can_continue = can_start && has_incomplete;

                                            if ui.add_enabled(
                                                can_continue,
                                                egui::Button::new(egui::RichText::new("⏯ Продолжить").strong()).fill(egui::Color32::from_rgb(34, 197, 94))
                                            ).clicked() {
                                                self.start_download(ctx, true, false);
                                            }
                                        }

                                        if ui.button("Очистить лог").clicked() {
                                            self.dl_log_output.clear();
                                        }
                                    });
                                    ui.add_space(14.0);

                                     // Статус и лог
                                     ui.add_space(8.0);
                                     ui.separator();
                                     ui.add_space(8.0);

                                     ui.label(egui::RichText::new("Статус загрузки").strong().size(15.0));
                                     ui.add_space(6.0);

                                     let status_color = if self.dl_is_running {
                                         accent
                                     } else if self.dl_status_text.contains("успешно") {
                                         egui::Color32::from_rgb(34, 197, 94)
                                     } else if self.dl_status_text.contains("Ошибка") || self.dl_status_text.contains("Не удалось") {
                                         egui::Color32::from_rgb(239, 68, 68)
                                     } else {
                                         muted
                                     };
                                     ui.label(egui::RichText::new(&self.dl_status_text).strong().color(status_color));
                                     ui.add_space(8.0);

                                     // Если процесс идет или завершен, показываем прогресс-бар и сводку
                                     let total_selected = self.dl_tracks.iter().filter(|t| t.selected).count();
                                     if total_selected > 0 {
                                         let success_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Success).count();
                                         let warning_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Warning).count();
                                         let failed_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Failed).count();
                                         let skipped_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Skipped).count();
                                         let finished_count = success_count + warning_count + failed_count + skipped_count;

                                         let progress = finished_count as f32 / total_selected as f32;
                                         ui.add(
                                             egui::ProgressBar::new(progress)
                                                 .text(format!("Обработано: {} из {}", finished_count, total_selected))
                                                 .show_percentage()
                                         );
                                         ui.add_space(8.0);
                                         ui.label(format!(
                                             "Успешно: {}, проверить: {}, ошибок: {}, пропущено: {}",
                                             success_count, warning_count, failed_count, skipped_count
                                         ));

                                         ui.horizontal(|ui| {
                                             ui.label(egui::RichText::new(format!("✅ Скачано: {}", success_count)).color(egui::Color32::from_rgb(34, 197, 94)));
                                             ui.add_space(10.0);
                                             ui.label(egui::RichText::new(format!("⏭️ Пропущено: {}", skipped_count)).color(egui::Color32::from_rgb(156, 163, 175)));
                                             ui.add_space(10.0);
                                             ui.label(egui::RichText::new(format!("❌ Ошибки: {}", failed_count)).color(egui::Color32::from_rgb(239, 68, 68)));
                                         });
                                         ui.add_space(14.0);

                                         if self.dl_is_running {
                                             let active_downloads: Vec<&TrackItem> = self.dl_tracks.iter()
                                                 .filter(|t| t.selected && t.status == TrackStatus::Downloading)
                                                 .collect();

                                             ui.label(egui::RichText::new("🔄 Сейчас скачивается:").strong());
                                             ui.add_space(4.0);

                                             if active_downloads.is_empty() {
                                                 ui.label(egui::RichText::new("Подключение к источникам...").italics().color(muted));
                                             } else {
                                                 for track in active_downloads {
                                                     ui.horizontal(|ui| {
                                                         ui.add(egui::widgets::Spinner::new().size(12.0));
                                                         ui.label(format!("{:02}. {} — {}", track.pos, track.artist, track.title));
                                                     });
                                                 }
                                             }
                                             ui.add_space(14.0);
                                         }
                                     }

                                     ui.add_space(10.0);
                                     ui.collapsing("⚙️ Лог диагностики", |ui| {
                                         let logs_height = (ui.available_height() - 10.0).max(100.0);
                                         egui::ScrollArea::vertical()
                                             .auto_shrink([false, false])
                                             .max_height(logs_height)
                                             .id_salt("dl_log_scroll_excel")
                                             .show(ui, |ui| {
                                                 ui.add(
                                                     egui::TextEdit::multiline(&mut self.dl_log_output)
                                                         .font(egui::TextStyle::Monospace)
                                                         .desired_width(f32::INFINITY)
                                                         .desired_rows(8)
                                                         .lock_focus(true)
                                                 );
                                             });
                                     });
                                     ui.add_space(10.0);
                                });
                            });

                            // Правая колонка: список треков с чекбоксами
                            ui.vertical(|ui| {
                                ui.set_width(col_width);
                                ui.set_height(col_height);
                                card_frame.show(ui, |ui| {
                                    ui.set_min_width(col_width - 36.0);
                                    ui.set_min_height(col_height - 36.0);

                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Список треков в файле")
                                                .strong()
                                                .size(18.0)
                                                .color(text),
                                        );

                                        if self.dl_is_parsing_excel {
                                            ui.add_space(8.0);
                                            ui.add(egui::widgets::Spinner::new());
                                            ui.label(egui::RichText::new("Чтение...").color(muted));
                                        }
                                    });
                                    ui.add_space(14.0);

                                    if !self.dl_tracks.is_empty() {
                                        ui.horizontal(|ui| {
                                            let selected_count = self.dl_tracks.iter().filter(|t| t.selected).count();
                                            ui.label(format!("Выбрано: {} из {}", selected_count, self.dl_tracks.len()));

                                            ui.add_space(10.0);
                                            if ui.button("Выбрать все").clicked() && !self.dl_is_running {
                                                for t in &mut self.dl_tracks {
                                                    t.selected = true;
                                                }
                                            }
                                            if ui.button("Снять все").clicked() && !self.dl_is_running {
                                                for t in &mut self.dl_tracks {
                                                    t.selected = false;
                                                }
                                            }
                                        });
                                        ui.add_space(8.0);

                                        let tracks_height = ui.available_height() - 10.0;
                                        egui::ScrollArea::vertical()
                                            .auto_shrink([false, false])
                                            .max_height(tracks_height)
                                            .id_salt("dl_tracks_scroll")
                                            .show(ui, |ui| {
                                                egui::Grid::new("dl_tracks_grid")
                                                    .num_columns(4)
                                                    .spacing([12.0, 8.0])
                                                    .striped(true)
                                                    .show(ui, |ui| {
                                                        for track in &mut self.dl_tracks {
                                                            // Чекбокс
                                                            ui.add_enabled(
                                                                !self.dl_is_running,
                                                                egui::Checkbox::without_text(&mut track.selected)
                                                            );

                                                            // Номер
                                                            ui.label(egui::RichText::new(format!("{:02}.", track.pos)).color(muted));

                                                            // Название / Артист (ограничиваем ширину и обрезаем с троеточием)
                                                            let name_text = format!("{} — {}", track.artist, track.title);
                                                            ui.allocate_ui(egui::vec2((col_width - 240.0).max(150.0), 20.0), |ui| {
                                                                ui.add(
                                                                    egui::Label::new(
                                                                        egui::RichText::new(name_text).strong().color(text)
                                                                    ).truncate()
                                                                );
                                                            });

                                                            // Статус
                                                            let (status_text, status_color) = match track.status {
                                                                TrackStatus::Pending => ("⏳ Ожидание", muted),
                                                                TrackStatus::Downloading => ("🔄 Загрузка", egui::Color32::from_rgb(250, 204, 21)),
                                                                TrackStatus::Success => ("✅ Успешно", egui::Color32::from_rgb(34, 197, 94)),
                                                                TrackStatus::Warning => ("⚠️ Проверить", egui::Color32::from_rgb(249, 115, 22)),
                                                                TrackStatus::Failed => ("❌ Ошибка", egui::Color32::from_rgb(239, 68, 68)),
                                                                TrackStatus::Skipped => ("⏭️ Пропущен", egui::Color32::from_rgb(156, 163, 175)),
                                                            };
                                                            let status_resp = ui.label(egui::RichText::new(status_text).color(status_color));
                                                            if let Some(note) = &track.quality_note {
                                                                status_resp.on_hover_text(note);
                                                            }
                                                            ui.end_row();
                                                        }
                                                    });
                                            });
                                    } else {
                                        ui.vertical_centered(|ui| {
                                            ui.add_space(40.0);
                                            if self.dl_is_parsing_excel {
                                                ui.label("Идет парсинг таблицы, пожалуйста, подождите...");
                                            } else if self.dl_excel_path.is_some() {
                                                ui.label("Не найдено подходящих треков в файле.");
                                            } else {
                                                ui.label("Выберите файл Excel слева, чтобы просмотреть список треков.");
                                            }
                                            ui.add_space(40.0);
                                        });
                                    }
                                });
                            });
                        });
                    } else {
                        // ОДИНОЧНЫЙ РЕЖИМ (Старый одноколоночный макет)
                        ui.vertical(|ui| {
                            ui.set_width(content_width.max(280.0));
                            card_frame.show(ui, |ui| {
                                let inner_width = ui.available_width().max(240.0);
                                ui.set_width(inner_width);

                                ui.label(
                                    egui::RichText::new("Поиск и проверка аудио")
                                        .strong()
                                        .size(18.0)
                                        .color(text),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Безопасный режим только анализирует кандидатов, проверяет совпадение и доступность URL — без загрузки файлов.",
                                    )
                                    .size(12.0)
                                    .color(muted),
                                );
                                ui.add_space(14.0);

                                // Выбор режима
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("Режим:").strong());
                                    if ui.selectable_label(true, "Поиск трека").clicked() && !self.dl_is_running {
                                        self.dl_mode_excel = false;
                                    }
                                    if ui.selectable_label(false, "Пакетный (Excel)").clicked() && !self.dl_is_running {
                                        self.dl_mode_excel = true;
                                    }
                                });
                                ui.add_space(12.0);

                                // Одиночный трек
                                ui.label(egui::RichText::new("Исполнитель и название").strong().color(text));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.dl_track_query)
                                        .hint_text("Например: Земфира — Искала")
                                        .margin(egui::vec2(10.0, 9.0))
                                        .desired_width(ui.available_width()),
                                );
                                ui.add_space(12.0);

                                let has_query = !self.dl_track_query.trim().is_empty();
                                if self.dl_is_running {
                                    if ui
                                        .add_sized(
                                            egui::vec2(ui.available_width(), 40.0),
                                            egui::Button::new(
                                                egui::RichText::new("Остановить текущую операцию").strong(),
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.request_stop_download();
                                    }
                                } else {
                                    let search_button = egui::Button::new(
                                        egui::RichText::new("Найти и проверить — без загрузки")
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(76, 121, 195))
                                    .rounding(8.0);
                                    if ui
                                        .add_enabled_ui(has_query, |ui| {
                                            ui.add_sized(
                                                egui::vec2(ui.available_width(), 42.0),
                                                search_button,
                                            )
                                        })
                                        .inner
                                        .clicked()
                                    {
                                        self.start_download(ctx, false, true);
                                    }
                                }
                                if !has_query {
                                    ui.label(
                                        egui::RichText::new("Введите запрос, чтобы начать проверку.")
                                            .size(11.0)
                                            .color(muted),
                                    );
                                }
                                ui.add_space(18.0);
                                ui.separator();
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new("Загрузка проверенного варианта")
                                        .strong()
                                        .size(14.0)
                                        .color(text),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Необязательный следующий шаг. Используйте только для материалов, на которые у вас есть права.",
                                    )
                                    .size(11.0)
                                    .color(muted),
                                );
                                ui.add_space(8.0);

                                // Папка сохранения
                                ui.horizontal(|ui| {
                                    ui.label("Папка:");
                                    if ui.button("Выбрать папку").clicked() && !self.dl_is_running
                                        && let Some(path) = rfd::FileDialog::new().pick_folder() {
                                            self.dl_output_dir = Some(path);
                                        }
                                    if let Some(path) = &self.dl_output_dir {
                                        ui.label(path.to_string_lossy());
                                    } else {
                                        ui.label(egui::RichText::new("не выбрана").color(muted));
                                    }
                                });
                                ui.add_space(12.0);

                                // Дополнительные параметры
                                ui.horizontal(|ui| {
                                    ui.label("Формат:");
                                    egui::ComboBox::from_id_salt("dl_format_single")
                                        .selected_text(&self.dl_format)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut self.dl_format, "mp3".to_string(), "MP3");
                                            ui.selectable_value(&mut self.dl_format, "flac".to_string(), "FLAC");
                                            ui.selectable_value(&mut self.dl_format, "m4a".to_string(), "M4A");
                                        });

                                    ui.add_space(20.0);
                                    ui.label("Лимит поиска:");
                                    ui.add(egui::Slider::new(&mut self.dl_limit_candidates, 1..=10));
                                });
                                ui.add_space(16.0);

                                if !self.dl_is_running {
                                    let can_download = has_query && self.dl_output_dir.is_some();
                                    if ui
                                        .add_enabled(
                                            can_download,
                                            egui::Button::new(
                                                egui::RichText::new("Скачать лучший проверенный вариант")
                                                    .strong(),
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.start_download(ctx, false, false);
                                    }
                                }
                                ui.add_space(14.0);

                                ui.label(
                                    egui::RichText::new(&self.dl_status_text)
                                        .strong()
                                        .color(if self.dl_status_text.contains("Ошибка") {
                                            egui::Color32::from_rgb(239, 96, 96)
                                        } else {
                                            accent
                                        }),
                                );
                                if let Some(report) = self.dl_search_result.clone() {
                                    ui.add_space(10.0);
                                    let providers_ok = report
                                        .providers
                                        .iter()
                                        .filter(|provider| provider.status == "ok")
                                        .count();
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} — {}",
                                                report.track.artists.join(", "),
                                                report.track.title
                                            ))
                                            .strong()
                                            .size(14.0)
                                            .color(text),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{} · {} источников",
                                                        format_audio_duration(report.track.duration_sec),
                                                        providers_ok
                                                    ))
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(muted),
                                                );
                                            },
                                        );
                                    });
                                    ui.add_space(8.0);

                                    if let Some(candidate) = report.selected.clone() {
                                        let verified = candidate.public_url_ok == Some(true);
                                        let candidate_color = if verified && candidate.flags.is_empty() {
                                            success
                                        } else {
                                            egui::Color32::from_rgb(246, 181, 70)
                                        };
                                        egui::Frame::none()
                                            .fill(egui::Color32::from_rgb(22, 29, 39))
                                            .stroke(egui::Stroke::new(
                                                1.0_f32,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    255, 255, 255, 20,
                                                ),
                                            ))
                                            .rounding(12.0)
                                            .inner_margin(14.0)
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.vertical(|ui| {
                                                        ui.label(
                                                            egui::RichText::new("Лучший доступный вариант")
                                                                .size(11.0)
                                                                .strong()
                                                                .color(candidate_color),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(&candidate.title)
                                                                .strong()
                                                                .size(14.0)
                                                                .color(text),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "{} · {} · {}",
                                                                candidate.artists.join(", "),
                                                                audio_source_label(&candidate.source),
                                                                format_audio_duration(candidate.duration_sec)
                                                            ))
                                                            .size(11.0)
                                                            .color(muted),
                                                        );
                                                    });
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(egui::Align::Center),
                                                        |ui| {
                                                            ui.vertical(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "{:.0}%",
                                                                        candidate.score
                                                                    ))
                                                                    .monospace()
                                                                    .strong()
                                                                    .size(17.0)
                                                                    .color(candidate_color),
                                                                );
                                                                ui.label(
                                                                    egui::RichText::new(if verified {
                                                                        "URL проверен"
                                                                    } else {
                                                                        "URL не подтверждён"
                                                                    })
                                                                    .size(10.0)
                                                                    .color(muted),
                                                                );
                                                            });
                                                        },
                                                    );
                                                });
                                                if !candidate.flags.is_empty() {
                                                    ui.add_space(8.0);
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Обратите внимание: {}",
                                                            candidate.flags.join(", ")
                                                        ))
                                                        .size(10.5)
                                                        .color(candidate_color),
                                                    );
                                                }
                                                if let Some(url) = &candidate.public_url {
                                                    ui.add_space(10.0);
                                                    if ui
                                                        .add_sized(
                                                            egui::vec2(150.0, 40.0),
                                                            egui::Button::new("Открыть страницу"),
                                                        )
                                                        .clicked()
                                                    {
                                                        open_url(url);
                                                    }
                                                }
                                            });
                                    } else {
                                        ui.label(
                                            egui::RichText::new("Подходящий доступный вариант не найден.")
                                                .color(egui::Color32::from_rgb(246, 181, 70)),
                                        );
                                    }

                                    let alternatives: Vec<_> = report
                                        .candidates
                                        .iter()
                                        .filter(|candidate| {
                                            report
                                                .selected
                                                .as_ref()
                                                .map(|selected| selected.rank != candidate.rank)
                                                .unwrap_or(true)
                                        })
                                        .take(4)
                                        .cloned()
                                        .collect();
                                    if !alternatives.is_empty() {
                                        ui.add_space(8.0);
                                        egui::CollapsingHeader::new(format!(
                                            "Другие варианты ({})",
                                            alternatives.len()
                                        ))
                                        .id_salt("audio_search_alternatives")
                                        .show(ui, |ui| {
                                            for candidate in alternatives {
                                                egui::Frame::none()
                                                    .fill(egui::Color32::from_rgb(16, 20, 27))
                                                    .rounding(8.0)
                                                    .inner_margin(egui::vec2(10.0, 8.0))
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.vertical(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(&candidate.title)
                                                                        .size(11.5)
                                                                        .color(text),
                                                                );
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "{} · {}",
                                                                        audio_source_label(&candidate.source),
                                                                        format_audio_duration(candidate.duration_sec)
                                                                    ))
                                                                    .size(10.0)
                                                                    .color(muted),
                                                                );
                                                            });
                                                            ui.with_layout(
                                                                egui::Layout::right_to_left(
                                                                    egui::Align::Center,
                                                                ),
                                                                |ui| {
                                                                    ui.label(
                                                                        egui::RichText::new(format!(
                                                                            "{:.0}%",
                                                                            candidate.score
                                                                        ))
                                                                        .monospace()
                                                                        .color(muted),
                                                                    );
                                                                },
                                                            );
                                                        });
                                                    });
                                                ui.add_space(4.0);
                                            }
                                        });
                                    }

                                    let safe = !report.safe_mode.download_performed
                                        && !report.safe_mode.direct_media_url_requested;
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(if safe {
                                            "✓ Безопасный режим: медиафайлы и прямые ссылки не запрашивались"
                                        } else {
                                            "Проверьте границы безопасного режима"
                                        })
                                        .size(10.5)
                                        .color(if safe { success } else { muted }),
                                    );
                                }
                                ui.add_space(6.0);
                                egui::CollapsingHeader::new("Технические детали")
                                    .id_salt("dl_log_details_single")
                                    .default_open(self.dl_is_running)
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Диагностика источников и оценки совпадения")
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui.small_button("Очистить").clicked() {
                                                        self.dl_log_output.clear();
                                                    }
                                                },
                                            );
                                        });
                                        egui::ScrollArea::vertical()
                                            .max_height(260.0)
                                            .id_salt("dl_log_scroll_single")
                                            .stick_to_bottom(self.dl_is_running)
                                            .show(ui, |ui| {
                                                ui.add(
                                                    egui::TextEdit::multiline(&mut self.dl_log_output)
                                                        .font(egui::TextStyle::Monospace)
                                                        .desired_width(f32::INFINITY)
                                                        .desired_rows(10)
                                                        .interactive(false),
                                                );
                                            });
                                    });
                            });
                        });
                    }

                    ui.add_space(batch_margin);
                });
            } else if self.active_tab == ActiveTab::PptxOptimizer {
                let batch_margin = 14.0;
                let content_width = ui.available_width() - batch_margin * 2.0;
                let col_height = ui.available_height() - 20.0;

                ui.horizontal(|ui| {
                    ui.add_space(batch_margin);
                    ui.vertical(|ui| {
                        ui.set_width(content_width.max(280.0));
                        ui.set_height(col_height);
                        card_frame.show(ui, |ui| {
                            ui.set_width(ui.available_width().max(240.0));
                            ui.set_min_height(col_height - 36.0);

                            ui.label(
                                egui::RichText::new("Оптимизация презентации")
                                    .strong()
                                    .size(18.0)
                                    .color(text),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Выберите папку пакета. Программа найдёт основной PPTX и создаст рядом лёгкую копию.")
                                    .size(12.0)
                                    .color(muted),
                            );
                            ui.add_space(16.0);

                            drop_zone_frame.show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    let folder_label = self
                                        .pptx_package_dir
                                        .as_ref()
                                        .map(|path| path.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "Перетащите папку пакета сюда или выберите её".to_string());
                                    let button = egui::Button::new(
                                        egui::RichText::new(folder_label)
                                            .strong()
                                            .size(12.0)
                                            .color(if self.pptx_package_dir.is_some() {
                                                success
                                            } else {
                                                egui::Color32::from_rgb(198, 205, 216)
                                            }),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .min_size(egui::vec2(ui.available_width(), 42.0));

                                    if ui.add_enabled(!self.pptx_is_running, button).clicked()
                                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                                    {
                                        self.select_pptx_package_folder(path);
                                    }
                                });
                            });
                            ui.add_space(16.0);

                            egui::Grid::new("pptx_optimizer_grid")
                                .num_columns(2)
                                .spacing([14.0, 10.0])
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new("Исходный PPTX").color(muted));
                                    if let Some(input) = &self.pptx_input_path {
                                        let size = std::fs::metadata(input)
                                            .map(|meta| format_file_size(meta.len()))
                                            .unwrap_or_else(|_| "?".to_string());
                                        ui.label(format!(
                                            "{} ({})",
                                            lyrics::display_file_name(input),
                                            size
                                        ));
                                    } else {
                                        ui.label("Не выбран");
                                    }
                                    ui.end_row();

                                    ui.label(egui::RichText::new("Новый файл").color(muted));
                                    if let Some(output) = &self.pptx_output_path {
                                        ui.label(lyrics::display_file_name(output));
                                    } else {
                                        ui.label("Будет создан рядом с исходником");
                                    }
                                    ui.end_row();

                                    ui.label(egui::RichText::new("Профиль").color(muted));
                                    ui.label("MP4 1352px · быстрый H.264 · AAC 96k");
                                    ui.end_row();
                                });

                            ui.add_space(16.0);
                            ui.horizontal(|ui| {
                                let can_start = self.pptx_input_path.is_some() && !self.pptx_is_running;
                                if ui
                                    .add_enabled(
                                        can_start,
                                        egui::Button::new(
                                            egui::RichText::new("Оптимизировать презентацию")
                                                .strong(),
                                        )
                                        .fill(egui::Color32::from_rgb(76, 121, 195)),
                                    )
                                    .clicked()
                                {
                                    self.start_pptx_optimization(ctx.clone());
                                }

                                if let Some(output) = &self.pptx_output_path
                                    && output.exists()
                                    && ui.button("Открыть папку").clicked()
                                {
                                    open_in_explorer(output);
                                }

                                if ui.button("Очистить лог").clicked() && !self.pptx_is_running {
                                    self.pptx_log_output.clear();
                                }
                            });

                            ui.add_space(16.0);
                            ui.separator();
                            ui.add_space(10.0);

                            let status_color = if self.pptx_is_running {
                                accent
                            } else if self.pptx_status_text.contains("Готово") {
                                success
                            } else if self.pptx_status_text.contains("Ошибка") {
                                egui::Color32::from_rgb(255, 100, 100)
                            } else {
                                muted
                            };
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&self.pptx_status_text)
                                        .strong()
                                        .color(status_color),
                                );
                                if self.pptx_is_running {
                                    ui.add(egui::widgets::Spinner::new().size(14.0));
                                }
                            });
                            ui.add_space(8.0);
                            ui.add(
                                egui::ProgressBar::new(self.pptx_progress)
                                    .animate(self.pptx_is_running)
                                    .show_percentage(),
                            );

                            if let Some(started_at) = self.pptx_start_time
                                && self.pptx_is_running
                            {
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Прошло: {} мин",
                                        started_at.elapsed().as_secs() / 60
                                    ))
                                    .size(11.0)
                                    .color(muted),
                                );
                            }

                            ui.add_space(14.0);
                            log_frame.show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("pptx_optimizer_log")
                                    .stick_to_bottom(true)
                                    .max_height((ui.available_height() - 8.0).max(220.0))
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::TextEdit::multiline(&mut self.pptx_log_output)
                                                .font(egui::TextStyle::Monospace)
                                                .desired_width(f32::INFINITY)
                                                .desired_rows(16)
                                                .lock_focus(true),
                                        );
                                    });
                            });
                        });
                    });
                    ui.add_space(batch_margin);
                });
            } else {
                self.single_workspace_ui(ui, ctx);

            }
        });

        self.workspace_windows(ctx);

        if self.last_autosave.elapsed() >= Duration::from_secs(1) {
            self.persist_state();
            self.last_autosave = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_state();
        self.batch_cancel.store(true, Ordering::Release);
        self.waveform_cancel.store(true, Ordering::Release);
        self.stop_preview();
        self.halt_video_preview();
    }
}

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), "{APP_VERSION}");
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!(
                "Караоке-Видео Генератор v{} (Word-Level)",
                APP_VERSION
            ))
            .with_icon(app_icon())
            .with_inner_size([1160.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        &format!("Караоке-Видео Генератор v{}", APP_VERSION),
        options,
        Box::new(|cc| Ok(Box::new(KaraokeApp::new(cc)))),
    )
}

fn parse_log_line_and_update_status(log_line: &str, tracks: &mut [TrackItem]) {
    if log_line.contains("[*] [") {
        if let Some(pos) = extract_track_num_between(log_line, "[*] [", "]") {
            update_status_by_pos(tracks, pos, TrackStatus::Downloading);
        }
    } else if log_line.contains("[#] Track ") {
        if let Some(pos) = extract_track_num_after(log_line, "[#] Track ") {
            update_status_by_pos(tracks, pos, TrackStatus::Skipped);
        }
    } else if log_line.contains("[+] [") {
        if let Some(pos) = extract_track_num_between(log_line, "[+] [", "]") {
            update_status_by_pos(tracks, pos, TrackStatus::Success);
        }
    } else if log_line.contains("[?] [") && log_line.contains("Download quality:") {
        if let Some(pos) = extract_track_num_between(log_line, "[?] [", "]") {
            update_download_quality_by_pos(tracks, pos, log_line);
        }
    } else if log_line.contains("[-] [") {
        if let Some(pos) = extract_track_num_between(log_line, "[-] [", "]") {
            update_status_by_pos(tracks, pos, TrackStatus::Failed);
        }
    } else if log_line.contains("[-] Thread error processing track ")
        && let Some(pos) = extract_track_num_after(log_line, "[-] Thread error processing track ")
    {
        update_status_by_pos(tracks, pos, TrackStatus::Failed);
    }
}

fn extract_track_num_between(s: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let start_idx = s.find(prefix)? + prefix.len();
    let sub = &s[start_idx..];
    let end_idx = sub.find(suffix)?;
    sub[..end_idx].trim().parse::<usize>().ok()
}

fn extract_track_num_after(s: &str, prefix: &str) -> Option<usize> {
    let start_idx = s.find(prefix)? + prefix.len();
    let sub = &s[start_idx..];
    let digits: String = sub.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<usize>().ok()
}

fn update_status_by_pos(tracks: &mut [TrackItem], pos: usize, status: TrackStatus) {
    if let Some(t) = tracks.iter_mut().find(|t| t.pos == pos) {
        t.status = status;
    }
}

fn update_download_quality_by_pos(tracks: &mut [TrackItem], pos: usize, log_line: &str) {
    if let Some(t) = tracks.iter_mut().find(|t| t.pos == pos) {
        let detail = log_line
            .split_once("Download quality:")
            .map(|(_, tail)| tail.trim())
            .unwrap_or(log_line)
            .to_string();
        t.quality_note = Some(humanize_download_quality_note(&detail));
        if detail.starts_with("suspicious") {
            t.status = TrackStatus::Warning;
        }
    }
}

fn humanize_download_quality_note(detail: &str) -> String {
    let reasons = detail
        .split_once(';')
        .map(|(_, tail)| tail.trim())
        .unwrap_or(detail);
    if reasons == "clean_match" || detail.starts_with("ok") {
        return "Совпадение подтверждено".to_string();
    }

    reasons
        .split(',')
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| {
            if let Some(value) = reason.strip_prefix("actual_duration_diff:") {
                format!(
                    "Фактическая длительность отличается на {} сек.",
                    value.trim_end_matches('s')
                )
            } else if let Some(value) = reason.strip_prefix("title_match:") {
                format!("Совпадение названия: {value}%")
            } else if let Some(value) = reason.strip_prefix("artist_match:") {
                format!("Совпадение исполнителя: {value}%")
            } else if let Some(value) = reason.strip_prefix("low_score:") {
                format!("Низкая общая уверенность: {value}%")
            } else if let Some(value) = reason.strip_prefix("fallback_score:") {
                format!("Низкая уверенность резервного поиска: {value}")
            } else if reason == "duration_unknown" {
                "Не удалось подтвердить длительность".to_string()
            } else if let Some(value) = reason.strip_prefix("critical_marker:") {
                format!("Обнаружена другая версия: {value}")
            } else if let Some(value) = reason.strip_prefix("source:") {
                format!("Источник: {value}")
            } else if let Some(value) = reason.strip_prefix("candidate:") {
                format!("Выбранный вариант: {value}")
            } else {
                reason.replace('_', " ")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn moving_trim_start_past_end_keeps_end_fixed() {
        assert_eq!(clamp_trim_window(9000, 6000, 10000), (5000, 6000));
        assert_eq!(clamp_trim_window(2500, 6000, 10000), (2500, 6000));
        assert_eq!(clamp_trim_window(100, 250, 250), (0, 250));
    }

    #[test]
    fn loudness_settings_migrate_and_round_trip() {
        let mut settings: AppSettings = serde_json::from_str(r#"{"quality":"high"}"#).unwrap();
        assert!(settings.normalize_audio);
        assert_eq!(settings.loudness_target_db, 89);
        settings.normalize_audio = false;
        settings.loudness_target_db = 92;
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn suspicious_source_markers_detect_live_versions() {
        let markers = suspicious_source_markers("Artist - Song Live Version.mp3");
        assert!(markers.contains(&"live"));
        assert!(markers.contains(&"version"));
    }

    #[test]
    fn suspicious_source_markers_ignore_clean_names() {
        assert!(suspicious_source_markers("Artist - Song.mp3").is_empty());
    }

    #[test]
    fn parses_safe_audio_search_report() {
        let json = r#"{
            "track":{"title":"Искала","artists":["Земфира"],"duration_sec":214},
            "providers":[{"provider":"youtube","status":"ok","candidates":1,"error":null}],
            "selected":{"rank":1,"score":91.1,"source":"youtube","title":"Official Video","artists":["Zemfira"],"duration_sec":211,"public_url":"https://example.com","public_url_ok":true,"flags":[]},
            "candidates":[],
            "safe_mode":{"download_performed":false,"direct_media_url_requested":false}
        }"#;

        let report: AudioSearchReport = serde_json::from_str(json).unwrap();

        assert_eq!(report.track.title, "Искала");
        assert_eq!(report.selected.unwrap().public_url_ok, Some(true));
        assert!(!report.safe_mode.download_performed);
    }

    #[test]
    fn humanizes_download_quality_warning() {
        let note = humanize_download_quality_note(
            "suspicious; actual_duration_diff:18s, source:youtube-search",
        );
        assert!(note.contains("Фактическая длительность отличается на 18 сек."));
        assert!(note.contains("Источник: youtube-search"));
    }

    #[test]
    fn responsive_panel_split_never_exceeds_available_width() {
        for width in [320.0, 640.0, 900.0, 1800.0] {
            let gap = 18.0;
            let (left, right) = split_panel_widths(width, gap, 0.38);
            assert!(left >= 0.0 && right >= 0.0);
            assert!((left + gap + right - width).abs() < 0.01);
        }
    }
}
