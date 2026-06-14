//! Работа с файлами текста/LRC: классификация, чтение, парсинг временных меток
//! и сдвиг таймингов под обрезку аудио. Также разбор "исполнитель - название" из
//! имени файла и сортировочные ключи для пакетного режима.

use std::path::{Path, PathBuf};

pub fn file_extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

pub fn is_audio_file(path: &Path) -> bool {
    matches!(file_extension_lower(path).as_deref(), Some("mp3"))
}

pub fn is_lyrics_file(path: &Path) -> bool {
    matches!(file_extension_lower(path).as_deref(), Some("txt" | "lrc"))
}

pub fn display_file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

pub fn read_lyrics_file(path: &Path) -> Result<String, String> {
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

pub fn parse_lrc_timestamp_ms(raw: &str) -> Option<i64> {
    let (minutes, rest) = raw.split_once(':')?;
    let minutes = minutes.trim().parse::<i64>().ok()?;
    let seconds = rest.trim().parse::<f64>().ok()?;
    Some(minutes * 60_000 + (seconds * 1000.0).round() as i64)
}

pub fn format_lrc_timestamp_ms(ms: i64) -> String {
    let ms = ms.max(0);
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let centiseconds = (ms % 1000) / 10;
    format!("{:02}:{:02}.{:02}", minutes, seconds, centiseconds)
}

pub fn shift_lrc_for_trim(lyrics: &str, trim_start_ms: i64, trim_duration_ms: i64) -> String {
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

pub fn parse_artist_title_from_stem(stem: &str) -> (String, String) {
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

pub fn folder_sort_key(path: &Path) -> (usize, String) {
    let name = display_file_name(path);
    let number = name
        .split_once('.')
        .and_then(|(prefix, _)| prefix.trim().parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    (number, name.to_lowercase())
}

pub fn find_matching_lyrics(audio_path: &Path, files: &[PathBuf]) -> Option<PathBuf> {
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
