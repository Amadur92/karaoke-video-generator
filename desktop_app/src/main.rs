#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Скрывает консоль на Windows в релиз-сборке

use eframe::egui;
use rodio::Source;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// Шрифты Montserrat вкомпилированы прямо в бинарник — нулевая зависимость от внешних файлов
const MONTSERRAT_REGULAR: &[u8] = include_bytes!("../assets/Montserrat-Regular.ttf");
const MONTSERRAT_BOLD: &[u8] = include_bytes!("../assets/Montserrat-Bold.ttf");
const MONTSERRAT_BLACK: &[u8] = include_bytes!("../assets/Montserrat-Black.ttf");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
fn hide_subprocess_window(cmd: &mut std::process::Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_subprocess_window(_cmd: &mut std::process::Command) {}

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

/// Вычисляет путь к файлу настроек в домашней директории пользователя
fn settings_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("karaoke-generator");
    let _ = std::fs::create_dir_all(&p);
    p.push("settings.json");
    p
}

fn app_data_dir() -> PathBuf {
    let mut p = dirs::data_dir()
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    p.push("karaoke-generator");
    let _ = std::fs::create_dir_all(&p);
    p
}

fn debug_log(message: impl AsRef<str>) {
    let path = app_data_dir().join("karaoke_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", message.as_ref());
    }
}

/// Вычисляет рабочую директорию рядом с исполняемым файлом
fn app_base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn executable_name(base_name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.exe", base_name)
    } else {
        base_name.to_string()
    }
}

/// Ищет bundled worker рядом с приложением или Python-скрипт в dev-режиме.
fn find_worker() -> Option<PathBuf> {
    let base = app_base_dir();
    let worker_exe = executable_name("karaoke_worker");
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../worker/karaoke_worker.py"),
        base.join(&worker_exe),
        base.join("worker").join(&worker_exe),
        base.join("../Resources/worker").join(&worker_exe),
        base.join("karaoke_worker.py"),
        base.join("worker/karaoke_worker.py"),
        base.join("../../../worker/karaoke_worker.py"),
        base.join("../../../../worker/karaoke_worker.py"),
        PathBuf::from("/Users/mihailsokolenko/wow_quiz/worker/karaoke_worker.py"),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

fn find_rust_renderer() -> Option<PathBuf> {
    let base = app_base_dir();
    let renderer_exe = executable_name("karaoke_render");
    let candidates = [
        base.join(&renderer_exe),
        base.join("worker").join(&renderer_exe),
        base.join("../Resources/worker").join(&renderer_exe),
        base.join("../release").join(&renderer_exe),
        base.join("../../../target/release").join(&renderer_exe),
        base.join("../../../desktop_app/target/release")
            .join(&renderer_exe),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

fn is_python_worker(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("py"))
        .unwrap_or(false)
}

fn bundled_bin_dir() -> Option<PathBuf> {
    let base = app_base_dir();
    let candidates = [
        base.join("bin"),
        base.join("../Resources/bin"),
        base.join("../../../packaging/bin"),
    ];

    candidates
        .into_iter()
        .find(|candidate| candidate.exists() && candidate.is_dir())
}

fn tool_name(base_name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.exe", base_name)
    } else {
        base_name.to_string()
    }
}

fn tool_path(base_name: &str) -> PathBuf {
    let name = tool_name(base_name);
    bundled_bin_dir()
        .map(|dir| dir.join(&name))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) {
    if path.exists() {
        let _ = std::process::Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(path)
            .status();
    }
}

#[cfg(not(target_os = "macos"))]
fn clear_quarantine(_path: &Path) {}

fn clear_bundled_runtime_quarantine() {
    if let Some(bin_dir) = bundled_bin_dir() {
        clear_quarantine(&bin_dir);
    }

    let base = app_base_dir();
    clear_quarantine(&base.join("worker"));
}

fn format_time_ms(ms: i64) -> String {
    let total_seconds = (ms.max(0) as f32 / 1000.0).round() as i64;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}", minutes, seconds)
}

fn parse_ffmpeg_hms_ms(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours = parts[0].parse::<f64>().ok()?;
    let minutes = parts[1].parse::<f64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;
    Some(((hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0).round() as i64)
}

fn parse_ffmpeg_time_ms(line: &str) -> Option<i64> {
    if let Some(value) = line.strip_prefix("out_time_us=") {
        return value.trim().parse::<i64>().ok().map(|v| v / 1000);
    }

    if let Some(value) = line.strip_prefix("out_time_ms=") {
        return value.trim().parse::<i64>().ok().map(|v| v / 1000);
    }

    if let Some(value) = line.strip_prefix("out_time=") {
        return parse_ffmpeg_hms_ms(value.trim());
    }

    let start = line.find("time=")? + "time=".len();
    let value = line[start..].split_whitespace().next()?;
    parse_ffmpeg_hms_ms(value)
}

fn is_ffmpeg_progress_key(line: &str) -> bool {
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "dup_frames"
            | "drop_frames"
            | "speed"
            | "progress"
    ) || key.starts_with("stream_")
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

fn probe_audio_duration_ms(path: &str) -> Result<i64, String> {
    let mut cmd = std::process::Command::new(tool_path("ffprobe"));
    hide_subprocess_window(&mut cmd);
    let output = cmd
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path,
        ])
        .output()
        .map_err(|e| format!("Не удалось запустить ffprobe: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe не смог прочитать файл: {}", err.trim()));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let seconds = raw
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Не удалось прочитать длительность аудио: {}", e))?;
    Ok((seconds * 1000.0).round() as i64)
}

