//! Обёртки над ffmpeg/ffprobe: чтение длительности и размера, обрезка аудио с
//! плавными переходами, подготовка звука для предпросмотра, а также разбор
//! прогресса ffmpeg из stdout/stderr. Скрывает консольное окно подпроцессов
//! на Windows.

use std::path::Path;

use crate::paths;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
pub fn hide_subprocess_window(cmd: &mut std::process::Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_subprocess_window(_cmd: &mut std::process::Command) {}

pub fn format_time_ms(ms: i64) -> String {
    let total_seconds = (ms.max(0) as f32 / 1000.0).round() as i64;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}", minutes, seconds)
}

pub fn parse_ffmpeg_hms_ms(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours = parts[0].parse::<f64>().ok()?;
    let minutes = parts[1].parse::<f64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;
    Some(((hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0).round() as i64)
}

pub fn parse_ffmpeg_time_ms(line: &str) -> Option<i64> {
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

pub fn is_ffmpeg_progress_key(line: &str) -> bool {
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

pub fn probe_audio_duration_ms(path: &str) -> Result<i64, String> {
    let mut cmd = std::process::Command::new(paths::tool_path("ffprobe"));
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

pub fn render_trimmed_audio(
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

    let mut cmd = std::process::Command::new(paths::tool_path("ffmpeg"));
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

pub fn probe_video_size(path: &str) -> Result<(usize, usize), String> {
    let mut cmd = std::process::Command::new(paths::tool_path("ffprobe"));
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

pub fn preview_video_size(path: &str) -> Result<(usize, usize), String> {
    let (width, height) = probe_video_size(path)?;
    let target_width = width.clamp(2, 720);
    if target_width == width {
        return Ok((width, height));
    }

    let scaled_height = ((height as f32 * target_width as f32 / width as f32).round() as usize)
        .max(2)
        .next_multiple_of(2);
    Ok((target_width, scaled_height))
}

pub fn render_video_preview_audio(input: &str, output: &Path, start_ms: i64) -> Result<(), String> {
    let mut cmd = std::process::Command::new(paths::tool_path("ffmpeg"));
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