fn render_trimmed_audio(
    input: &str,
    start_ms: i64,
    end_ms: i64,
    fade_in_ms: i64,
    fade_out_ms: i64,
    output: &Path,
) -> Result<(), String> {
    let duration_ms = end_ms - start_ms;
    if duration_ms < 1000 {
        return Err("Оставьте хотя бы 1 секунду аудио после обрезки.".to_string());
    }

    let max_fade_ms = (duration_ms / 2).max(0);
    let fade_in_ms = fade_in_ms.clamp(0, max_fade_ms);
    let fade_out_ms = fade_out_ms.clamp(0, max_fade_ms);
    let mut audio_filters = Vec::new();

    if fade_in_ms > 0 {
        audio_filters.push(format!(
            "afade=t=in:st=0:d={:.3}",
            fade_in_ms as f64 / 1000.0
        ));
    }
    if fade_out_ms > 0 {
        let fade_out_start_ms = (duration_ms - fade_out_ms).max(0);
        audio_filters.push(format!(
            "afade=t=out:st={:.3}:d={:.3}",
            fade_out_start_ms as f64 / 1000.0,
            fade_out_ms as f64 / 1000.0
        ));
    }

    let mut cmd = std::process::Command::new(tool_path("ffmpeg"));
    hide_subprocess_window(&mut cmd);
    cmd.arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", start_ms as f64 / 1000.0))
        .arg("-t")
        .arg(format!("{:.3}", duration_ms as f64 / 1000.0))
        .arg("-i")
        .arg(input)
        .arg("-vn");

    if !audio_filters.is_empty() {
        cmd.arg("-af").arg(audio_filters.join(","));
    }

    let status = cmd
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ar")
        .arg("44100")
        .arg("-ac")
        .arg("2")
        .arg(output)
        .status()
        .map_err(|e| format!("Не удалось запустить ffmpeg: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("ffmpeg не смог создать обрезанный аудиофайл.".to_string())
    }
}

fn probe_video_size(path: &str) -> Result<(usize, usize), String> {
    let mut cmd = std::process::Command::new(tool_path("ffprobe"));
    hide_subprocess_window(&mut cmd);
    let output = cmd
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=s=x:p=0",
            path,
        ])
        .output()
        .map_err(|e| format!("Не удалось запустить ffprobe: {}", e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe не смог прочитать видео: {}", err.trim()));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut parts = raw.trim().split('x');
    let width = parts
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .ok_or_else(|| "Не удалось прочитать ширину видео.".to_string())?;
    let height = parts
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .ok_or_else(|| "Не удалось прочитать высоту видео.".to_string())?;

    Ok((width.max(1), height.max(1)))
}

fn preview_video_size(path: &str) -> Result<(usize, usize), String> {
    let (width, height) = probe_video_size(path)?;
    let target_width = width.min(720).max(2);
    if target_width == width {
        return Ok((width, height));
    }

    let scaled_height = ((height as f32 * target_width as f32 / width as f32).round() as usize)
        .max(2)
        .next_multiple_of(2);
    Ok((target_width, scaled_height))
}

fn render_video_preview_audio(input: &str, output: &Path, start_ms: i64) -> Result<(), String> {
    let mut cmd = std::process::Command::new(tool_path("ffmpeg"));
    hide_subprocess_window(&mut cmd);
    let status = cmd
        .arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", start_ms.max(0) as f64 / 1000.0))
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ar")
        .arg("44100")
        .arg("-ac")
        .arg("2")
        .arg(output)
        .status()
        .map_err(|e| format!("Не удалось запустить ffmpeg: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("ffmpeg не смог подготовить звук для предпросмотра.".to_string())
    }
}

/// Путь к папке экспорта видео
fn exports_dir() -> PathBuf {
    let base = app_base_dir();
    let portable_exports = base.join("exports");
    let exports = if base.join("worker").exists() && base.join("bin").exists() {
        portable_exports
    } else {
        app_data_dir().join("exports")
    };
    let _ = std::fs::create_dir_all(&exports);
    exports
}

/// Путь к папке для временных файлов
fn temp_dir() -> PathBuf {
    let temp = app_data_dir().join("tmp");
    let _ = std::fs::create_dir_all(&temp);
    temp
}

fn upload_dir() -> PathBuf {
    let uploads = app_data_dir().join("uploads");
    let _ = std::fs::create_dir_all(&uploads);
    uploads
}

fn file_extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn is_audio_file(path: &Path) -> bool {
    matches!(file_extension_lower(path).as_deref(), Some("mp3"))
}

fn is_lyrics_file(path: &Path) -> bool {
    matches!(file_extension_lower(path).as_deref(), Some("txt" | "lrc"))
}

fn display_file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn read_lyrics_file(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|err| {
        format!(
            "Не удалось прочитать файл текста {}: {}",
            display_file_name(path),
            err
        )
    })?;

    Ok(text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

fn parse_lrc_timestamp_ms(raw: &str) -> Option<i64> {
    let (minutes, rest) = raw.split_once(':')?;
    let minutes = minutes.trim().parse::<i64>().ok()?;
    let seconds = rest.trim().parse::<f64>().ok()?;
    Some(minutes * 60_000 + (seconds * 1000.0).round() as i64)
}

fn format_lrc_timestamp_ms(ms: i64) -> String {
    let ms = ms.max(0);
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let centiseconds = (ms % 1000) / 10;
    format!("{:02}:{:02}.{:02}", minutes, seconds, centiseconds)
}

fn shift_lrc_for_trim(lyrics: &str, trim_start_ms: i64, trim_duration_ms: i64) -> String {
    if !lyrics.contains('[') {
        return lyrics.to_string();
    }

    let trim_end_ms = trim_duration_ms.max(0);
    let mut shifted_lines = Vec::new();

    for raw_line in lyrics.lines() {
        let mut rest = raw_line;
        let mut timestamps = Vec::new();

        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end_idx) = stripped.find(']') else {
                break;
            };
            let tag = &stripped[..end_idx];
            if let Some(time_ms) = parse_lrc_timestamp_ms(tag) {
                let shifted = time_ms - trim_start_ms;
                if shifted >= 0 && shifted <= trim_end_ms + 250 {
                    timestamps.push(format!("[{}]", format_lrc_timestamp_ms(shifted)));
                }
            } else {
                timestamps.push(format!("[{}]", tag));
            }
            rest = &stripped[end_idx + 1..];
        }

        if raw_line.trim_start().starts_with('[') {
            if !timestamps.is_empty() {
                shifted_lines.push(format!("{}{}", timestamps.join(""), rest));
            }
        } else {
            shifted_lines.push(raw_line.to_string());
        }
    }

    shifted_lines.join("\n")
}

fn parse_artist_title_from_stem(stem: &str) -> (String, String) {
    let cleaned = stem
        .split_once(". ")
        .and_then(|(prefix, rest)| prefix.parse::<usize>().ok().map(|_| rest))
        .unwrap_or(stem)
        .trim();

    if let Some((artist, title)) = cleaned.split_once(" - ") {
        (artist.trim().to_string(), title.trim().to_string())
    } else {
        (String::new(), cleaned.to_string())
    }
}

fn folder_sort_key(path: &Path) -> (usize, String) {
    let name = display_file_name(path);
    let number = name
        .split_once('.')
        .and_then(|(prefix, _)| prefix.trim().parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    (number, name.to_lowercase())
}

fn find_matching_lyrics(audio_path: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    let audio_stem = audio_path.file_stem()?.to_string_lossy();
    let mut text_files: Vec<PathBuf> = files
        .iter()
        .filter(|path| is_lyrics_file(path))
        .cloned()
        .collect();
    text_files.sort_by_key(|path| {
        let ext_priority = match file_extension_lower(path).as_deref() {
            Some("lrc") => 0,
            Some("txt") => 1,
            _ => 2,
        };
        let stem_match = path
            .file_stem()
            .map(|stem| stem.to_string_lossy() == audio_stem)
            .unwrap_or(false);
        (
            !stem_match,
            ext_priority,
            display_file_name(path).to_lowercase(),
        )
    });
    text_files.into_iter().next()
}

fn scan_batch_folder(
    root: &Path,
    fade_in_ms: i32,
    fade_out_ms: i32,
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
    folders.sort_by_key(|path| folder_sort_key(path));

    let mut items = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            warnings.push(format!(
                "Не удалось прочитать папку {}",
                display_file_name(&folder)
            ));
            continue;
        };
        let files: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        let mut audio_files: Vec<PathBuf> = files
            .iter()
            .filter(|path| is_audio_file(path))
            .cloned()
            .collect();
        audio_files.sort_by_key(|path| display_file_name(path).to_lowercase());
        if audio_files.is_empty() {
            continue;
        }

        for audio_path in audio_files {
            let duration_ms = match probe_audio_duration_ms(&audio_path.to_string_lossy()) {
                Ok(duration) => duration,
                Err(err) => {
                    warnings.push(format!(
                        "{}: не удалось прочитать длительность ({})",
                        display_file_name(&audio_path),
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
            let (artist, title) = parse_artist_title_from_stem(&stem);
            let (lyrics_path, status) = match find_matching_lyrics(&audio_path, &files) {
                Some(path) => (path, BatchStatus::Ready),
                None => {
                    warnings.push(format!(
                        "{}: найдено аудио, но нет .lrc/.txt",
                        display_file_name(&folder)
                    ));
                    (PathBuf::new(), BatchStatus::MissingLyrics)
                }
            };
            items.push(BatchItem {
                folder: folder.clone(),
                audio_path,
                lyrics_path,
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
            });
        }
    }

    (items, warnings)
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

fn batch_output_path(item: &BatchItem) -> PathBuf {
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
    item.folder.join(safe_output_filename(&format!(
        "{} - {} (karaoke).mp4",
        artist, title
    )))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
struct AppSettings {
    model: String,
    quality: String,
    font: String,
    color_active: [u8; 3],
    color_inactive: [u8; 3],
    color_bg: [u8; 3],
    inactive_opacity: f32,
    audio_delay_ms: i32,
    fade_in_ms: i32,
    fade_out_ms: i32,
    artist: String,
    title: String,
    lyrics: String,
    plain_lines: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model: "base".to_string(),
            quality: "medium".to_string(),
            font: "montserrat".to_string(),
            color_active: [0, 0, 0],
            color_inactive: [180, 185, 195],
            color_bg: [255, 255, 255],
            inactive_opacity: 0.65,
            audio_delay_ms: 0,
            fade_in_ms: 0,
            fade_out_ms: 0,
            artist: String::new(),
            title: String::new(),
            lyrics: String::new(),
            plain_lines: false,
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
}

enum ProgressUpdate {
    Progress(CLIProgress),
    BatchProgress {
        index: usize,
        status: BatchStatus,
        progress: f32,
        message: String,
        output_path: Option<String>,
    },
    RawLog(String),
    Error(String),
    Finished(bool),
    BatchFinished,
}

struct VideoFrame {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

struct AudioLoadUpdate {
    path: String,
    result: Result<i64, String>,
}

#[derive(Clone, Debug, PartialEq)]
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
struct BatchItem {
    folder: PathBuf,
    audio_path: PathBuf,
    lyrics_path: PathBuf,
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
        let wave = ((ratio * 18.0).sin().abs() * 0.55
            + (ratio * 47.0).sin().abs() * 0.30
            + (ratio * 91.0).sin().abs() * 0.15)
            .clamp(0.15, 1.0);
        let amp = max_amp * wave;
        let waveform_color = if selected_rect.contains(egui::pos2(x, center_y)) {
            egui::Color32::from_rgb(72, 222, 226)
        } else {
            egui::Color32::from_rgb(43, 94, 103)
        };
        painter.line_segment(
            [egui::pos2(x, center_y - amp), egui::pos2(x, center_y + amp)],
            egui::Stroke::new(2.0, waveform_color),
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
        egui::Stroke::new(1.0, egui::Color32::from_rgb(85, 225, 231)),
    );

    let start_x = to_x(start_ms);
    let end_x = to_x(end_ms);
    let draw_label = |x: f32, y: f32, value: String, color: egui::Color32| {
        let width = (value.chars().count() as f32 * 6.2 + 12.0).max(42.0);
        let label_rect = egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(width, 18.0));
        painter.rect_filled(label_rect, 5.0, egui::Color32::from_rgb(28, 33, 43));
        painter.rect_stroke(label_rect, 5.0, egui::Stroke::new(1.0, color));
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
        format_time_ms(start_ms),
        success,
    );
    draw_label(
        end_x,
        track_rect.top() - 14.0,
        format_time_ms(end_ms),
        success,
    );

    painter.line_segment(
        [
            egui::pos2(track_rect.left(), track_rect.bottom() + 10.0),
            egui::pos2(track_rect.right(), track_rect.bottom() + 10.0),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_rgb(39, 47, 60)),
    );
    for tick in 0..=4 {
        let x = track_rect.left() + track_width * tick as f32 / 4.0;
        painter.line_segment(
            [
                egui::pos2(x, track_rect.bottom() + 14.0),
                egui::pos2(x, track_rect.bottom() + 18.0),
            ],
            egui::Stroke::new(1.0, muted),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackStatus {
    Pending,
    Downloading,
    Success,
    Failed,
    Skipped,
}

impl Default for TrackStatus {
    fn default() -> Self {
        Self::Pending
    }
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

    // Цвета в формате RGB [r, g, b]
    color_active: [u8; 3],
    color_inactive: [u8; 3],
    color_bg: [u8; 3],
    inactive_opacity: f32,

    // Сдвиг аудио (мс)
    audio_delay_ms: i32,

    // Статус выполнения
    is_generating: bool,
    progress: f32,
    status_text: String,
    log_output: String,

    // Канал получения прогресса из фонового потока
    rx: Option<Receiver<ProgressUpdate>>,
    audio_rx: Option<Receiver<AudioLoadUpdate>>,

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
    active_tab: ActiveTab,
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
    dl_child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    batch_start_time: Option<Instant>,
    dl_start_time: Option<Instant>,
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
        visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);

        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(226, 232, 240));
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);

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
        cc.egui_ctx.set_fonts(fonts);

        // 3. Загрузка сохраненных настроек
        let settings: AppSettings = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default();

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
            font: settings.font,
            color_active: settings.color_active,
            color_inactive: settings.color_inactive,
            color_bg: settings.color_bg,
            inactive_opacity: settings.inactive_opacity.clamp(0.0, 1.0),
            audio_delay_ms: settings.audio_delay_ms,
            is_generating: false,
            progress: 0.0,
            status_text: "Готов к работе".to_string(),
            log_output: String::new(),
            rx: None,
            audio_rx: None,
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
            batch_root: None,
            batch_items: Vec::new(),
            batch_running: false,
            batch_stop_requested: false,
            batch_current_index: None,
            batch_selected_index: None,
            batch_status_text: String::new(),
            batch_is_scanning: false,
            batch_scan_rx: None,
            batch_single_mode: false,
            batch_cancel: Arc::new(AtomicBool::new(false)),
            active_tab: ActiveTab::SingleTrack,
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
            dl_child: std::sync::Arc::new(std::sync::Mutex::new(None)),
            batch_start_time: None,
            dl_start_time: None,
        }
    }

    fn is_preview_playing(&self) -> bool {
        self.preview_sink
            .as_ref()
            .map(|sink| !sink.empty())
            .unwrap_or(false)
    }

    fn stop_preview(&mut self) {
        if let Some(sink) = self.preview_sink.take() {
            sink.stop();
        }
        self.preview_stream = None;
        self.preview_started_at = None;
        self.preview_started_ms = 0;
        self.preview_end_ms = 0;
    }

    fn play_audio_preview(
        &mut self,
        path: &Path,
        start_ms: i64,
        end_ms: i64,
        skip_ms: i64,
    ) -> Result<(), String> {
        self.stop_preview();

        let file = std::fs::File::open(path)
            .map_err(|e| format!("Не удалось открыть preview-аудио: {}", e))?;
        let source = rodio::Decoder::new(std::io::BufReader::new(file))
            .map_err(|e| format!("Не удалось декодировать preview-аудио: {}", e))?;
        let (stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| format!("Не удалось открыть аудиовыход: {}", e))?;
        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| format!("Не удалось создать аудио-плеер: {}", e))?;

        sink.append(source.skip_duration(Duration::from_millis(skip_ms.max(0) as u64)));
        sink.play();

        self.preview_stream = Some(stream);
        self.preview_sink = Some(sink);
        self.preview_started_at = Some(Instant::now());
        self.preview_started_ms = start_ms;
        self.preview_end_ms = end_ms;
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
            format_time_ms(self.video_position_ms.min(self.video_duration_ms))
        );
    }

    fn start_video_preview(&mut self, path: &str, ctx: &egui::Context) -> Result<(), String> {
        self.halt_video_preview();
        self.video_status = "Готовим встроенный предпросмотр...".to_string();

        if self.video_duration_ms <= 0 {
            self.video_duration_ms = probe_audio_duration_ms(path).unwrap_or(0);
        }
        if self.video_duration_ms > 0 && self.video_position_ms >= self.video_duration_ms - 250 {
            self.video_position_ms = 0;
        }
        let start_ms = self
            .video_position_ms
            .clamp(0, self.video_duration_ms.max(0));

        let (width, height) = preview_video_size(path)?;
        let audio_path = temp_dir().join("karaoke_video_preview.wav");
        render_video_preview_audio(path, &audio_path, start_ms)?;

        let audio_file = std::fs::File::open(&audio_path)
            .map_err(|e| format!("Не удалось открыть звук видео: {}", e))?;
        let source = rodio::Decoder::new(std::io::BufReader::new(audio_file))
            .map_err(|e| format!("Не удалось декодировать звук видео: {}", e))?;
        let (stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| format!("Не удалось открыть аудиовыход: {}", e))?;
        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| format!("Не удалось создать аудио-плеер: {}", e))?;

        let (tx, rx) = channel::<VideoFrame>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let ctx_thread = ctx.clone();
        let path = path.to_string();

        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(tool_path("ffmpeg"));
            hide_subprocess_window(&mut cmd);
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
                return;
            };

            let frame_len = width.saturating_mul(height).saturating_mul(4);
            let frame_duration = Duration::from_millis(1000 / 24);
            let started_at = Instant::now();
            let mut frame_index = 0u32;

            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    return;
                }

                let mut pixels = vec![0u8; frame_len];
                if stdout.read_exact(&mut pixels).is_err() {
                    break;
                }

                let target_at = started_at + frame_duration * frame_index;
                let now = Instant::now();
                if target_at > now {
                    std::thread::sleep(target_at - now);
                }

                if tx
                    .send(VideoFrame {
                        width,
                        height,
                        pixels,
                    })
                    .is_err()
                {
                    let _ = child.kill();
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

    fn sync_video_preview(&mut self, ctx: &egui::Context) {
        if let Some(started_at) = self.video_started_at {
            let elapsed_ms = started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
            self.video_position_ms = (self.video_started_ms + elapsed_ms)
                .min(self.video_duration_ms.max(self.video_started_ms));
        }

        if let Some(rx) = &self.video_rx {
            let mut got_frame = false;
            while let Ok(frame) = rx.try_recv() {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width, frame.height],
                    &frame.pixels,
                );
                let texture = self.video_texture.get_or_insert_with(|| {
                    ctx.load_texture("video_preview", image.clone(), egui::TextureOptions::LINEAR)
                });
                texture.set(image, egui::TextureOptions::LINEAR);
                got_frame = true;
            }
            if got_frame {
                ctx.request_repaint();
            }
        }

        if let Some(sink) = &self.video_sink {
            if !sink.empty()
                && (self.video_duration_ms <= 0 || self.video_position_ms < self.video_duration_ms)
            {
                ctx.request_repaint_after(Duration::from_millis(33));
                return;
            }
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
            let result = probe_audio_duration_ms(&path_str);
            let _ = tx.send(AudioLoadUpdate {
                path: path_str,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn set_lyrics_file(&mut self, path: PathBuf) {
        match read_lyrics_file(&path) {
            Ok(lyrics) => {
                let file_name = display_file_name(&path);
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

        if path.is_dir() {
            self.load_batch_folder(path, ctx);
        } else if is_audio_file(&path) {
            self.set_audio_file(path, ctx);
        } else if is_lyrics_file(&path) {
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
                display_file_name(&path)
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
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let (items, warnings) = scan_batch_folder(&path, fade_in, fade_out);
            let first_folder_name = display_file_name(&path);
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
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let mut items = Vec::new();
            let mut warnings = Vec::new();
            for path in &paths {
                let (mut folder_items, mut folder_warnings) =
                    scan_batch_folder(path, fade_in, fade_out);
                items.append(&mut folder_items);
                warnings.append(&mut folder_warnings);
            }
            items.sort_by_key(|item| folder_sort_key(&item.folder));

            let first_folder_name = paths
                .first()
                .map(|p| display_file_name(p))
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

    fn apply_batch_item_to_single_state(&mut self, index: usize) -> Result<(), String> {
        let item = self
            .batch_items
            .get(index)
            .cloned()
            .ok_or_else(|| "Задание не найдено".to_string())?;
        let lyrics = if item.status == BatchStatus::MissingLyrics
            || item.lyrics_path.as_os_str().is_empty()
        {
            String::new()
        } else {
            read_lyrics_file(&item.lyrics_path)?
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
        self.trim_status = format!("Длительность: {}", format_time_ms(item.duration_ms));
        self.artist = item.artist;
        self.title = item.title;
        self.lyrics = lyrics;
        self.generated_file = None;
        Ok(())
    }

    fn start_parsing_excel(&mut self, path: PathBuf, ctx: egui::Context) {
        self.dl_is_parsing_excel = true;
        self.dl_tracks.clear();
        self.dl_status_text = "Чтение файла Excel...".to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        self.dl_parse_rx = Some(rx);

        let worker_path = match find_worker() {
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
            let mut cmd = if is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            cmd.env("PYTHONUTF8", "1");
            cmd.arg("parse-sheet").arg(&path);

            #[cfg(windows)]
            cmd.creation_flags(0x08000000);

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

    fn start_download(&mut self, ctx: &egui::Context, continue_only: bool) {
        if self.dl_is_running {
            return;
        }

        self.dl_stop_requested = false;
        self.dl_start_time = Some(Instant::now());
        let mode_excel = self.dl_mode_excel;
        let query = self.dl_track_query.trim().to_string();
        let excel_path = self.dl_excel_path.clone();
        let output_dir = self.dl_output_dir.clone();
        let limit = self.dl_limit_candidates;
        let format = self.dl_format.clone();
        let overwrite = self.dl_overwrite;
        let max_workers = self.dl_max_workers;

        let output_path = match output_dir {
            Some(p) => p,
            None => {
                self.dl_status_text = "Ошибка: не выбрана папка сохранения".to_string();
                return;
            }
        };

        let mut tracks_file_path = None;
        if mode_excel {
            let selected_tracks: Vec<TrackItem> = self
                .dl_tracks
                .iter()
                .filter(|t| {
                    if continue_only {
                        t.selected
                            && t.status != TrackStatus::Success
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
                        && t.status != TrackStatus::Skipped
                    {
                        t.status = TrackStatus::Pending;
                    }
                } else {
                    if t.selected {
                        t.status = TrackStatus::Pending;
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

        let worker_path = match find_worker() {
            Some(w) => w,
            None => {
                self.dl_status_text =
                    "Ошибка: не удалось найти исполняемый файл воркера (karaoke_worker)"
                        .to_string();
                return;
            }
        };

        self.dl_is_running = true;
        self.dl_status_text = "Запуск процесса загрузки...".to_string();
        self.dl_log_output.push_str("🚀 Запуск загрузчика...\n");

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        self.dl_rx = Some(rx);

        let ctx_clone = ctx.clone();
        let dl_child_clone = self.dl_child.clone();

        std::thread::spawn(move || {
            let mut cmd = if is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            cmd.env("PYTHONUTF8", "1");

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
                    "🎵 Одиночный режим.\nТрек: {}\nПапка сохранения: {}",
                    query,
                    output_path.to_string_lossy()
                ));
            }

            #[cfg(windows)]
            cmd.creation_flags(0x08000000);

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
                    for line in reader.lines() {
                        if let Ok(l) = line {
                            let _ = tx_err.send(l);
                            ctx_err.request_repaint();
                        }
                    }
                }
            });

            if let Some(out) = stdout {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(out);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = tx.send(l);
                        ctx_clone.request_repaint();
                    }
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
        if let Ok(mut guard) = self.dl_child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
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
            let _ = std::fs::remove_file(batch_timings_path(&item.audio_path));
        }
        self.start_batch_pipeline(ctx, false);
    }

    fn continue_batch_queue(&mut self, ctx: &egui::Context) {
        if self.batch_items.is_empty() || self.is_generating || self.batch_running {
            return;
        }
        for item in &mut self.batch_items {
            if item.status == BatchStatus::Done
                || item.status == BatchStatus::MissingLyrics
                || matches!(item.status, BatchStatus::Error(_))
            {
                continue;
            }
            item.progress = 0.0;
            item.output_path = None;
            item.status = if batch_timings_path(&item.audio_path).exists() {
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
        let worker_path = match find_worker() {
            Some(path) => path,
            None => {
                self.log_output
                    .push_str("❌ Batch: не найден karaoke_worker.\n");
                return;
            }
        };
        let renderer_path = match find_rust_renderer() {
            Some(path) => path,
            None => {
                self.log_output
                    .push_str("❌ Batch: не найден Rust-рендер karaoke_render.\n");
                return;
            }
        };

        self.batch_running = true;
        self.batch_start_time = Some(Instant::now());
        self.batch_stop_requested = false;
        self.batch_current_index = None;
        self.batch_cancel.store(false, Ordering::Relaxed);
        self.progress = 0.0;
        self.status_text = "Batch: подготовка очереди...".to_string();
        self.batch_status_text = if resume {
            "Batch: продолжение очереди...".to_string()
        } else {
            format!("В очереди {} заданий", self.batch_items.len())
        };
        self.log_output.push_str(&format!(
            "🚀 Batch: {} {} заданий через общий align-процесс\n",
            if resume {
                "продолжение"
            } else {
                "запуск"
            },
            self.batch_items.len()
        ));

        let items = self.batch_items.clone();
        let model = self.model.clone();
        let quality = self.quality.clone();
        let font = self.font.clone();
        let active_hex = format!(
            "#{:02X}{:02X}{:02X}",
            self.color_active[0], self.color_active[1], self.color_active[2]
        );
        let inactive_hex = format!(
            "#{:02X}{:02X}{:02X}",
            self.color_inactive[0], self.color_inactive[1], self.color_inactive[2]
        );
        let bg_hex = format!(
            "#{:02X}{:02X}{:02X}",
            self.color_bg[0], self.color_bg[1], self.color_bg[2]
        );
        let inactive_opacity = self.inactive_opacity.clamp(0.0, 1.0);
        let audio_delay_seconds = self.audio_delay_ms as f32 / 1000.0;
        let plain_lines = self.plain_lines;
        let cancel = self.batch_cancel.clone();
        let temp = temp_dir().join("batch");
        let (tx, rx) = channel::<ProgressUpdate>();
        self.rx = Some(rx);
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            #[derive(Serialize)]
            struct AlignQueueEntry {
                index: usize,
                audio: String,
                artist: String,
                title: String,
                lyrics_file: String,
                timings_output: String,
            }

            #[derive(Clone)]
            struct RenderTask {
                index: usize,
                audio_path: PathBuf,
                timings_path: PathBuf,
                output_path: PathBuf,
                duration_ms: i64,
            }

            let _ = std::fs::create_dir_all(&temp);
            let mut align_queue = Vec::new();
            let mut render_tasks = Vec::new();

            for (idx, item) in items.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(ProgressUpdate::RawLog("⏸ Batch остановлен.".to_string()));
                    let _ = tx.send(ProgressUpdate::BatchFinished);
                    ctx.request_repaint();
                    return;
                }

                if item.status == BatchStatus::Done
                    || item.status == BatchStatus::MissingLyrics
                    || matches!(item.status, BatchStatus::Error(_))
                {
                    continue;
                }

                let item_temp = temp.join(format!("{:03}", idx + 1));
                let _ = std::fs::create_dir_all(&item_temp);
                let timings_path = batch_timings_path(&item.audio_path);
                let mut render_audio_path = item.audio_path.clone();
                let mut lyrics_text =
                    std::fs::read_to_string(&item.lyrics_path).unwrap_or_else(|_| String::new());

                let start = item
                    .trim_start_ms
                    .clamp(0, item.duration_ms.saturating_sub(1000));
                let end = item.trim_end_ms.clamp(start + 1000, item.duration_ms);
                let should_trim = start > 0
                    || end < item.duration_ms.saturating_sub(250)
                    || item.fade_in_ms > 0
                    || item.fade_out_ms > 0;

                if should_trim {
                    let trimmed_path = item_temp.join("audio.wav");
                    match render_trimmed_audio(
                        &item.audio_path.to_string_lossy(),
                        start,
                        end,
                        item.fade_in_ms as i64,
                        item.fade_out_ms as i64,
                        &trimmed_path,
                    ) {
                        Ok(()) => {
                            lyrics_text =
                                shift_lrc_for_trim(&lyrics_text, start, end.saturating_sub(start));
                            render_audio_path = trimmed_path;
                        }
                        Err(err) => {
                            let _ = tx.send(ProgressUpdate::BatchProgress {
                                index: idx,
                                status: BatchStatus::Error(err.clone()),
                                progress: 0.0,
                                message: err,
                                output_path: None,
                            });
                            continue;
                        }
                    }
                }

                let lyrics_path = item_temp.join("lyrics.txt");
                if let Err(err) = std::fs::write(&lyrics_path, lyrics_text) {
                    let message = format!("Не удалось подготовить текст: {err}");
                    let _ = tx.send(ProgressUpdate::BatchProgress {
                        index: idx,
                        status: BatchStatus::Error(message.clone()),
                        progress: 0.0,
                        message,
                        output_path: None,
                    });
                    continue;
                }

                if timings_path.exists() {
                    let _ = tx.send(ProgressUpdate::BatchProgress {
                        index: idx,
                        status: BatchStatus::ReadyToRender,
                        progress: 1.0,
                        message: "Тайминги уже есть, Whisper пропущен.".to_string(),
                        output_path: None,
                    });
                } else {
                    align_queue.push(AlignQueueEntry {
                        index: idx,
                        audio: render_audio_path.to_string_lossy().to_string(),
                        artist: item.artist.clone(),
                        title: item.title.clone(),
                        lyrics_file: lyrics_path.to_string_lossy().to_string(),
                        timings_output: timings_path.to_string_lossy().to_string(),
                    });
                }

                render_tasks.push(RenderTask {
                    index: idx,
                    audio_path: render_audio_path,
                    timings_path,
                    output_path: batch_output_path(item),
                    duration_ms: (end - start).max(1),
                });
            }

            if !align_queue.is_empty() {
                let queue_path = temp.join("align_queue.json");
                match serde_json::to_string_pretty(&align_queue)
                    .map_err(|err| err.to_string())
                    .and_then(|data| {
                        std::fs::write(&queue_path, data).map_err(|err| err.to_string())
                    }) {
                    Ok(()) => {}
                    Err(err) => {
                        let _ = tx.send(ProgressUpdate::Error(format!(
                            "Batch: не удалось записать очередь align: {err}"
                        )));
                        let _ = tx.send(ProgressUpdate::BatchFinished);
                        return;
                    }
                }

                let _ = tx.send(ProgressUpdate::RawLog(format!(
                    "🧠 Batch align: {} треков, модель загрузится один раз.",
                    align_queue.len()
                )));

                let mut cmd = if is_python_worker(&worker_path) {
                    let mut cmd = std::process::Command::new("python3");
                    cmd.arg(&worker_path);
                    cmd
                } else {
                    std::process::Command::new(&worker_path)
                };
                hide_subprocess_window(&mut cmd);
                if let Some(bin_dir) = bundled_bin_dir() {
                    let old_path = std::env::var_os("PATH").unwrap_or_default();
                    let mut paths = vec![bin_dir];
                    paths.extend(std::env::split_paths(&old_path));
                    if let Ok(joined) = std::env::join_paths(paths) {
                        cmd.env("PATH", joined);
                    }
                }
                cmd.env("PYTHONUTF8", "1")
                    .arg("--cli")
                    .arg("--batch-align-queue")
                    .arg(&queue_path)
                    .arg("--model")
                    .arg(&model)
                    .arg("--quality")
                    .arg(&quality)
                    .arg("--font")
                    .arg(&font)
                    .arg("--color-active")
                    .arg(&active_hex)
                    .arg("--color-inactive")
                    .arg(&inactive_hex)
                    .arg("--color-bg")
                    .arg(&bg_hex)
                    .arg("--inactive-opacity")
                    .arg(inactive_opacity.to_string())
                    .arg("--audio-delay")
                    .arg(audio_delay_seconds.to_string())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                if plain_lines {
                    cmd.arg("--plain-lines");
                }

                match cmd.spawn() {
                    Ok(mut child) => {
                        let tx_err = tx.clone();
                        let ctx_err = ctx.clone();
                        let stderr_handle = child.stderr.take().map(|stderr| {
                            std::thread::spawn(move || {
                                use std::io::{BufRead, BufReader};
                                let reader = BufReader::new(stderr);
                                for line in reader.lines().map_while(Result::ok) {
                                    let trimmed = line.trim();
                                    if !trimmed.is_empty() {
                                        let _ = tx_err.send(ProgressUpdate::RawLog(format!(
                                            "[LOG] {}",
                                            trimmed
                                        )));
                                        ctx_err.request_repaint();
                                    }
                                }
                            })
                        });

                        if let Some(stdout) = child.stdout.take() {
                            use std::io::{BufRead, BufReader};
                            let reader = BufReader::new(stdout);
                            for line in reader.lines().map_while(Result::ok) {
                                if cancel.load(Ordering::Relaxed) {
                                    let _ = child.kill();
                                    break;
                                }
                                let trimmed = line.trim();
                                if trimmed.starts_with('{') && trimmed.ends_with('}') {
                                    if let Ok(update) = serde_json::from_str::<CLIProgress>(trimmed)
                                    {
                                        if let Some(index) = update.batch_align_index {
                                            let status = if update.error.is_some() {
                                                BatchStatus::Error(
                                                    update
                                                        .error
                                                        .clone()
                                                        .unwrap_or_else(|| "Ошибка".to_string()),
                                                )
                                            } else if update.done {
                                                BatchStatus::ReadyToRender
                                            } else {
                                                BatchStatus::Aligning
                                            };
                                            let _ = tx.send(ProgressUpdate::BatchProgress {
                                                index,
                                                status,
                                                progress: update.progress.clamp(0.0, 1.0),
                                                message: update.status,
                                                output_path: None,
                                            });
                                            ctx.request_repaint();
                                            continue;
                                        }
                                    }
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
                        let success = child.wait().map(|status| status.success()).unwrap_or(false);
                        if !success && !cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(ProgressUpdate::RawLog(
                                "❌ Batch align завершился с ошибкой.".to_string(),
                            ));
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(ProgressUpdate::Error(format!(
                            "Batch: не удалось запустить align worker: {err}"
                        )));
                        let _ = tx.send(ProgressUpdate::BatchFinished);
                        return;
                    }
                }
            }

            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(ProgressUpdate::RawLog("⏸ Batch остановлен.".to_string()));
                let _ = tx.send(ProgressUpdate::BatchFinished);
                ctx.request_repaint();
                return;
            }

            let _ = tx.send(ProgressUpdate::RawLog(
                "🎬 Batch render: рендерим видео по готовым таймингам.".to_string(),
            ));

            for task in render_tasks {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if !task.timings_path.exists() {
                    let _ = tx.send(ProgressUpdate::BatchProgress {
                        index: task.index,
                        status: BatchStatus::Error("Нет файла таймингов".to_string()),
                        progress: 0.0,
                        message: "Нет файла таймингов".to_string(),
                        output_path: None,
                    });
                    continue;
                }

                let _ = tx.send(ProgressUpdate::BatchProgress {
                    index: task.index,
                    status: BatchStatus::Rendering,
                    progress: 0.0,
                    message: "Рендеринг".to_string(),
                    output_path: None,
                });

                let mut render_cmd = std::process::Command::new(&renderer_path);
                hide_subprocess_window(&mut render_cmd);
                if let Some(bin_dir) = bundled_bin_dir() {
                    let old_path = std::env::var_os("PATH").unwrap_or_default();
                    let mut paths = vec![bin_dir];
                    paths.extend(std::env::split_paths(&old_path));
                    if let Ok(joined) = std::env::join_paths(paths) {
                        render_cmd.env("PATH", joined);
                    }
                }
                render_cmd
                    .arg("--timings")
                    .arg(&task.timings_path)
                    .arg("--audio")
                    .arg(&task.audio_path)
                    .arg("--output")
                    .arg(&task.output_path)
                    .arg("--quality")
                    .arg(&quality)
                    .arg("--color-active")
                    .arg(&active_hex)
                    .arg("--color-inactive")
                    .arg(&inactive_hex)
                    .arg("--color-bg")
                    .arg(&bg_hex)
                    .arg("--inactive-opacity")
                    .arg(inactive_opacity.to_string())
                    .arg("--engine")
                    .arg(
                        std::env::var("KARAOKE_RENDER_ENGINE")
                            .unwrap_or_else(|_| "ass".to_string()),
                    )
                    .arg("--audio-delay")
                    .arg(audio_delay_seconds.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped());
                if plain_lines {
                    render_cmd.arg("--plain-lines");
                }

                match render_cmd.spawn() {
                    Ok(mut child) => {
                        if let Some(stderr) = child.stderr.take() {
                            use std::io::{BufRead, BufReader};
                            let reader = BufReader::new(stderr);
                            for line in reader.lines().map_while(Result::ok) {
                                if cancel.load(Ordering::Relaxed) {
                                    let _ = child.kill();
                                    break;
                                }
                                let trimmed = line.trim();
                                let progress = parse_ffmpeg_time_ms(trimmed)
                                    .map(|time_ms| {
                                        (time_ms as f32 / task.duration_ms as f32).clamp(0.0, 0.98)
                                    })
                                    .or_else(|| {
                                        trimmed.strip_prefix("render ").and_then(|percent| {
                                            percent
                                                .trim_end_matches('%')
                                                .parse::<f32>()
                                                .ok()
                                                .map(|value| (value / 100.0).clamp(0.0, 0.98))
                                        })
                                    });
                                if let Some(progress) = progress {
                                    let _ = tx.send(ProgressUpdate::BatchProgress {
                                        index: task.index,
                                        status: BatchStatus::Rendering,
                                        progress,
                                        message: format!(
                                            "Рендеринг: {}%",
                                            (progress * 100.0).round() as i32
                                        ),
                                        output_path: None,
                                    });
                                    ctx.request_repaint();
                                    continue;
                                }
                                if is_ffmpeg_progress_key(trimmed) {
                                    continue;
                                }
                                if !trimmed.is_empty() {
                                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                                        "[Rust] {}",
                                        trimmed
                                    )));
                                }
                            }
                        }
                        let success = child.wait().map(|status| status.success()).unwrap_or(false);
                        if success && !cancel.load(Ordering::Relaxed) {
                            let output = task.output_path.to_string_lossy().to_string();
                            let _ = tx.send(ProgressUpdate::BatchProgress {
                                index: task.index,
                                status: BatchStatus::Done,
                                progress: 1.0,
                                message: "Готово".to_string(),
                                output_path: Some(output),
                            });
                        } else if !cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(ProgressUpdate::BatchProgress {
                                index: task.index,
                                status: BatchStatus::Error(
                                    "Rust-рендер завершился с ошибкой".to_string(),
                                ),
                                progress: 0.0,
                                message: "Rust-рендер завершился с ошибкой".to_string(),
                                output_path: None,
                            });
                        }
                    }
                    Err(err) => {
                        let message = format!("Ошибка запуска Rust-рендера: {err}");
                        let _ = tx.send(ProgressUpdate::BatchProgress {
                            index: task.index,
                            status: BatchStatus::Error(message.clone()),
                            progress: 0.0,
                            message,
                            output_path: None,
                        });
                    }
                }
            }

            let _ = tx.send(ProgressUpdate::BatchFinished);
            ctx.request_repaint();
        });
    }

    fn request_stop_batch(&mut self) {
        self.batch_stop_requested = true;
        self.batch_cancel.store(true, Ordering::Relaxed);
        self.batch_status_text =
            "Остановка запрошена: текущее видео будет завершено, новые не начнутся.".to_string();
        self.log_output
            .push_str("⏸ Batch: остановка после текущего задания.\n");
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
        let mut start = self.trim_start_ms.clamp(0, duration.saturating_sub(1000));
        let mut end = self.trim_end_ms.clamp(1000, duration);
        if end - start < 1000 {
            if start + 1000 <= duration {
                end = start + 1000;
            } else {
                start = duration.saturating_sub(1000);
                end = duration;
            }
        }
        Some((start, end))
    }

    fn normalize_trim_state(&mut self) {
        if let Some(duration) = self.audio_duration_ms {
            let min_gap = 1000;
            self.trim_start_ms = self
                .trim_start_ms
                .clamp(0, duration.saturating_sub(min_gap));
            self.trim_end_ms = self.trim_end_ms.clamp(min_gap, duration);
            if self.trim_end_ms - self.trim_start_ms < min_gap {
                self.trim_end_ms = (self.trim_start_ms + min_gap).min(duration);
                if self.trim_end_ms - self.trim_start_ms < min_gap {
                    self.trim_start_ms = self.trim_end_ms.saturating_sub(min_gap);
                }
            }
            let selected_ms = (self.trim_end_ms - self.trim_start_ms).max(min_gap);
            let max_fade_ms = (selected_ms / 2).min(30_000) as i32;
            self.fade_in_ms = self.fade_in_ms.clamp(0, max_fade_ms);
            self.fade_out_ms = self.fade_out_ms.clamp(0, max_fade_ms);
            self.trim_playhead_ms = self
                .trim_playhead_ms
                .clamp(self.trim_start_ms, self.trim_end_ms);
        }
    }

    fn preview_trimmed_audio(&mut self) {
        let audio_path = match &self.audio_path {
            Some(path) => path.clone(),
            None => return,
        };
        let (start, end) = match self.clamped_trim_bounds() {
            Some(bounds) => bounds,
            None => {
                self.trim_status = "Сначала выберите аудио с читаемой длительностью.".to_string();
                return;
            }
        };
        self.normalize_trim_state();
        let play_start = self.trim_playhead_ms.clamp(start, end.saturating_sub(500));

        let preview_path = temp_dir().join("karaoke_trim_preview.wav");
        match render_trimmed_audio(
            &audio_path,
            start,
            end,
            self.fade_in_ms as i64,
            self.fade_out_ms as i64,
            &preview_path,
        )
        .and_then(|_| self.play_audio_preview(&preview_path, play_start, end, play_start - start))
        {
            Ok(()) => {
                self.trim_status = format!(
                    "Проигрывается предпросмотр: {} - {}",
                    format_time_ms(play_start),
                    format_time_ms(end)
                );
            }
            Err(err) => {
                self.trim_status = err;
            }
        }
    }

    fn trim_timeline_ui(
        &mut self,
        ui: &mut egui::Ui,
        duration_ms: i64,
        accent: egui::Color32,
        success: egui::Color32,
        muted: egui::Color32,
    ) {
        self.normalize_trim_state();

        let desired_size = egui::vec2(ui.available_width(), 86.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());
        let track_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.center().y - 16.0),
            egui::pos2(rect.right() - 8.0, rect.center().y + 18.0),
        );
        let track_width = track_rect.width().max(1.0);

        let to_x = |ms: i64| -> f32 {
            track_rect.left() + (ms as f32 / duration_ms.max(1) as f32) * track_width
        };
        let from_x = |x: f32| -> i64 {
            let ratio = ((x - track_rect.left()) / track_width).clamp(0.0, 1.0);
            (ratio * duration_ms as f32).round() as i64
        };

        if (response.dragged() || response.clicked()) && !self.is_generating {
            if let Some(pointer) = response.interact_pointer_pos() {
                if self.is_preview_playing() {
                    self.stop_preview();
                }

                let selected_ms = (self.trim_end_ms - self.trim_start_ms).max(1000);
                let fade_in_x = to_x(self.trim_start_ms + self.fade_in_ms as i64);
                let fade_out_x = to_x(self.trim_end_ms - self.fade_out_ms as i64);
                let target_ms = from_x(pointer.x);
                let nearest = if track_rect.expand2(egui::vec2(0.0, 10.0)).contains(pointer)
                    && (fade_in_x - pointer.x)
                        .abs()
                        .min((fade_out_x - pointer.x).abs())
                        <= 16.0
                {
                    if (fade_in_x - pointer.x).abs() <= (fade_out_x - pointer.x).abs() {
                        3
                    } else {
                        4
                    }
                } else if pointer.y <= track_rect.top() {
                    if (to_x(self.trim_start_ms) - pointer.x).abs()
                        <= (to_x(self.trim_end_ms) - pointer.x).abs()
                    {
                        0
                    } else {
                        1
                    }
                } else {
                    2
                };

                match nearest {
                    0 => self.trim_start_ms = target_ms.min(self.trim_end_ms - 1000).max(0),
                    1 => {
                        self.trim_end_ms = target_ms.max(self.trim_start_ms + 1000).min(duration_ms)
                    }
                    3 => {
                        self.fade_in_ms = (target_ms - self.trim_start_ms)
                            .clamp(0, selected_ms / 2)
                            .min(30_000) as i32
                    }
                    4 => {
                        self.fade_out_ms = (self.trim_end_ms - target_ms)
                            .clamp(0, selected_ms / 2)
                            .min(30_000) as i32
                    }
                    _ => {
                        self.trim_playhead_ms =
                            target_ms.clamp(self.trim_start_ms, self.trim_end_ms)
                    }
                }
                self.normalize_trim_state();
            }
        }

        let painter = ui.painter();
        painter.rect_filled(track_rect, 4.0, egui::Color32::from_rgb(20, 28, 34));

        let selected_rect = egui::Rect::from_min_max(
            egui::pos2(to_x(self.trim_start_ms), track_rect.top()),
            egui::pos2(to_x(self.trim_end_ms), track_rect.bottom()),
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
            let wave = ((ratio * 18.0).sin().abs() * 0.55
                + (ratio * 47.0).sin().abs() * 0.30
                + (ratio * 91.0).sin().abs() * 0.15)
                .clamp(0.15, 1.0);
            let amp = max_amp * wave;
            let waveform_color = if selected_rect.contains(egui::pos2(x, center_y)) {
                egui::Color32::from_rgb(72, 222, 226)
            } else {
                egui::Color32::from_rgb(43, 94, 103)
            };
            painter.line_segment(
                [egui::pos2(x, center_y - amp), egui::pos2(x, center_y + amp)],
                egui::Stroke::new(2.0, waveform_color),
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
            egui::Stroke::new(1.0, egui::Color32::from_rgb(85, 225, 231)),
        );

        painter.line_segment(
            [
                egui::pos2(track_rect.left(), track_rect.top() - 10.0),
                egui::pos2(track_rect.right(), track_rect.top() - 10.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(56, 66, 82)),
        );
        painter.line_segment(
            [
                egui::pos2(track_rect.left(), track_rect.bottom() + 10.0),
                egui::pos2(track_rect.right(), track_rect.bottom() + 10.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(39, 47, 60)),
        );

        for tick in 0..=4 {
            let x = track_rect.left() + track_width * tick as f32 / 4.0;
            painter.line_segment(
                [
                    egui::pos2(x, track_rect.bottom() + 14.0),
                    egui::pos2(x, track_rect.bottom() + 18.0),
                ],
                egui::Stroke::new(1.0, muted),
            );
        }

        let start_x = to_x(self.trim_start_ms);
        let end_x = to_x(self.trim_end_ms);
        let play_x = to_x(self.trim_playhead_ms);
        let fade_in_x = to_x(self.trim_start_ms + self.fade_in_ms as i64);
        let fade_out_x = to_x(self.trim_end_ms - self.fade_out_ms as i64);

        let fade_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(238, 244, 245));
        if self.fade_in_ms > 0 {
            painter.add(egui::Shape::CubicBezier(
                egui::epaint::CubicBezierShape::from_points_stroke(
                    [
                        egui::pos2(start_x, selected_rect.bottom() - 2.0),
                        egui::pos2(
                            start_x + (fade_in_x - start_x) * 0.32,
                            selected_rect.bottom() - 1.0,
                        ),
                        egui::pos2(
                            start_x + (fade_in_x - start_x) * 0.68,
                            selected_rect.top() + 3.0,
                        ),
                        egui::pos2(fade_in_x, selected_rect.top() + 3.0),
                    ],
                    false,
                    egui::Color32::TRANSPARENT,
                    fade_stroke,
                ),
            ));
        }
        if self.fade_out_ms > 0 {
            painter.add(egui::Shape::CubicBezier(
                egui::epaint::CubicBezierShape::from_points_stroke(
                    [
                        egui::pos2(fade_out_x, selected_rect.top() + 3.0),
                        egui::pos2(
                            fade_out_x + (end_x - fade_out_x) * 0.32,
                            selected_rect.top() + 3.0,
                        ),
                        egui::pos2(
                            fade_out_x + (end_x - fade_out_x) * 0.68,
                            selected_rect.bottom() - 1.0,
                        ),
                        egui::pos2(end_x, selected_rect.bottom() - 2.0),
                    ],
                    false,
                    egui::Color32::TRANSPARENT,
                    fade_stroke,
                ),
            ));
        }

        let draw_label = |x: f32, y: f32, text_value: String, color: egui::Color32| {
            let char_w = 6.2;
            let width = (text_value.chars().count() as f32 * char_w + 12.0).max(42.0);
            let label_rect =
                egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(width, 18.0));
            painter.rect_filled(label_rect, 5.0, egui::Color32::from_rgb(28, 33, 43));
            painter.rect_stroke(label_rect, 5.0, egui::Stroke::new(1.0, color));
            painter.text(
                label_rect.center(),
                egui::Align2::CENTER_CENTER,
                text_value,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgb(238, 241, 247),
            );
        };

        let draw_fade_handle = |x: f32, label: &str, ms: i32| {
            let handle = egui::Rect::from_center_size(
                egui::pos2(x, selected_rect.top() + 2.0),
                egui::vec2(9.0, 9.0),
            );
            painter.rect_filled(handle, 1.5, egui::Color32::from_rgb(220, 230, 232));
            painter.rect_stroke(
                handle,
                1.5,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 66, 72)),
            );
            draw_label(
                x,
                selected_rect.top() - 12.0,
                format!("{} {}", label, format_time_ms(ms as i64)),
                egui::Color32::from_rgb(220, 230, 232),
            );
        };
        draw_fade_handle(fade_in_x, "↑", self.fade_in_ms);
        draw_fade_handle(fade_out_x, "↓", self.fade_out_ms);

        let draw_pin =
            |x: f32, color: egui::Color32, label: &str, time: Option<String>, above: bool| {
                let tip_y = if above {
                    track_rect.top() - 1.0
                } else {
                    track_rect.bottom() + 1.0
                };
                let head_y = if above { tip_y - 22.0 } else { tip_y + 22.0 };
                let head_rect = egui::Rect::from_center_size(
                    egui::pos2(x, head_y),
                    egui::vec2(if label == "▶" { 22.0 } else { 18.0 }, 18.0),
                );
                let stem = if above {
                    vec![
                        egui::pos2(x - 6.0, head_rect.bottom() - 2.0),
                        egui::pos2(x + 6.0, head_rect.bottom() - 2.0),
                        egui::pos2(x, tip_y),
                    ]
                } else {
                    vec![
                        egui::pos2(x - 6.0, head_rect.top() + 2.0),
                        egui::pos2(x, tip_y),
                        egui::pos2(x + 6.0, head_rect.top() + 2.0),
                    ]
                };
                painter.add(egui::Shape::convex_polygon(stem, color, egui::Stroke::NONE));
                painter.rect_filled(head_rect, 5.0, color);
                painter.text(
                    head_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    egui::Color32::WHITE,
                );
                if let Some(time) = time {
                    draw_label(x, head_rect.top() - 11.0, time, color);
                }
            };

        draw_pin(
            start_x,
            success,
            "S",
            Some(format_time_ms(self.trim_start_ms)),
            true,
        );
        draw_pin(
            end_x,
            success,
            "E",
            Some(format_time_ms(self.trim_end_ms)),
            true,
        );
        draw_pin(play_x, accent, "▶", None, false);

        painter.line_segment(
            [
                egui::pos2(play_x, track_rect.top() - 1.0),
                egui::pos2(play_x, track_rect.bottom() + 1.0),
            ],
            egui::Stroke::new(1.0, accent),
        );
    }

    fn start_generation(&mut self, ctx: egui::Context) {
        let audio_path = match &self.audio_path {
            Some(p) => p.clone(),
            None => return,
        };
        self.stop_video_preview();
        clear_bundled_runtime_quarantine();

        let worker_path = match find_worker() {
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
        let rust_renderer_path = find_rust_renderer();

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
        let font = self.font.clone();
        let active_color = self.color_active;
        let inactive_color = self.color_inactive;
        let bg_color = self.color_bg;
        let inactive_opacity = self.inactive_opacity.clamp(0.0, 1.0);
        let audio_delay_seconds = self.audio_delay_ms as f32 / 1000.0;
        let fade_in_ms = self.fade_in_ms;
        let fade_out_ms = self.fade_out_ms;
        let plain_lines = self.plain_lines;
        let trim_bounds = self.clamped_trim_bounds();
        let temp = temp_dir();
        let default_exports = exports_dir();
        let output_dir = self
            .batch_current_index
            .and_then(|index| self.batch_items.get(index))
            .map(|item| item.folder.clone())
            .unwrap_or(default_exports);
        let uploads = upload_dir();
        let worker_path_for_log = worker_path.to_string_lossy().to_string();
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
            let clean_filename = format!("{} - {} (karaoke).mp4", output_artist, output_title)
                .replace("/", "_")
                .replace("\\", "_");
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
            debug_log(format!(
                "[karaoke-ui] worker={} renderer={:?} model={} plain_lines={}",
                worker_path_for_log, renderer_path_for_log, model, plain_lines
            ));
            ctx.request_repaint();

            let mut trim_for_lyrics: Option<(i64, i64)> = None;
            let worker_audio_path = if let Some((start, end)) = trim_bounds {
                let should_trim = start > 0
                    || probe_audio_duration_ms(&audio_path)
                        .map(|duration| end < duration - 250)
                        .unwrap_or(false)
                    || fade_in_ms > 0
                    || fade_out_ms > 0;

                if should_trim {
                    let trimmed_path = temp.join("karaoke_trimmed_generation.wav");
                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                        "✂️ Подготовка аудио: {} - {}, восхождение {}, затухание {}",
                        format_time_ms(start),
                        format_time_ms(end),
                        format_time_ms(fade_in_ms as i64),
                        format_time_ms(fade_out_ms as i64)
                    )));
                    ctx.request_repaint();

                    match render_trimmed_audio(
                        &audio_path,
                        start,
                        end,
                        fade_in_ms as i64,
                        fade_out_ms as i64,
                        &trimmed_path,
                    ) {
                        Ok(()) => {
                            trim_for_lyrics = Some((start, end.saturating_sub(start)));
                            trimmed_path.to_string_lossy().to_string()
                        }
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

            let lyrics_for_worker = trim_for_lyrics
                .map(|(start, duration)| shift_lrc_for_trim(&lyrics, start, duration))
                .unwrap_or(lyrics);
            if let Err(e) = std::fs::write(&temp_lyrics_path, &lyrics_for_worker) {
                let _ = tx.send(ProgressUpdate::Error(format!(
                    "Не удалось записать временный файл текста: {}",
                    e
                )));
                return;
            }

            let mut cmd = if is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            hide_subprocess_window(&mut cmd);

            if let Some(bin_dir) = bundled_bin_dir() {
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

            if plain_lines {
                cmd.arg("--plain-lines");
            }

            if use_rust_renderer {
                cmd.arg("--timings-only")
                    .arg("--timings-output")
                    .arg(timings_output_path.to_string_lossy().as_ref());
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    debug_log(format!("[karaoke-ui] worker spawn failed: {e}"));
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
                    for line in reader.lines() {
                        if let Ok(line_str) = line {
                            let trimmed = line_str.trim();
                            if !trimmed.is_empty() {
                                let _ = tx_err
                                    .send(ProgressUpdate::RawLog(format!("[LOG] {}", trimmed)));
                                ctx_err.request_repaint();
                            }
                        }
                    }
                })
            });

            // Потоковое чтение stdout (вывод прогресса в JSON)
            if let Some(stdout) = child.stdout.take() {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(stdout);
                let exports_str = output_dir.to_string_lossy().to_string();
                for line in reader.lines() {
                    if let Ok(line_str) = line {
                        let trimmed = line_str.trim();
                        if trimmed.starts_with('{') && trimmed.ends_with('}') {
                            if let Ok(mut update) = serde_json::from_str::<CLIProgress>(trimmed) {
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
                        }
                        if !trimmed.is_empty() {
                            let _ = tx.send(ProgressUpdate::RawLog(trimmed.to_string()));
                            ctx.request_repaint();
                        }
                    }
                }
            }

            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }

            let status = child.wait();
            let mut success = status.map(|s| s.success()).unwrap_or(false);
            debug_log(format!("[karaoke-ui] worker finished success={success}"));

            if success {
                if let Some(renderer_path) = rust_renderer_path {
                    let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                        progress: 0.55,
                        status: "Rust-рендер: сборка видео...".to_string(),
                        done: false,
                        error: None,
                        file: None,
                        batch_align_index: None,
                    }));
                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                        "🎬 Rust-рендер: {}",
                        output_mp4_path.to_string_lossy()
                    )));
                    let render_duration_ms = probe_audio_duration_ms(&worker_audio_path)
                        .unwrap_or(0)
                        .max(1);
                    debug_log(format!(
                        "[karaoke-ui] render start renderer={} output={} plain_lines={}",
                        renderer_path.to_string_lossy(),
                        output_mp4_path.to_string_lossy(),
                        plain_lines
                    ));
                    ctx.request_repaint();

                    let mut render_cmd = std::process::Command::new(&renderer_path);
                    hide_subprocess_window(&mut render_cmd);
                    if let Some(bin_dir) = bundled_bin_dir() {
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
                        .arg("--color-active")
                        .arg(&active_hex)
                        .arg("--color-inactive")
                        .arg(&inactive_hex)
                        .arg("--color-bg")
                        .arg(&bg_hex)
                        .arg("--inactive-opacity")
                        .arg(inactive_opacity.to_string())
                        .arg("--engine")
                        .arg(
                            std::env::var("KARAOKE_RENDER_ENGINE")
                                .unwrap_or_else(|_| "ass".to_string()),
                        )
                        .arg("--audio-delay")
                        .arg(audio_delay_seconds.to_string())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped());

                    if plain_lines {
                        render_cmd.arg("--plain-lines");
                    }

                    match render_cmd.spawn() {
                        Ok(mut render_child) => {
                            if let Some(stderr) = render_child.stderr.take() {
                                use std::io::{BufRead, BufReader};
                                let reader = BufReader::new(stderr);
                                for line in reader.lines().map_while(Result::ok) {
                                    let trimmed = line.trim();
                                    if let Some(time_ms) = parse_ffmpeg_time_ms(trimmed) {
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
                                        }));
                                        ctx.request_repaint();
                                        continue;
                                    }
                                    if let Some(percent) = trimmed.strip_prefix("render ") {
                                        let number = percent.trim_end_matches('%');
                                        if let Ok(value) = number.parse::<f32>() {
                                            let mapped = 0.55 + (value / 100.0) * 0.43;
                                            let _ =
                                                tx.send(ProgressUpdate::Progress(CLIProgress {
                                                    progress: mapped.clamp(0.55, 0.98),
                                                    status: format!(
                                                        "Rust-рендер: {:.0}%",
                                                        value.clamp(0.0, 100.0)
                                                    ),
                                                    done: false,
                                                    error: None,
                                                    file: None,
                                                    batch_align_index: None,
                                                }));
                                            ctx.request_repaint();
                                            continue;
                                        }
                                    }
                                    if is_ffmpeg_progress_key(trimmed) {
                                        continue;
                                    }
                                    if !trimmed.is_empty() {
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
                            debug_log(format!("[karaoke-ui] render finished success={success}"));
                            if success {
                                let _ = tx.send(ProgressUpdate::Progress(CLIProgress {
                                    progress: 1.0,
                                    status: "Видео собрано Rust-рендером.".to_string(),
                                    done: true,
                                    error: None,
                                    file: Some(output_mp4_path.to_string_lossy().to_string()),
                                    batch_align_index: None,
                                }));
                            } else {
                                let _ = tx.send(ProgressUpdate::Error(
                                    "Rust-рендер завершился с ошибкой.".to_string(),
                                ));
                            }
                            ctx.request_repaint();
                        }
                        Err(e) => {
                            success = false;
                            debug_log(format!("[karaoke-ui] render spawn failed: {e}"));
                            let _ = tx.send(ProgressUpdate::Error(format!(
                                "Ошибка запуска Rust-рендера: {}",
                                e
                            )));
                            ctx.request_repaint();
                        }
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
        self.sync_preview_playhead(ctx);
        self.sync_video_preview(ctx);

        let old_settings = AppSettings {
            model: self.model.clone(),
            quality: self.quality.clone(),
            font: self.font.clone(),
            color_active: self.color_active,
            color_inactive: self.color_inactive,
            color_bg: self.color_bg,
            inactive_opacity: self.inactive_opacity,
            audio_delay_ms: self.audio_delay_ms,
            fade_in_ms: self.fade_in_ms,
            fade_out_ms: self.fade_out_ms,
            artist: self.artist.clone(),
            title: self.title.clone(),
            lyrics: self.lyrics.clone(),
            plain_lines: self.plain_lines,
        };

        // Единая современная темная тема для всех стандартных виджетов egui.
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = 10.0.into();
        visuals.widgets.active.rounding = 8.0.into();
        visuals.widgets.hovered.rounding = 8.0.into();
        visuals.widgets.inactive.rounding = 8.0.into();
        visuals.widgets.open.rounding = 8.0.into();

        visuals.selection.bg_fill = egui::Color32::from_rgb(45, 118, 255);
        visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);

        visuals.extreme_bg_color = egui::Color32::from_rgb(12, 14, 18);
        visuals.window_fill = egui::Color32::from_rgb(12, 14, 18);
        visuals.panel_fill = egui::Color32::from_rgb(12, 14, 18);
        visuals.faint_bg_color = egui::Color32::from_rgb(24, 28, 36);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(28, 33, 43);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(39, 46, 60);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(45, 118, 255);

        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(226, 229, 236));
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(196, 202, 214));

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
                        self.trim_status = format!("Длительность: {}", format_time_ms(duration_ms));
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

        // Опрос прогресса из фонового канала
        loop {
            let update = self.rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(update) = update else {
                break;
            };

            match update {
                ProgressUpdate::Progress(prog) => {
                    self.progress = prog.progress;
                    self.status_text = prog.status;
                    if let Some(index) = self.batch_current_index {
                        if let Some(item) = self.batch_items.get_mut(index) {
                            item.progress = prog.progress;
                        }
                    }
                    if let Some(err) = &prog.error {
                        debug_log(format!("[karaoke-ui] Progress error: {}", err));
                        self.log_output
                            .push_str(&format!("❌ Ошибка ИИ: {}\n", err));
                    }
                    if let Some(full_path) = prog.file {
                        self.log_output
                            .push_str(&format!("🎉 Успешно сохранено: {}\n", full_path));
                        self.stop_video_preview();
                        self.video_texture = None;
                        self.video_status = String::new();
                        self.video_duration_ms = probe_audio_duration_ms(&full_path).unwrap_or(0);
                        self.video_position_ms = 0;
                        self.generated_file = Some(full_path);
                        if let Some(index) = self.batch_current_index {
                            if let Some(item) = self.batch_items.get_mut(index) {
                                item.output_path = self.generated_file.clone();
                            }
                        }
                    }
                }
                ProgressUpdate::BatchProgress {
                    index,
                    status,
                    progress,
                    message,
                    output_path,
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
                        if output_path.is_some() {
                            item.output_path = output_path.clone();
                        }
                    }
                    if let Some(path) = output_path {
                        self.generated_file = Some(path.clone());
                        self.log_output
                            .push_str(&format!("🎉 Batch сохранено: {}\n", path));
                    }
                }
                ProgressUpdate::RawLog(log) => {
                    debug_log(format!("[worker-raw] {}", log));
                    self.log_output.push_str(&format!("{}\n", log));
                }
                ProgressUpdate::Error(err) => {
                    debug_log(format!("[worker-error] {}", err));
                    if self.batch_current_index.is_some() {
                        self.log_output.push_str(&format!("❌ Ошибка: {}\n", err));
                        self.finish_batch_current(false, Some(err), ctx);
                        continue;
                    }
                    self.is_generating = false;
                    self.status_text = "Ошибка".to_string();
                    self.log_output.push_str(&format!("❌ Ошибка: {}\n", err));
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
                        "Batch завершен: готово {}, ошибок {}, без текста {}",
                        done, failed, missing
                    );
                    self.log_output.push_str(&format!(
                        "✅ Batch завершен: готово {}, ошибок {}, без текста {}\n",
                        done, failed, missing
                    ));
                }
            }
        }

        // Опрос фонового канала сканирования папок (Batch)
        if let Some(rx) = &self.batch_scan_rx {
            if let Ok(res) = rx.try_recv() {
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

                if res.folder_count == 1 {
                    self.batch_status_text = format!(
                        "Найдено {} треков в {} · к обработке {} · без текста {}",
                        total_items, res.first_folder_name, processable_items, missing_lyrics
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
                        "Найдено {} треков в {} папках · к обработке {} · без текста {}",
                        total_items, res.folder_count, processable_items, missing_lyrics
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
                    "🎵 Найдено треков: {}, к обработке: {}, без текста: {}\n",
                    total_items, processable_items, missing_lyrics
                ));
                for warning in res.warnings {
                    self.log_output.push_str(&format!("⚠️ {}\n", warning));
                }
            }
        }

        // Опрос фонового канала парсинга Excel
        if let Some(rx) = &self.dl_parse_rx {
            if let Ok(res) = rx.try_recv() {
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
        }

        // Опрос фонового канала загрузчика
        loop {
            let update = self.dl_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(log_line) = update else {
                break;
            };
            if log_line == "___FINISHED_SUCCESS___" {
                self.dl_is_running = false;
                self.dl_status_text = "Загрузка успешно завершена!".to_string();
                self.dl_log_output
                    .push_str("\n🎉 Загрузка успешно завершена!\n");
            } else if log_line == "___FINISHED_FAILURE___" {
                self.dl_is_running = false;
                if self.dl_stop_requested {
                    self.dl_status_text = "Загрузка остановлена пользователем".to_string();
                    self.dl_log_output
                        .push_str("\n🛑 Загрузка остановлена пользователем.\n");
                    self.dl_stop_requested = false;
                } else {
                    self.dl_status_text = "Загрузка завершилась с ошибкой".to_string();
                    self.dl_log_output
                        .push_str("\n❌ Загрузка прервана из-за ошибки.\n");
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
        if dropped_dirs.len() > 1 && !self.is_generating && !self.batch_running {
            self.load_batch_folders(dropped_dirs, ctx);
            self.active_tab = ActiveTab::Batch;
        } else {
            for path in dropped_paths {
                self.handle_dropped_file(path, ctx);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(12, 14, 18));

            let card_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(19, 23, 31))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 56)))
                .rounding(10.0)
                .inner_margin(18.0);

            let drop_zone_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(15, 18, 24))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(54, 65, 82)))
                .rounding(8.0)
                .inner_margin(18.0);

            let log_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(10, 12, 17))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(34, 40, 52)))
                .rounding(8.0)
                .inner_margin(12.0);

            let page_margin = 24.0;
            let muted = egui::Color32::from_rgb(145, 154, 171);
            let text = egui::Color32::from_rgb(238, 241, 247);
            let accent = egui::Color32::from_rgb(88, 166, 255);
            let success = egui::Color32::from_rgb(54, 211, 153);

            ui.add_space(page_margin - 6.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Караоке-Видео Генератор v{}", APP_VERSION))
                            .strong()
                            .size(27.0)
                            .color(text)
                    );
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new("Соберите MP4 с пословной подсветкой, Whisper-синхронизацией и аккуратной типографикой.")
                            .size(13.0)
                            .color(muted)
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status_color = if self.is_generating {
                        accent
                    } else if self.generated_file.is_some() {
                        success
                    } else {
                        muted
                    };
                    let status = if self.is_generating {
                        "Генерация"
                    } else if self.generated_file.is_some() {
                        "Готово"
                    } else {
                        "Ожидание"
                    };
                    ui.label(
                        egui::RichText::new(format!("v{} · {}", APP_VERSION, status))
                            .strong()
                            .size(13.0)
                            .color(status_color)
                    );
                });
            });
            ui.add_space(18.0);

             ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.active_tab == ActiveTab::SingleTrack, egui::RichText::new("Один трек").strong())
                    .clicked()
                    && !self.batch_running
                {
                    self.active_tab = ActiveTab::SingleTrack;
                }
                if ui
                    .selectable_label(self.active_tab == ActiveTab::Batch, egui::RichText::new("Пакетный рендеринг").strong())
                    .clicked()
                {
                    self.active_tab = ActiveTab::Batch;
                }
                if ui
                    .selectable_label(self.active_tab == ActiveTab::Downloader, egui::RichText::new("Загрузчик аудио").strong())
                    .clicked()
                {
                    self.active_tab = ActiveTab::Downloader;
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(match self.active_tab {
                        ActiveTab::SingleTrack => "Обычный режим создания караоке-видео для одного трека.",
                        ActiveTab::Batch => "Массовая генерация: каждая подпапка получает свой MP4 рядом с MP3/LRC.",
                        ActiveTab::Downloader => "Загрузка аудио: скачивание музыки по названию или списку из Excel.",
                    })
                    .size(12.0)
                    .color(muted),
                );
            });
            ui.add_space(14.0);

            if self.active_tab == ActiveTab::Batch {
                let batch_margin = 14.0;
                let content_width = ui.available_width() - batch_margin * 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(batch_margin);
                    ui.vertical(|ui| {
                        ui.set_width(content_width.max(520.0));

                        card_frame.show(ui, |ui| {
                            let top_inner_width = (content_width - 36.0).max(760.0);
                            ui.set_min_width(top_inner_width);

                            let top_gap = 24.0;
                            let available_top_width = top_inner_width - top_gap;
                            let left_panel_width = (available_top_width * 0.36).floor().max(430.0);
                            let right_panel_width =
                                (available_top_width - left_panel_width).floor().max(360.0);
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
                            let missing_lyrics = self
                                .batch_items
                                .iter()
                                .filter(|item| item.status == BatchStatus::MissingLyrics)
                                .count();

                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(left_panel_width, 132.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("Массовая генерация")
                                                .strong()
                                                .size(18.0)
                                                .color(text),
                                        );
                                        ui.add_space(10.0);
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add_enabled(
                                                    !self.is_generating && !self.batch_running && !self.batch_is_scanning,
                                                    egui::Button::new(
                                                        egui::RichText::new("Выбрать папку")
                                                            .size(13.0)
                                                            .strong(),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                if let Some(path) =
                                                    rfd::FileDialog::new().pick_folder()
                                                {
                                                    self.load_batch_folder(path, ctx);
                                                }
                                            }

                                            let can_start_batch = !self.batch_items.is_empty()
                                                && !self.is_generating
                                                && !self.batch_running;
                                            if ui
                                                .add_enabled(
                                                    can_start_batch,
                                                    egui::Button::new(
                                                        egui::RichText::new("Запустить сначала")
                                                            .size(13.0)
                                                            .strong(),
                                                    )
                                                    .fill(egui::Color32::from_rgb(45, 118, 255)),
                                                )
                                                .clicked()
                                            {
                                                self.start_batch_queue(ctx);
                                            }

                                            let can_continue_batch = !self.batch_items.is_empty()
                                                && !self.is_generating
                                                && !self.batch_running
                                                && self
                                                    .batch_items
                                                    .iter()
                                                    .any(|item| {
                                                        item.status == BatchStatus::Ready
                                                            || item.status == BatchStatus::ReadyToRender
                                                    });
                                            if ui
                                                .add_enabled(
                                                    can_continue_batch,
                                                    egui::Button::new(
                                                        egui::RichText::new("Продолжить")
                                                            .size(13.0)
                                                            .strong(),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                self.continue_batch_queue(ctx);
                                            }

                                            let can_gen_selected = !self.batch_items.is_empty()
                                                && !self.is_generating
                                                && !self.batch_running
                                                && self.batch_selected_index.is_some();
                                            let selected_can_generate = self
                                                .batch_selected_index
                                                .and_then(|idx| self.batch_items.get(idx))
                                                .map(|item| item.status != BatchStatus::MissingLyrics)
                                                .unwrap_or(false);
                                            if ui
                                                .add_enabled(
                                                    can_gen_selected && selected_can_generate,
                                                    egui::Button::new(
                                                        egui::RichText::new("Сгенерировать выбранный")
                                                            .size(13.0)
                                                            .strong(),
                                                    ).fill(egui::Color32::from_rgb(168, 85, 247)),
                                                )
                                                .clicked()
                                            {
                                                if let Some(idx) = self.batch_selected_index {
                                                    self.start_single_batch_item(idx, ctx);
                                                }
                                            }

                                            if ui
                                                .add_enabled(
                                                    self.batch_running
                                                        && !self.batch_stop_requested,
                                                    egui::Button::new(
                                                        egui::RichText::new(
                                                            "Остановить после текущего",
                                                        )
                                                        .size(13.0),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                self.request_stop_batch();
                                            }
                                        });
                                        ui.add_space(12.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} / {} готово",
                                                    done,
                                                    self.batch_items.len()
                                                ))
                                                .strong()
                                                .size(13.0)
                                                .color(muted),
                                            );
                                            ui.separator();
                                            ui.label(
                                                egui::RichText::new(format!("{} ошибок", failed))
                                                    .strong()
                                                    .size(13.0)
                                                    .color(if failed > 0 {
                                                        egui::Color32::from_rgb(255, 176, 96)
                                                    } else {
                                                        muted
                                                    }),
                                            );
                                            ui.separator();
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} без текста",
                                                    missing_lyrics
                                                ))
                                                .strong()
                                                .size(13.0)
                                                .color(if missing_lyrics > 0 {
                                                    egui::Color32::from_rgb(255, 176, 96)
                                                } else {
                                                    muted
                                                }),
                                            );
                                        });
                                        ui.add_space(4.0);

                                        let batch_video_dir = self
                                            .batch_root
                                            .clone()
                                            .unwrap_or_else(exports_dir);
                                        if batch_video_dir.exists() {
                                            if ui.button("📁 Открыть папку с видео").clicked() {
                                                open_in_explorer(&batch_video_dir);
                                            }
                                            ui.add_space(4.0);
                                        }

                                        if self.batch_running {
                                            if let Some(start_time) = self.batch_start_time {
                                                let elapsed = start_time.elapsed().as_secs();
                                                let elapsed_str = format!("{:02}:{:02}", elapsed / 60, elapsed % 60);

                                                let mut time_str = format!("⏱️ Прошло: {}", elapsed_str);

                                                let n = self.batch_items.len();
                                                if n > 0 {
                                                    let current_idx = self.batch_current_index.unwrap_or(0);
                                                    let current_progress = self.progress;
                                                    let total_progress = (current_idx as f32 + current_progress) / n as f32;

                                                    if total_progress > 0.001 && total_progress < 1.0 {
                                                        let total_est = elapsed as f32 / total_progress;
                                                        let rem = (total_est - elapsed as f32).round() as u64;
                                                        let rem_str = format!("{:02}:{:02}", rem / 60, rem % 60);
                                                        time_str.push_str(&format!("  |  ⏳ Осталось ~{}", rem_str));
                                                    }
                                                }
                                                ui.label(egui::RichText::new(time_str).size(12.0).color(muted));
                                                ui.add_space(4.0);
                                            }
                                        }

                                        let folder_label = self
                                            .batch_root
                                            .as_ref()
                                            .map(|path| display_file_name(path))
                                            .unwrap_or_else(|| "Папка не выбрана".to_string());
                                        ui.label(
                                            egui::RichText::new(folder_label)
                                                .size(12.0)
                                                .color(muted),
                                        );
                                    },
                                );

                                ui.add_space(top_gap);

                                ui.allocate_ui_with_layout(
                                    egui::vec2(right_panel_width, 132.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("Настройки генерации")
                                                .strong()
                                                .size(18.0)
                                                .color(text),
                                        );
                                        ui.add_space(10.0);
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new("Модель")
                                                        .size(11.0)
                                                        .color(muted),
                                                );
                                                egui::ComboBox::from_id_salt("batch_model_combo")
                                                    .selected_text(match self.model.as_str() {
                                                        "medium" => "medium",
                                                        "small" => "small",
                                                        _ => "base",
                                                    })
                                                    .width(170.0)
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut self.model,
                                                            "base".to_string(),
                                                            "base",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.model,
                                                            "small".to_string(),
                                                            "small",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.model,
                                                            "medium".to_string(),
                                                            "medium",
                                                        );
                                                    });
                                            });
                                            ui.add_space(12.0);
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new("Качество")
                                                        .size(11.0)
                                                        .color(muted),
                                                );
                                                egui::ComboBox::from_id_salt("batch_quality_combo")
                                                    .selected_text(match self.quality.as_str() {
                                                        "ultra" => "Ультра",
                                                        "high" => "Высокое",
                                                        _ => "Стандарт",
                                                    })
                                                    .width(170.0)
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut self.quality,
                                                            "medium".to_string(),
                                                            "Стандарт",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.quality,
                                                            "high".to_string(),
                                                            "Высокое",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.quality,
                                                            "ultra".to_string(),
                                                            "Ультра",
                                                        );
                                                    });
                                            });
                                            ui.add_space(12.0);
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new("Шрифт")
                                                        .size(11.0)
                                                        .color(muted),
                                                );
                                                egui::ComboBox::from_id_salt("batch_font_combo")
                                                    .selected_text(match self.font.as_str() {
                                                        "arial" => "Arial",
                                                        "helvetica" => "Helvetica",
                                                        "georgia" => "Georgia",
                                                        _ => "Montserrat",
                                                    })
                                                    .width(170.0)
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut self.font,
                                                            "montserrat".to_string(),
                                                            "Montserrat",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.font,
                                                            "arial".to_string(),
                                                            "Arial",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.font,
                                                            "helvetica".to_string(),
                                                            "Helvetica",
                                                        );
                                                        ui.selectable_value(
                                                            &mut self.font,
                                                            "georgia".to_string(),
                                                            "Georgia",
                                                        );
                                                    });
                                            });
                                        });
                                        ui.add_space(12.0);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new("Цвета")
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                            ui.add_space(4.0);
                                            ui.color_edit_button_srgb(&mut self.color_active);
                                            ui.label(
                                                egui::RichText::new("Активный")
                                                    .size(11.0)
                                                    .color(text),
                                            );
                                            ui.add_space(8.0);
                                            ui.color_edit_button_srgb(&mut self.color_inactive);
                                            ui.label(
                                                egui::RichText::new("Будущий")
                                                    .size(11.0)
                                                    .color(text),
                                            );
                                            ui.add_space(8.0);
                                            ui.color_edit_button_srgb(&mut self.color_bg);
                                            ui.label(
                                                egui::RichText::new("Фон")
                                                    .size(11.0)
                                                    .color(text),
                                            );
                                            ui.separator();
                                            ui.label(
                                                egui::RichText::new("Прозрачность")
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                            ui.add_sized(
                                                egui::vec2(110.0, 18.0),
                                                egui::Slider::new(
                                                    &mut self.inactive_opacity,
                                                    0.2..=1.0,
                                                )
                                                .show_value(false),
                                            );
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{}%",
                                                    (self.inactive_opacity * 100.0).round()
                                                        as i32
                                                ))
                                                .size(11.0)
                                                .color(accent),
                                            );
                                            ui.checkbox(&mut self.plain_lines, "Только строки");
                                        });
                                    },
                                );
                            });
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Очередь").strong().size(16.0).color(text));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new("Слева список, справа текст выбранного трека.").size(11.0).color(muted));
                            });
                        });
                        ui.add_space(6.0);

                        if self.batch_is_scanning {
                            drop_zone_frame.show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(30.0);
                                    ui.add(egui::widgets::Spinner::new().size(30.0));
                                    ui.add_space(12.0);
                                    ui.label(
                                        egui::RichText::new("Сканирование папки, считывание длительности аудио...")
                                            .size(14.0)
                                            .strong()
                                            .color(accent),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new("Пожалуйста, подождите. Для больших папок это может занять несколько секунд.")
                                            .size(11.0)
                                            .color(muted),
                                    );
                                    ui.add_space(30.0);
                                });
                            });
                        } else if self.batch_items.is_empty() {
                            drop_zone_frame.show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(20.0);
                                    ui.label(
                                        egui::RichText::new("Перетащите сюда папку batch или выберите ее кнопкой выше")
                                            .size(14.0)
                                            .color(muted),
                                    );
                                    ui.add_space(20.0);
                                });
                            });
                        } else {
                            let selected_index = self
                                .batch_selected_index
                                .filter(|idx| *idx < self.batch_items.len())
                                .unwrap_or(0);
                            self.batch_selected_index = Some(selected_index);

                            let selected_snapshot = self.batch_items[selected_index].clone();
                            let should_load = self.audio_path.as_deref()
                                != Some(&selected_snapshot.audio_path.to_string_lossy());
                            if should_load && !self.is_generating && !self.batch_running {
                                let _ = self.apply_batch_item_to_single_state(selected_index);
                            }

                            let column_gap = 18.0;
                            let available_width = ui.available_width();
                            let column_width = ((available_width - column_gap) / 2.0)
                                .floor()
                                .max(360.0);
                            let bottom_margin = 30.0;
                            let column_height =
                                (ctx.screen_rect().bottom() - ui.cursor().top() - bottom_margin)
                                    .max(320.0);
                            let header_height = 28.0;
                            let body_height = (column_height - header_height).max(260.0);
                            let list_inner_width = column_width - 20.0;
                            let mut select_request = None;
                            let mut remove_request = None;

                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(column_width, column_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Список")
                                                    .strong()
                                                    .size(14.0)
                                                    .color(text),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{} треков",
                                                            self.batch_items.len()
                                                        ))
                                                        .size(12.0)
                                                        .color(muted),
                                                    );
                                                },
                                            );
                                        });
                                        ui.add_space(6.0);

                                        let (body_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(column_width, body_height),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            body_rect,
                                            6.0,
                                            egui::Color32::from_rgb(12, 15, 21),
                                        );
                                        ui.painter().rect_stroke(
                                            body_rect,
                                            6.0,
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgb(34, 40, 52),
                                            ),
                                        );
                                        let inner_rect = body_rect.shrink(8.0);
                                        ui.allocate_new_ui(
                                            egui::UiBuilder::new().max_rect(inner_rect),
                                            |ui| {
                                                egui::ScrollArea::vertical()
                                                    .id_salt("batch_track_list_simple")
                                                    .max_height(inner_rect.height())
                                                    .auto_shrink([false, false])
                                                    .show(ui, |ui| {
                                                        for idx in 0..self.batch_items.len() {
                                                            let item =
                                                                self.batch_items[idx].clone();
                                                            let selected =
                                                                self.batch_selected_index
                                                                    == Some(idx);
                                                            let is_running = matches!(
                                                                item.status,
                                                                BatchStatus::Aligning
                                                                    | BatchStatus::Rendering
                                                            );
                                                            let row_fill = if is_running {
                                                                egui::Color32::from_rgb(24, 39, 66)
                                                            } else if selected {
                                                                egui::Color32::from_rgb(32, 39, 52)
                                                            } else {
                                                                egui::Color32::from_rgb(15, 18, 24)
                                                            };
                                                            let row_stroke = if selected {
                                                                accent
                                                            } else {
                                                                egui::Color32::from_rgb(34, 40, 52)
                                                            };
                                                            let name = if item.artist.is_empty() {
                                                                item.title.clone()
                                                            } else {
                                                                format!(
                                                                    "{} - {}",
                                                                    item.artist, item.title
                                                                )
                                                            };

                                                            let row_response = egui::Frame::none()
                                                                .fill(row_fill)
                                                                .stroke(egui::Stroke::new(
                                                                    1.0, row_stroke,
                                                                ))
                                                                .rounding(5.0)
                                                                .inner_margin(egui::vec2(8.0, 6.0))
                                                                .show(ui, |ui| {
                                                                    ui.set_min_width(
                                                                        list_inner_width - 16.0,
                                                                    );
                                                                    ui.horizontal(|ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                format!(
                                                                                    "{:03}",
                                                                                    idx + 1
                                                                                ),
                                                                            )
                                                                            .monospace()
                                                                            .size(10.5)
                                                                            .color(muted),
                                                                        );
                                                                        ui.vertical(|ui| {
                                                                            ui.set_width(
                                                                                list_inner_width
                                                                                    - 190.0,
                                                                            );
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    name,
                                                                                )
                                                                                .size(12.0)
                                                                                .strong()
                                                                                .color(text),
                                                                            );
                                                                        });
                                                                        ui.vertical(|ui| {
                                                                            ui.set_width(108.0);
                                                                            ui.with_layout(
                                                                                egui::Layout::right_to_left(
                                                                                    egui::Align::Center,
                                                                                ),
                                                                                |ui| {
                                                                                    if matches!(
                                                                                        item.status,
                                                                                        BatchStatus::Aligning
                                                                                            | BatchStatus::Rendering
                                                                                    ) {
                                                                                        let percent =
                                                                                            ((item.progress * 100.0).round() as i32)
                                                                                                .clamp(0, 100);
                                                                                        ui.label(
                                                                                            egui::RichText::new(format!(
                                                                                                "{}%",
                                                                                                percent
                                                                                            ))
                                                                                            .monospace()
                                                                                            .size(11.0)
                                                                                            .color(accent),
                                                                                        );
                                                                                    } else if item.status
                                                                                        == BatchStatus::MissingLyrics
                                                                                    {
                                                                                        ui.label(
                                                                                            egui::RichText::new("нет текста")
                                                                                                .size(10.5)
                                                                                                .color(egui::Color32::from_rgb(
                                                                                                    255, 176, 96,
                                                                                                )),
                                                                                        );
                                                                                    } else {
                                                                                        ui.add_space(30.0);
                                                                                    }
                                                                                },
                                                                            );
                                                                        });
                                                                    });
                                                                })
                                                                .response;
                                                            let control_y =
                                                                row_response.rect.center().y;
                                                            let remove_rect =
                                                                egui::Rect::from_center_size(
                                                                    egui::pos2(
                                                                        row_response.rect.right()
                                                                            - 24.0,
                                                                        control_y,
                                                                    ),
                                                                    egui::vec2(24.0, 24.0),
                                                                );
                                                            let status_rect =
                                                                egui::Rect::from_center_size(
                                                                    egui::pos2(
                                                                        row_response.rect.right()
                                                                            - 58.0,
                                                                        control_y,
                                                                    ),
                                                                    egui::vec2(22.0, 22.0),
                                                                );
                                                            let painter = ui.painter();
                                                            let status_center =
                                                                status_rect.center();
                                                            match &item.status {
                                                                BatchStatus::Done => {
                                                                    painter.circle_filled(
                                                                        status_center,
                                                                        8.0,
                                                                        success,
                                                                    );
                                                                    painter.line_segment(
                                                                        [
                                                                            egui::pos2(
                                                                                status_center.x
                                                                                    - 4.0,
                                                                                status_center.y,
                                                                            ),
                                                                            egui::pos2(
                                                                                status_center.x
                                                                                    - 1.0,
                                                                                status_center.y
                                                                                    + 3.0,
                                                                            ),
                                                                        ],
                                                                        egui::Stroke::new(
                                                                            1.8,
                                                                            egui::Color32::from_rgb(
                                                                                8, 18, 14,
                                                                            ),
                                                                        ),
                                                                    );
                                                                    painter.line_segment(
                                                                        [
                                                                            egui::pos2(
                                                                                status_center.x
                                                                                    - 1.0,
                                                                                status_center.y
                                                                                    + 3.0,
                                                                            ),
                                                                            egui::pos2(
                                                                                status_center.x
                                                                                    + 5.0,
                                                                                status_center.y
                                                                                    - 4.0,
                                                                            ),
                                                                        ],
                                                                        egui::Stroke::new(
                                                                            1.8,
                                                                            egui::Color32::from_rgb(
                                                                                8, 18, 14,
                                                                            ),
                                                                        ),
                                                                    );
                                                                }
                                                                BatchStatus::Aligning
                                                                | BatchStatus::Rendering => {
                                                                    painter.circle_stroke(
                                                                        status_center,
                                                                        11.5,
                                                                        egui::Stroke::new(
                                                                            1.5, accent,
                                                                        ),
                                                                    );
                                                                    painter.circle_filled(
                                                                        status_center,
                                                                        4.0,
                                                                        accent,
                                                                    );
                                                                }
                                                                BatchStatus::ReadyToRender => {
                                                                    painter.circle_filled(
                                                                        status_center,
                                                                        8.0,
                                                                        egui::Color32::from_rgb(
                                                                            246, 181, 70,
                                                                        ),
                                                                    );
                                                                    painter.text(
                                                                        status_center,
                                                                        egui::Align2::CENTER_CENTER,
                                                                        "R",
                                                                        egui::FontId::monospace(8.0),
                                                                        egui::Color32::from_rgb(
                                                                            18, 16, 10,
                                                                        ),
                                                                    );
                                                                }
                                                                BatchStatus::Error(_) => {
                                                                    painter.circle_filled(
                                                                        status_center,
                                                                        8.0,
                                                                        egui::Color32::from_rgb(
                                                                            255, 100, 100,
                                                                        ),
                                                                    );
                                                                    painter.text(
                                                                        status_center,
                                                                        egui::Align2::CENTER_CENTER,
                                                                        "!",
                                                                        egui::FontId::proportional(
                                                                            10.0,
                                                                        ),
                                                                        egui::Color32::WHITE,
                                                                    );
                                                                }
                                                                BatchStatus::Ready => {
                                                                    painter.circle_stroke(
                                                                        status_center,
                                                                        8.0,
                                                                        egui::Stroke::new(
                                                                            1.0,
                                                                            egui::Color32::from_rgb(
                                                                                42, 52, 68,
                                                                            ),
                                                                        ),
                                                                    );
                                                                }
                                                                BatchStatus::MissingLyrics => {
                                                                    painter.circle_filled(
                                                                        status_center,
                                                                        8.0,
                                                                        egui::Color32::from_rgb(
                                                                            255, 176, 96,
                                                                        ),
                                                                    );
                                                                    painter.text(
                                                                        status_center,
                                                                        egui::Align2::CENTER_CENTER,
                                                                        "T",
                                                                        egui::FontId::monospace(8.0),
                                                                        egui::Color32::from_rgb(
                                                                            24, 18, 8,
                                                                        ),
                                                                    );
                                                                }
                                                            }
                                                            painter.rect_filled(
                                                                remove_rect,
                                                                9.0,
                                                                egui::Color32::from_rgb(
                                                                    34, 24, 28,
                                                                ),
                                                            );
                                                            painter.text(
                                                                remove_rect.center(),
                                                                egui::Align2::CENTER_CENTER,
                                                                "×",
                                                                egui::FontId::proportional(13.0),
                                                                egui::Color32::from_rgb(
                                                                    205, 178, 184,
                                                                ),
                                                            );
                                                            let remove_response = ui
                                                                .interact(
                                                                    remove_rect,
                                                                    ui.id().with((
                                                                        "batch-remove",
                                                                        idx,
                                                                    )),
                                                                    egui::Sense::click(),
                                                                )
                                                                .on_hover_text(
                                                                    "Убрать из текущего списка",
                                                                );
                                                            if remove_response.clicked()
                                                                && !self.batch_running
                                                                && !self.is_generating
                                                            {
                                                                remove_request = Some(idx);
                                                            }
                                                            let click_rect =
                                                                egui::Rect::from_min_max(
                                                                    row_response.rect.min,
                                                                    egui::pos2(
                                                                        row_response.rect.right()
                                                                            - 94.0,
                                                                        row_response.rect.bottom(),
                                                                    ),
                                                                );
                                                            let response = ui.interact(
                                                                click_rect,
                                                                ui.id().with(("batch-row", idx)),
                                                                egui::Sense::click(),
                                                            );

                                                            if response.clicked() {
                                                                select_request = Some(idx);
                                                            }

                                                            if selected {
                                                                egui::Frame::none()
                                                                    .fill(egui::Color32::from_rgb(
                                                                        10, 13, 19,
                                                                    ))
                                                                    .stroke(egui::Stroke::new(
                                                                        1.0,
                                                                        egui::Color32::from_rgb(
                                                                            48, 58, 76,
                                                                        ),
                                                                    ))
                                                                    .rounding(6.0)
                                                                    .inner_margin(10.0)
                                                                    .show(ui, |ui| {
                                                                        ui.set_min_width(
                                                                            list_inner_width - 30.0,
                                                                        );
                                                                        let duration_ms = item
                                                                            .duration_ms
                                                                            .max(1000);
                                                                        if self.batch_running
                                                                            || self.is_generating
                                                                        {
                                                                            batch_trim_timeline_static_ui(
                                                                                ui, &item, accent,
                                                                                success, muted,
                                                                            );
                                                                        } else {
                                                                            self.trim_timeline_ui(
                                                                                ui,
                                                                                duration_ms,
                                                                                accent,
                                                                                success,
                                                                                muted,
                                                                            );
                                                                        }
                                                                        ui.horizontal(|ui| {
                                                                            ui.with_layout(
                                                                                egui::Layout::right_to_left(
                                                                                    egui::Align::Center,
                                                                                ),
                                                                                |ui| {
                                                                                    let is_playing = self
                                                                                        .is_preview_playing();
                                                                                    if ui
                                                                                        .add_enabled(
                                                                                            !self.batch_running
                                                                                                && !self.is_generating,
                                                                                            egui::Button::new(if is_playing {
                                                                                                "Стоп"
                                                                                            } else {
                                                                                                "Прослушать"
                                                                                            }),
                                                                                        )
                                                                                        .clicked()
                                                                                    {
                                                                                        if is_playing {
                                                                                            self.stop_preview();
                                                                                            self.trim_status = "Предпросмотр остановлен.".to_string();
                                                                                        } else {
                                                                                            self.preview_trimmed_audio();
                                                                                        }
                                                                                    }
                                                                                    if ui
                                                                                        .add_enabled(
                                                                                            !self.batch_running
                                                                                                && !self.is_generating,
                                                                                            egui::Button::new("Сбросить"),
                                                                                        )
                                                                                        .clicked()
                                                                                    {
                                                                                        self.stop_preview();
                                                                                        self.trim_start_ms = 0;
                                                                                        self.trim_end_ms = duration_ms;
                                                                                        self.trim_playhead_ms = 0;
                                                                                        self.fade_in_ms = 0;
                                                                                        self.fade_out_ms = 0;
                                                                                        self.trim_status = format!(
                                                                                            "Обрезка сброшена: {}",
                                                                                            format_time_ms(duration_ms)
                                                                                        );
                                                                                    }
                                                                                },
                                                                            );
                                                                        });
                                                                        if !self.trim_status.is_empty() {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    &self
                                                                                        .trim_status,
                                                                                )
                                                                                .size(10.5)
                                                                                .color(muted),
                                                                            );
                                                                        }
                                                                        if !self.batch_running
                                                                            && !self.is_generating
                                                                        {
                                                                            if let Some(item_mut) =
                                                                                self.batch_items
                                                                                    .get_mut(idx)
                                                                            {
                                                                                item_mut
                                                                                    .trim_start_ms =
                                                                                    self.trim_start_ms;
                                                                                item_mut
                                                                                    .trim_end_ms =
                                                                                    self.trim_end_ms;
                                                                                item_mut
                                                                                    .fade_in_ms =
                                                                                    self.fade_in_ms;
                                                                                item_mut
                                                                                    .fade_out_ms =
                                                                                    self.fade_out_ms;
                                                                            }
                                                                        }
                                                                    });
                                                            }
                                                            ui.add_space(4.0);
                                                        }
                                                    });
                                            },
                                        );
                                    },
                                );

                                ui.add_space(column_gap);

                                ui.allocate_ui_with_layout(
                                    egui::vec2(column_width, column_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        if let Some(index) = select_request {
                                            self.batch_selected_index = Some(index);
                                            let _ = self.apply_batch_item_to_single_state(index);
                                        }

                                        let index = self
                                            .batch_selected_index
                                            .filter(|idx| *idx < self.batch_items.len())
                                            .unwrap_or(0);
                                        let item_snapshot = self.batch_items[index].clone();
                                        let name = if item_snapshot.artist.is_empty() {
                                            item_snapshot.title.clone()
                                        } else {
                                            format!(
                                                "{} - {}",
                                                item_snapshot.artist, item_snapshot.title
                                            )
                                        };

                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Текст")
                                                    .strong()
                                                    .size(14.0)
                                                    .color(text),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{:03}",
                                                            index + 1
                                                        ))
                                                        .monospace()
                                                        .size(12.0)
                                                        .color(muted),
                                                    );
                                                },
                                            );
                                        });
                                        ui.add_space(6.0);

                                        let (body_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(column_width, body_height),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            body_rect,
                                            6.0,
                                            egui::Color32::from_rgb(12, 15, 21),
                                        );
                                        ui.painter().rect_stroke(
                                            body_rect,
                                            6.0,
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgb(34, 40, 52),
                                            ),
                                        );
                                        let inner_rect = body_rect.shrink(10.0);
                                        ui.allocate_new_ui(
                                            egui::UiBuilder::new().max_rect(inner_rect),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(name)
                                                        .strong()
                                                        .size(14.0)
                                                        .color(text),
                                                );
                                                let lyrics_path_label =
                                                    if item_snapshot.lyrics_path.as_os_str().is_empty() {
                                                        "LRC/TXT не найден".to_string()
                                                    } else {
                                                        item_snapshot.lyrics_path.to_string_lossy().to_string()
                                                    };
                                                ui.label(
                                                    egui::RichText::new(lyrics_path_label)
                                                        .size(10.0)
                                                        .color(muted),
                                                );
                                                ui.add_space(8.0);
                                                let text_top = ui.cursor().top();
                                                let text_height =
                                                    (inner_rect.bottom() - text_top).max(160.0);
                                                egui::ScrollArea::vertical()
                                                    .id_salt("batch_lyrics_text_simple")
                                                    .max_height(text_height)
                                                    .auto_shrink([false, false])
                                                    .show(ui, |ui| {
                                                        let edit_response = ui.add(
                                                            egui::TextEdit::multiline(&mut self.lyrics)
                                                                .font(egui::TextStyle::Monospace)
                                                                .desired_width(ui.available_width())
                                                                .desired_rows(16)
                                                        );
                                                        if edit_response.changed() {
                                                            if let Some(selected_idx) = self.batch_selected_index {
                                                                if let Some(item) = self.batch_items.get_mut(selected_idx) {
                                                                    let path = if item.lyrics_path.as_os_str().is_empty() {
                                                                        item.folder.join("lyrics.txt")
                                                                    } else {
                                                                        item.lyrics_path.clone()
                                                                    };
                                                                    if std::fs::write(&path, &self.lyrics).is_ok() {
                                                                        item.lyrics_path = path;
                                                                        if item.status == BatchStatus::MissingLyrics
                                                                            && !self.lyrics.trim().is_empty()
                                                                        {
                                                                            item.status = BatchStatus::Ready;
                                                                            item.progress = 0.0;
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    });
                                            },
                                        );
                                    },
                                );
                            });

                            if let Some(index) = remove_request {
                                if index < self.batch_items.len()
                                    && !self.batch_running
                                    && !self.is_generating
                                {
                                    self.batch_items.remove(index);
                                    self.batch_selected_index = if self.batch_items.is_empty() {
                                        None
                                    } else {
                                        Some(index.min(self.batch_items.len() - 1))
                                    };
                                    if let Some(selected) = self.batch_selected_index {
                                        let _ = self.apply_batch_item_to_single_state(selected);
                                    } else {
                                        self.audio_path = None;
                                        self.lyrics.clear();
                                    }
                                }
                            }
                        }
                    });
                    ui.add_space(page_margin);
                });
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
                                        if ui.button("Выбрать файл").clicked() && !self.dl_is_running {
                                            if let Some(path) = rfd::FileDialog::new()
                                                .add_filter("Excel Files", &["xlsx"])
                                                .pick_file()
                                            {
                                                self.dl_excel_path = Some(path.clone());
                                                self.start_parsing_excel(path, ctx.clone());
                                            }
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
                                        if ui.button("Выбрать папку").clicked() && !self.dl_is_running {
                                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                                self.dl_output_dir = Some(path);
                                            }
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
                                                egui::Button::new(egui::RichText::new("▶ Начать заново").strong()).fill(egui::Color32::from_rgb(45, 118, 255))
                                            ).clicked() {
                                                self.start_download(ctx, false);
                                            }

                                            let has_incomplete = self.dl_tracks.iter().any(|t| {
                                                t.selected && t.status != TrackStatus::Success && t.status != TrackStatus::Skipped
                                            });
                                            let can_continue = can_start && has_incomplete;

                                            if ui.add_enabled(
                                                can_continue,
                                                egui::Button::new(egui::RichText::new("⏯ Продолжить").strong()).fill(egui::Color32::from_rgb(34, 197, 94))
                                            ).clicked() {
                                                self.start_download(ctx, true);
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
                                         let failed_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Failed).count();
                                         let skipped_count = self.dl_tracks.iter().filter(|t| t.selected && t.status == TrackStatus::Skipped).count();
                                         let finished_count = success_count + failed_count + skipped_count;

                                         let progress = finished_count as f32 / total_selected as f32;
                                         ui.add(
                                             egui::ProgressBar::new(progress)
                                                 .text(format!("Обработано: {} из {}", finished_count, total_selected))
                                                 .show_percentage()
                                         );
                                         ui.add_space(8.0);

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
                                                                TrackStatus::Failed => ("❌ Ошибка", egui::Color32::from_rgb(239, 68, 68)),
                                                                TrackStatus::Skipped => ("⏭️ Пропущен", egui::Color32::from_rgb(156, 163, 175)),
                                                            };
                                                            ui.label(egui::RichText::new(status_text).color(status_color));
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
                            ui.set_width(content_width.max(520.0));
                            card_frame.show(ui, |ui| {
                                let inner_width = (content_width - 36.0).max(760.0);
                                ui.set_min_width(inner_width);

                                ui.label(
                                    egui::RichText::new("Загрузчик аудио")
                                        .strong()
                                        .size(18.0)
                                        .color(text),
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
                                ui.horizontal(|ui| {
                                    ui.label("Название трека/запрос:");
                                    ui.text_edit_singleline(&mut self.dl_track_query);
                                });
                                ui.add_space(10.0);

                                // Папка сохранения
                                ui.horizontal(|ui| {
                                    ui.label("Папка для сохранения:");
                                    if ui.button("Выбрать папку").clicked() && !self.dl_is_running {
                                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                            self.dl_output_dir = Some(path);
                                        }
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

                                // Кнопки
                                ui.horizontal(|ui| {
                                    if self.dl_is_running {
                                        if ui.button(egui::RichText::new("⏹ Остановить загрузку").strong()).clicked() {
                                            self.request_stop_download();
                                        }
                                    } else {
                                        let can_start = !self.dl_track_query.trim().is_empty()
                                            && self.dl_output_dir.is_some();

                                        if ui.add_enabled(
                                            can_start,
                                            egui::Button::new(egui::RichText::new("Начать загрузку").strong()).fill(egui::Color32::from_rgb(45, 118, 255))
                                        ).clicked() {
                                            self.start_download(ctx, false);
                                        }
                                    }

                                    if ui.button("Очистить лог").clicked() {
                                        self.dl_log_output.clear();
                                    }
                                });
                                ui.add_space(14.0);

                                // Поле логов и статус
                                ui.label(egui::RichText::new(&self.dl_status_text).strong().color(accent));
                                ui.add_space(8.0);

                                let logs_height = (ui.available_height() - 36.0).max(200.0);
                                egui::ScrollArea::vertical()
                                    .max_height(logs_height)
                                    .id_salt("dl_log_scroll_single")
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::TextEdit::multiline(&mut self.dl_log_output)
                                                .font(egui::TextStyle::Monospace)
                                                .desired_width(f32::INFINITY)
                                                .desired_rows(12)
                                                .lock_focus(true)
                                        );
                                    });
                            });
                        });
                    }

                    ui.add_space(batch_margin);
                });
            } else {
            let total_width = ui.available_width();
            let spacing = 18.0;
            let col_width = ((total_width - page_margin * 2.0) - spacing) / 2.0;

            let result_height = if self.generated_file.is_some() {
                (ui.available_height() * 0.48).clamp(320.0, 520.0)
            } else {
                0.0
            };
            let main_height = (ui.available_height() - result_height - 18.0).max(220.0);

            ui.horizontal(|ui| {
                ui.add_space(page_margin);

                ui.allocate_ui_with_layout(
                    egui::vec2(col_width, main_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("left_settings_scroll")
                            .max_height(main_height)
                            .show(ui, |ui| {
                                card_frame.show(ui, |ui| {
                                    ui.label(egui::RichText::new("Исходник").strong().size(16.0).color(text));
                                    ui.label(egui::RichText::new("MP3-файл, исполнитель и название").size(12.0).color(muted));
                                    ui.add_space(12.0);

                                    drop_zone_frame.show(ui, |ui| {
                                        ui.vertical_centered(|ui| {
                                            let picker_text = match &self.audio_path {
                                                Some(path) => format!("Выбран файл: {}", std::path::Path::new(path).file_name().unwrap().to_string_lossy()),
                                                None => "Перетащите MP3 сюда или выберите файл".to_string(),
                                            };

                                            let text_color = if self.audio_path.is_some() {
                                                success
                                            } else {
                                                egui::Color32::from_rgb(198, 205, 216)
                                            };

                                            let file_btn = egui::Button::new(
                                                egui::RichText::new(picker_text)
                                                    .color(text_color)
                                                    .strong()
                                                    .size(12.0)
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .min_size(egui::vec2(ui.available_width(), 34.0));

                                            if ui.add(file_btn).clicked() && !self.is_generating {
                                                if let Some(path) = rfd::FileDialog::new()
                                                    .add_filter("Аудио", &["mp3"])
                                                    .pick_file()
                                                {
                                                    self.set_audio_file(path, ctx);
                                                }
                                            }

                                            if self.audio_path.is_some() {
                                                ui.add_space(4.0);
                                                ui.label(egui::RichText::new("Название и исполнитель заполнены из имени файла, если найден формат Artist - Title.").size(11.0).color(muted));
                                            }
                                        });
                                    });
                                    ui.add_space(16.0);

                                    if self.audio_path.is_some() {
                                        ui.label(
                                            egui::RichText::new("Обрезка аудио")
                                                .strong()
                                                .size(14.0)
                                                .color(text),
                                        );
                                        ui.label(
                                            egui::RichText::new(
                                                "Выберите рабочий фрагмент и прослушайте его перед генерацией.",
                                            )
                                            .size(12.0)
                                            .color(muted),
                                        );
                                        ui.add_space(8.0);

                                        if let Some(duration_ms) = self.audio_duration_ms {
                                            self.trim_timeline_ui(
                                                ui,
                                                duration_ms,
                                                accent,
                                                success,
                                                muted,
                                            );
                                            ui.add_space(4.0);

                                            ui.horizontal(|ui| {
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        let is_playing = self.is_preview_playing();
                                                        if is_playing {
                                                            if ui
                                                                .add_enabled(
                                                                    !self.is_generating,
                                                                    egui::Button::new("Стоп"),
                                                                )
                                                                .clicked()
                                                            {
                                                                self.stop_preview();
                                                                self.trim_status =
                                                                    "Предпросмотр остановлен."
                                                                        .to_string();
                                                            }
                                                        } else if ui
                                                            .add_enabled(
                                                                !self.is_generating,
                                                                egui::Button::new(
                                                                    "Прослушать с позиции",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.preview_trimmed_audio();
                                                        }
                                                        if ui
                                                            .add_enabled(
                                                                !self.is_generating,
                                                                egui::Button::new("Сбросить"),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.stop_preview();
                                                            self.trim_start_ms = 0;
                                                            self.trim_end_ms = duration_ms;
                                                            self.trim_playhead_ms = 0;
                                                            self.fade_in_ms = 0;
                                                            self.fade_out_ms = 0;
                                                            self.trim_status = format!(
                                                                "Обрезка сброшена: {}",
                                                                format_time_ms(duration_ms)
                                                            );
                                                        }
                                                    },
                                                );
                                            });
                                        }

                                        if !self.trim_status.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&self.trim_status)
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                        }

                                        ui.add_space(18.0);
                                        ui.separator();
                                        ui.add_space(18.0);
                                    }

                                    ui.label(egui::RichText::new("Метаданные").strong().size(14.0).color(text));
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut self.artist)
                                            .hint_text("Исполнитель")
                                            .margin(egui::vec2(10.0, 8.0))
                                            .desired_width(ui.available_width() * 0.47));
                                        ui.add_space(10.0);
                                        ui.add(egui::TextEdit::singleline(&mut self.title)
                                            .hint_text("Название песни")
                                            .margin(egui::vec2(10.0, 8.0))
                                            .desired_width(ui.available_width()));
                                    });

                                    ui.add_space(18.0);
                                    ui.separator();
                                    ui.add_space(18.0);

                                    ui.label(egui::RichText::new("Рендер").strong().size(16.0).color(text));
                                    ui.label(egui::RichText::new("Модель, качество и шрифт будущего видео").size(12.0).color(muted));
                                    ui.add_space(8.0);
                                    egui::Grid::new("settings_grid")
                                        .num_columns(2)
                                        .spacing([14.0, 12.0])
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new("Модель Whisper").color(muted));
                                            egui::ComboBox::from_id_salt("model_combo")
                                                .selected_text(match self.model.as_str() {
                                                    "medium" => "medium (1.5 GB)",
                                                    "small" => "small (460 MB)",
                                                    _ => "base (140 MB)",
                                                })
                                                .width(ui.available_width() - 10.0)
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut self.model, "base".to_string(), "base (140 MB)");
                                                    ui.selectable_value(&mut self.model, "small".to_string(), "small (460 MB)");
                                                    ui.selectable_value(&mut self.model, "medium".to_string(), "medium (1.5 GB)");
                                                });
                                            ui.end_row();

                                            ui.label(egui::RichText::new("Качество").color(muted));
                                            egui::ComboBox::from_id_salt("quality_combo")
                                                .selected_text(match self.quality.as_str() {
                                                    "ultra" => "Ультра HD (CRF 12)",
                                                    "high" => "Высокое (CRF 17)",
                                                    _ => "Стандартное (CRF 23)",
                                                })
                                                .width(ui.available_width() - 10.0)
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut self.quality, "medium".to_string(), "Стандартное (CRF 23)");
                                                    ui.selectable_value(&mut self.quality, "high".to_string(), "Высокое (CRF 17)");
                                                    ui.selectable_value(&mut self.quality, "ultra".to_string(), "Ультра HD (CRF 12)");
                                                });
                                            ui.end_row();

                                            ui.label(egui::RichText::new("Шрифт").color(muted));
                                            egui::ComboBox::from_id_salt("font_combo")
                                                .selected_text(match self.font.as_str() {
                                                    "arial" => "Arial",
                                                    "helvetica" => "Helvetica",
                                                    "georgia" => "Georgia",
                                                    _ => "Montserrat (Bold)",
                                                })
                                                .width(ui.available_width() - 10.0)
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut self.font, "montserrat".to_string(), "Montserrat (Bold)");
                                                    ui.selectable_value(&mut self.font, "arial".to_string(), "Arial");
                                                    ui.selectable_value(&mut self.font, "helvetica".to_string(), "Helvetica");
                                                    ui.selectable_value(&mut self.font, "georgia".to_string(), "Georgia");
                                                });
                                            ui.end_row();
                                        });

                                    ui.add_space(18.0);
                                    ui.separator();
                                    ui.add_space(18.0);

                                    ui.label(egui::RichText::new("Внешний вид и тайминг").strong().size(16.0).color(text));
                                    ui.label(egui::RichText::new("Цвета субтитров и ручная поправка синхронизации").size(12.0).color(muted));
                                    ui.add_space(8.0);

                                    ui.horizontal(|ui| {
                                        ui.color_edit_button_srgb(&mut self.color_active);
                                        ui.label(egui::RichText::new("Активный").size(12.0).color(text));
                                        ui.add_space(12.0);
                                        ui.color_edit_button_srgb(&mut self.color_inactive);
                                        ui.label(egui::RichText::new("Будущий").size(12.0).color(text));
                                        ui.add_space(12.0);
                                        ui.color_edit_button_srgb(&mut self.color_bg);
                                        ui.label(egui::RichText::new("Фон").size(12.0).color(text));
                                    });
                                    ui.add_space(14.0);

                                    ui.label(egui::RichText::new("Прозрачность соседних строк").color(muted));
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            egui::vec2(ui.available_width() - 90.0, 20.0),
                                            egui::Slider::new(&mut self.inactive_opacity, 0.0..=1.0).show_value(false)
                                        );
                                        ui.label(egui::RichText::new(format!("{}%", (self.inactive_opacity * 100.0).round() as i32)).strong().color(accent));
                                    });
                                    ui.add_space(14.0);

                                    ui.label(egui::RichText::new("Сдвиг синхронизации").color(muted));
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            egui::vec2(ui.available_width() - 130.0, 20.0),
                                            egui::Slider::new(&mut self.audio_delay_ms, -500..=500).show_value(false)
                                        );
                                        ui.label(egui::RichText::new(format!("{:+} мс", self.audio_delay_ms)).strong().color(accent));
                                        ui.add_space(4.0);
                                        if ui.button("Сброс").clicked() {
                                            self.audio_delay_ms = 0;
                                        }
                                    });
                                    ui.add_space(12.0);
                                    ui.checkbox(
                                        &mut self.plain_lines,
                                        "Только строки без подсветки слов",
                                    );
                                });
                            });
                    }
                );

                ui.add_space(spacing);

                ui.allocate_ui_with_layout(
                    egui::vec2(col_width, main_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("right_scroll_area")
                            .max_height(main_height)
                            .show(ui, |ui| {
                                card_frame.show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(egui::RichText::new("Текст песни").strong().size(16.0).color(text));
                                            ui.label(egui::RichText::new("Вставьте текст или перетащите сюда .txt/.lrc файл").size(12.0).color(muted));
                                        });
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            let lines = self.lyrics.lines().filter(|line| !line.trim().is_empty()).count();
                                            ui.label(egui::RichText::new(format!("{} строк", lines)).size(12.0).color(muted));
                                        });
                                    });
                                    ui.add_space(8.0);

                                    ui.add(egui::TextEdit::multiline(&mut self.lyrics)
                                        .hint_text("Вставьте текст песни построчно или перетащите .txt/.lrc файл...")
                                        .desired_width(ui.available_width())
                                        .desired_rows(16)
                                        .font(egui::TextStyle::Monospace));

                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(
                                                !self.is_generating,
                                                egui::Button::new(
                                                    egui::RichText::new("Загрузить .txt/.lrc")
                                                        .size(12.0)
                                                        .strong(),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            if let Some(path) = rfd::FileDialog::new()
                                                .add_filter("Текст песни", &["txt", "lrc"])
                                                .pick_file()
                                            {
                                                self.set_lyrics_file(path);
                                            }
                                        }
                                        ui.label(egui::RichText::new("LRC с таймкодами тоже поддерживается.").size(11.0).color(muted));
                                    });
                                });
                                ui.add_space(12.0);

                                card_frame.show(ui, |ui| {
                                    let is_ready = self.audio_path.is_some() && !self.lyrics.trim().is_empty();
                                    let btn_text = if self.is_generating { "ГЕНЕРАЦИЯ..." } else { "СГЕНЕРИРОВАТЬ ВИДЕО" };

                                    let btn_color = if self.is_generating {
                                        egui::Color32::from_rgb(44, 51, 65)
                                    } else if is_ready {
                                        egui::Color32::from_rgb(45, 118, 255)
                                    } else {
                                        egui::Color32::from_rgb(42, 48, 60)
                                    };

                                    let gen_btn = egui::Button::new(
                                        egui::RichText::new(btn_text)
                                            .strong()
                                            .size(14.0)
                                            .color(egui::Color32::WHITE)
                                    )
                                    .min_size(egui::vec2(ui.available_width(), 44.0))
                                    .fill(btn_color)
                                    .rounding(8.0);

                                    ui.add_enabled_ui(is_ready && !self.is_generating, |ui| {
                                        if ui.add(gen_btn).clicked() {
                                            self.start_generation(ctx.clone());
                                        }
                                    });

                                    if self.is_generating || self.progress > 0.0 {
                                        ui.add_space(12.0);
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(&self.status_text).strong().color(text));
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                ui.label(egui::RichText::new(format!("{}%", (self.progress * 100.0) as i32)).strong().color(accent));
                                            });
                                        });
                                        ui.add_space(6.0);
                                        ui.add(egui::ProgressBar::new(self.progress).animate(self.is_generating));

                                        if !self.is_generating && self.status_text.contains("успешно") {
                                            if let Some(ref file_path) = self.generated_file {
                                                let path = std::path::PathBuf::from(file_path);
                                                if path.exists() {
                                                    ui.add_space(6.0);
                                                    if ui.button("📁 Открыть папку с видео").clicked() {
                                                        open_in_explorer(&path);
                                                    }
                                                }
                                            }
                                        }

                                        ui.add_space(10.0);
                                        log_frame.show(ui, |ui| {
                                            egui::ScrollArea::vertical()
                                                .max_height(100.0)
                                                .id_salt("log_scroll")
                                                .stick_to_bottom(true)
                                                .show(ui, |ui| {
                                                    ui.add(egui::TextEdit::multiline(&mut self.log_output)
                                                        .desired_width(ui.available_width())
                                                        .desired_rows(4)
                                                        .font(egui::TextStyle::Monospace)
                                                        .text_color(egui::Color32::from_rgb(150, 196, 255))
                                                        .interactive(false));
                                                });
                                        });
                                    }
                                });
                            });
                    }
                );

                ui.add_space(page_margin);
            });

            if let Some(file_path) = self.generated_file.clone() {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(page_margin);
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width() - page_margin * 2.0, 78.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            card_frame.show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Видео готово")
                                                .strong()
                                                .size(16.0)
                                                .color(success),
                                        );
                                        ui.label(
                                            egui::RichText::new(
                                                "Проверьте результат прямо здесь или сохраните копию в нужное место.",
                                            )
                                            .size(12.0)
                                            .color(muted),
                                        );
                                        if !self.video_status.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&self.video_status)
                                                    .size(11.0)
                                                    .color(muted),
                                            );
                                        }
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let save_btn = egui::Button::new(
                                                egui::RichText::new("СОХРАНИТЬ КАК...")
                                                    .strong()
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(36, 156, 112))
                                            .rounding(8.0)
                                            .min_size(egui::vec2(160.0, 36.0));

                                            if ui.add(save_btn).clicked() {
                                                let default_name = format!(
                                                    "{} - {} (karaoke).mp4",
                                                    self.artist, self.title
                                                )
                                                .replace("/", "_")
                                                .replace("\\", "_");

                                                if let Some(dest_path) = rfd::FileDialog::new()
                                                    .set_file_name(&default_name)
                                                    .add_filter("Видео", &["mp4"])
                                                    .save_file()
                                                {
                                                    if let Err(e) = std::fs::copy(
                                                        file_path.as_str(),
                                                        dest_path,
                                                    ) {
                                                        self.log_output.push_str(&format!(
                                                            "❌ Ошибка копирования: {}\n",
                                                            e
                                                        ));
                                                    } else {
                                                        self.log_output.push_str(
                                                            "✅ Видеоролик успешно сохранен!\n",
                                                        );
                                                    }
                                                }
                                            }

                                        },
                                    );
                                });
                            });
                        },
                    );
                    ui.add_space(page_margin);
                });
            }

            if let Some(file_path) = self.generated_file.clone() {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(page_margin);
                    let available_width = ui.available_width() - page_margin * 2.0;
                    let preview_max_height = (result_height - 132.0).clamp(180.0, 380.0);
                    let preview_size = if let Some(texture) = &self.video_texture {
                        let texture_size = texture.size_vec2();
                        let aspect = if texture_size.y > 0.0 {
                            texture_size.x / texture_size.y
                        } else {
                            16.0 / 9.0
                        };
                        egui::vec2(available_width, (available_width / aspect).min(preview_max_height))
                    } else {
                        egui::vec2(
                            available_width,
                            (available_width * 9.0 / 16.0).min(preview_max_height),
                        )
                    };

                    card_frame.show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Предпросмотр")
                                        .strong()
                                        .size(13.0)
                                        .color(text),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(if self.is_video_playing() {
                                                "идет воспроизведение"
                                            } else {
                                                "готов к запуску"
                                            })
                                            .size(11.0)
                                            .color(muted),
                                        );
                                    },
                                );
                            });
                            ui.add_space(8.0);

                            let (preview_rect, preview_response) =
                                ui.allocate_exact_size(preview_size, egui::Sense::click());
                            let painter = ui.painter();
                            painter.rect_filled(
                                preview_rect,
                                8.0,
                                egui::Color32::from_rgb(8, 10, 14),
                            );

                            if let Some(texture) = &self.video_texture {
                                painter.image(
                                    texture.id(),
                                    preview_rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                            } else {
                                painter.rect_filled(
                                    preview_rect,
                                    8.0,
                                    egui::Color32::from_rgb(8, 10, 14),
                                );
                                painter.text(
                                    egui::pos2(
                                        preview_rect.center().x,
                                        preview_rect.center().y + 48.0,
                                    ),
                                    egui::Align2::CENTER_CENTER,
                                    "Нажмите play, чтобы посмотреть видео здесь",
                                    egui::FontId::proportional(13.0),
                                    muted,
                                );
                            }

                            painter.rect_stroke(
                                preview_rect,
                                8.0,
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 58, 74)),
                            );

                            let is_video_playing = self.is_video_playing();
                            if !is_video_playing {
                                painter.circle_filled(preview_rect.center(), 28.0, accent);
                                painter.text(
                                    preview_rect.center() + egui::vec2(2.0, 0.0),
                                    egui::Align2::CENTER_CENTER,
                                    "▶",
                                    egui::FontId::proportional(26.0),
                                    egui::Color32::WHITE,
                                );
                            }

                            if preview_response.clicked() && !is_video_playing {
                                if let Err(err) = self.start_video_preview(&file_path, ctx) {
                                    self.video_status = err;
                                }
                            }

                            let controls_rect = egui::Rect::from_min_max(
                                egui::pos2(preview_rect.left() + 12.0, preview_rect.bottom() - 54.0),
                                egui::pos2(preview_rect.right() - 12.0, preview_rect.bottom() - 12.0),
                            );
                            painter.rect_filled(
                                controls_rect,
                                8.0,
                                egui::Color32::from_rgba_unmultiplied(12, 16, 24, 232),
                            );

                            ui.allocate_new_ui(
                                egui::UiBuilder::new().max_rect(controls_rect.shrink(6.0)),
                                |ui| {
                                ui.horizontal_centered(|ui| {
                                    let is_video_playing = self.is_video_playing();
                                    let play_label = if is_video_playing { "Пауза" } else { "▶" };
                                    let play_btn = egui::Button::new(
                                        egui::RichText::new(play_label)
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(if is_video_playing {
                                        egui::Color32::from_rgb(78, 86, 103)
                                    } else {
                                        accent
                                    })
                                    .rounding(8.0)
                                    .min_size(egui::vec2(58.0, 30.0));

                                    if ui.add(play_btn).clicked() {
                                        if is_video_playing {
                                            self.pause_video_preview();
                                        } else if let Err(err) =
                                            self.start_video_preview(&file_path, ctx)
                                        {
                                            self.video_status = err;
                                        }
                                    }

                                    let stop_btn = egui::Button::new(
                                        egui::RichText::new("Стоп")
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(48, 56, 70))
                                    .rounding(8.0)
                                    .min_size(egui::vec2(66.0, 30.0));

                                    if ui.add(stop_btn).clicked() {
                                        self.stop_video_preview();
                                    }

                                    let mut position = self
                                        .video_position_ms
                                        .clamp(0, self.video_duration_ms.max(0));
                                    let slider_enabled = self.video_duration_ms > 0;
                                    let slider_width = (ui.available_width() - 96.0).max(120.0);
                                    let slider = egui::Slider::new(
                                        &mut position,
                                        0..=self.video_duration_ms.max(1),
                                    )
                                    .show_value(false);

                                    let slider_response = ui.add_sized(
                                        egui::vec2(slider_width, 28.0),
                                        slider.text("Позиция воспроизведения"),
                                    );
                                    if slider_enabled && slider_response.changed() {
                                        self.video_position_ms = position;
                                        if is_video_playing {
                                            self.pause_video_preview();
                                        }
                                        self.video_status = format!(
                                            "Позиция: {}.",
                                            format_time_ms(self.video_position_ms)
                                        );
                                    }

                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} / {}",
                                            format_time_ms(self.video_position_ms),
                                            if self.video_duration_ms > 0 {
                                                format_time_ms(self.video_duration_ms)
                                            } else {
                                                "--:--".to_string()
                                            }
                                        ))
                                        .size(12.0)
                                        .color(muted),
                                    );
                                });
                                },
                            );
                        });
                    });
                    ui.add_space(page_margin);
                });
            }
            }
        });

        // Автосохранение настроек при изменении
        let new_settings = AppSettings {
            model: self.model.clone(),
            quality: self.quality.clone(),
            font: self.font.clone(),
            color_active: self.color_active,
            color_inactive: self.color_inactive,
            color_bg: self.color_bg,
            inactive_opacity: self.inactive_opacity,
            audio_delay_ms: self.audio_delay_ms,
            fade_in_ms: self.fade_in_ms,
            fade_out_ms: self.fade_out_ms,
            artist: self.artist.clone(),
            title: self.title.clone(),
            lyrics: self.lyrics.clone(),
            plain_lines: self.plain_lines,
        };

        if old_settings != new_settings {
            if let Ok(data) = serde_json::to_string_pretty(&new_settings) {
                let _ = std::fs::write(settings_path(), data);
            }
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!(
                "Караоке-Видео Генератор v{} (Word-Level)",
                APP_VERSION
            ))
            .with_icon(app_icon())
            .with_inner_size([1100.0, 750.0])
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
    } else if log_line.contains("[-] [") {
        if let Some(pos) = extract_track_num_between(log_line, "[-] [", "]") {
            update_status_by_pos(tracks, pos, TrackStatus::Failed);
        }
    } else if log_line.contains("[-] Thread error processing track ") {
        if let Some(pos) = extract_track_num_after(log_line, "[-] Thread error processing track ") {
            update_status_by_pos(tracks, pos, TrackStatus::Failed);
        }
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
