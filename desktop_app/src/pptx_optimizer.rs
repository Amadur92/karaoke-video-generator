use crate::ffmpeg;
use crate::paths;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Clone, Debug)]
pub struct OptimizerConfig {
    pub input: PathBuf,
    pub output: PathBuf,
    pub work_dir: PathBuf,
    pub replacement_media_dirs: Vec<PathBuf>,
    pub width: u32,
    pub crf: u8,
    pub preset: String,
    pub audio_bitrate: String,
    pub workers: usize,
}

#[derive(Clone, Debug)]
pub enum OptimizerEvent {
    Plan {
        total: usize,
        remaining: usize,
    },
    Done {
        index: usize,
        total: usize,
        message: String,
    },
    Log(String),
    Summary(OptimizerSummary),
}

#[derive(Clone, Debug)]
pub struct OptimizerSummary {
    pub output_mb: f32,
    pub mp4_mb: f32,
    pub mp4_count: usize,
}

#[derive(Clone, Debug)]
struct MediaJob {
    name: String,
    size: u64,
    replacement: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct OptimizedRow {
    name: String,
    before: u64,
    after: u64,
    seconds: f32,
    source: &'static str,
}

#[derive(Clone, Debug)]
struct VideoInfo {
    width: u32,
    duration: f64,
}

fn probe_video(path: &Path) -> Result<VideoInfo, String> {
    let output = std::process::Command::new(paths::tool_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,codec_name",
            "-show_entries",
            "format=duration,bit_rate",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|err| format!("ffprobe не запустился: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("ffprobe вернул не JSON: {err}"))?;
    let stream = value
        .get("streams")
        .and_then(|streams| streams.get(0))
        .ok_or_else(|| "видео-поток не найден".to_string())?;
    let format = value
        .get("format")
        .ok_or_else(|| "format в ffprobe не найден".to_string())?;
    let width = stream.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let duration = format
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);

    Ok(VideoInfo { width, duration })
}

fn supports_h264_videotoolbox() -> bool {
    static SUPPORTS: OnceLock<bool> = OnceLock::new();
    *SUPPORTS.get_or_init(|| {
        let output = std::process::Command::new(paths::tool_path("ffmpeg"))
            .args(["-hide_banner", "-encoders"])
            .output();
        output
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("h264_videotoolbox")
            })
            .unwrap_or(false)
    })
}

fn is_valid_optimized(path: &Path, width: u32) -> bool {
    if !path.exists()
        || path
            .metadata()
            .map(|meta| meta.len() < 1000)
            .unwrap_or(true)
    {
        return false;
    }
    probe_video(path)
        .map(|info| info.width <= width + 8 && info.duration > 1.0)
        .unwrap_or(false)
}

fn extract_media(source_pptx: &Path, media_name: &str, destination: &Path) -> Result<(), String> {
    let file = File::open(source_pptx).map_err(|err| format!("PPTX не открылся: {err}"))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("PPTX не читается: {err}"))?;
    let mut media = archive
        .by_name(media_name)
        .map_err(|err| format!("{media_name}: не найдено в PPTX ({err})"))?;
    let mut out = File::create(destination).map_err(|err| format!("temp не создан: {err}"))?;
    std::io::copy(&mut media, &mut out).map_err(|err| format!("temp не записан: {err}"))?;
    Ok(())
}

fn media_number(path: &Path) -> Option<usize> {
    let stem = path.file_stem()?.to_string_lossy();
    stem.strip_prefix("media")?.parse::<usize>().ok()
}

fn collect_replacement_files(dirs: &[PathBuf]) -> HashMap<usize, PathBuf> {
    let mut files = HashMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file()
                || !path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("mp4"))
                    .unwrap_or(false)
            {
                continue;
            }
            if let Some(number) = media_number(&path) {
                files.insert(number, path);
            }
        }
    }
    files
}

#[derive(Clone, Debug)]
struct VideoSlot {
    target: String,
    x: i64,
    cx: i64,
    order: usize,
}

fn xml_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn relationship_attr(tag: &str, attr: &str) -> Option<String> {
    xml_attr(tag, attr)
}

fn normalize_media_target(target: &str) -> Option<String> {
    let filename = Path::new(target).file_name()?.to_string_lossy();
    if filename.to_lowercase().ends_with(".mp4") {
        Some(format!("ppt/media/{filename}"))
    } else {
        None
    }
}

fn slide_number(name: &str) -> Option<usize> {
    let file_name = Path::new(name).file_name()?.to_string_lossy();
    file_name
        .strip_prefix("slide")?
        .strip_suffix(".xml")?
        .parse::<usize>()
        .ok()
}

fn parse_i64_attr(tag: &str, attr: &str) -> Option<i64> {
    xml_attr(tag, attr)?.parse::<i64>().ok()
}

fn video_rid_from_pic(pic_xml: &str) -> Option<String> {
    let video_pos = pic_xml.find("videoFile")?;
    let after_video = &pic_xml[video_pos..];
    xml_attr(after_video, "r:link").or_else(|| xml_attr(after_video, "link"))
}

fn video_geometry_from_pic(pic_xml: &str) -> Option<(i64, i64)> {
    let off_pos = pic_xml.find("<a:off")?;
    let off_tag = pic_xml[off_pos..].split('>').next().unwrap_or("");
    let ext_pos = pic_xml.find("<a:ext")?;
    let ext_tag = pic_xml[ext_pos..].split('>').next().unwrap_or("");
    Some((
        parse_i64_attr(off_tag, "x")?,
        parse_i64_attr(ext_tag, "cx")?,
    ))
}

fn video_slots_by_geometry(source_pptx: &Path) -> Result<Vec<VideoSlot>, String> {
    let file = File::open(source_pptx).map_err(|err| format!("PPTX не открылся: {err}"))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("PPTX не читается: {err}"))?;
    let mut slide_names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|err| format!("PPTX entry #{index} не читается: {err}"))?;
        let name = entry.name().to_string();
        if name.starts_with("ppt/slides/slide")
            && name.ends_with(".xml")
            && let Some(number) = slide_number(&name)
        {
            slide_names.push((number, name));
        }
    }
    slide_names.sort_by_key(|(number, _)| *number);

    let file = File::open(source_pptx).map_err(|err| format!("PPTX не открылся: {err}"))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("PPTX не читается: {err}"))?;
    let mut slots = Vec::new();
    for (_, slide_name) in slide_names {
        let slide_number = slide_number(&slide_name).unwrap_or(0);
        let rel_name = format!("ppt/slides/_rels/slide{slide_number}.xml.rels");
        let mut rel_entry = match archive.by_name(&rel_name) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let mut rel_xml = String::new();
        rel_entry
            .read_to_string(&mut rel_xml)
            .map_err(|err| format!("{rel_name}: не UTF-8 ({err})"))?;
        drop(rel_entry);

        let mut rel_map = HashMap::new();
        for tag in rel_xml.split("<Relationship").skip(1) {
            let tag = tag.split('>').next().unwrap_or(tag);
            let rel_type = relationship_attr(tag, "Type").unwrap_or_default();
            let id = relationship_attr(tag, "Id").unwrap_or_default();
            let target = relationship_attr(tag, "Target").unwrap_or_default();
            if rel_type.ends_with("/video")
                && let Some(media_name) = normalize_media_target(&target)
            {
                rel_map.insert(id, media_name);
            }
        }
        if rel_map.is_empty() {
            continue;
        }

        let mut slide_entry = archive
            .by_name(&slide_name)
            .map_err(|err| format!("{slide_name}: не читается ({err})"))?;
        let mut slide_xml = String::new();
        slide_entry
            .read_to_string(&mut slide_xml)
            .map_err(|err| format!("{slide_name}: не UTF-8 ({err})"))?;
        drop(slide_entry);

        for pic in slide_xml.split("<p:pic").skip(1) {
            let pic_xml = pic.split("</p:pic>").next().unwrap_or(pic);
            let Some(rid) = video_rid_from_pic(pic_xml) else {
                continue;
            };
            let Some(target) = rel_map.get(&rid).cloned() else {
                continue;
            };
            let Some((x, cx)) = video_geometry_from_pic(pic_xml) else {
                continue;
            };
            slots.push(VideoSlot {
                target,
                x,
                cx,
                order: slots.len(),
            });
        }
    }
    Ok(slots)
}

fn song_video_targets_by_geometry(
    source_pptx: &Path,
    track_count: usize,
) -> Result<Vec<String>, String> {
    let slots = video_slots_by_geometry(source_pptx)?;
    if slots.len() < track_count {
        return Ok(Vec::new());
    }

    let mut clusters = HashMap::<(i64, i64), Vec<VideoSlot>>::new();
    for slot in slots {
        let key = (slot.x / 250_000, slot.cx / 250_000);
        clusters.entry(key).or_default().push(slot);
    }
    let Some((_, mut song_slots)) = clusters.into_iter().max_by_key(|(_, values)| values.len())
    else {
        return Ok(Vec::new());
    };
    if song_slots.len() < track_count {
        return Ok(Vec::new());
    }
    song_slots.sort_by_key(|slot| slot.order);
    song_slots.truncate(track_count);
    Ok(song_slots.into_iter().map(|slot| slot.target).collect())
}

fn build_replacement_map(
    source_pptx: &Path,
    dirs: &[PathBuf],
) -> Result<HashMap<String, PathBuf>, String> {
    let replacements = collect_replacement_files(dirs);
    if replacements.is_empty() {
        return Ok(HashMap::new());
    }
    let Some(track_count) = replacements.keys().copied().max() else {
        return Ok(HashMap::new());
    };
    let song_targets = song_video_targets_by_geometry(source_pptx, track_count)?;
    if song_targets.len() < track_count {
        return Ok(HashMap::new());
    }

    let mut map = HashMap::new();
    for track_number in 1..=track_count {
        let Some(replacement) = replacements.get(&track_number) else {
            continue;
        };
        if let Some(target) = song_targets.get(track_number - 1) {
            map.insert(target.clone(), replacement.clone());
        }
    }
    Ok(map)
}
fn optimize_one(config: &OptimizerConfig, job: &MediaJob) -> Result<OptimizedRow, String> {
    let started = Instant::now();
    let media_dir = config.work_dir.join("media");
    let base_name = Path::new(&job.name)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let raw = media_dir.join(format!("{base_name}.{}.src.mp4", std::process::id()));
    let out = media_dir.join(&base_name);
    let tmp = media_dir.join(format!("{base_name}.tmp.mp4"));

    if let Some(replacement) = &job.replacement
        && is_valid_optimized(replacement, config.width)
    {
        std::fs::copy(replacement, &out)
            .map_err(|err| format!("{base_name}: copy replacement failed: {err}"))?;
        let after = out
            .metadata()
            .map_err(|err| format!("{base_name}: metadata failed: {err}"))?
            .len();
        return Ok(OptimizedRow {
            name: job.name.clone(),
            before: job.size,
            after,
            seconds: started.elapsed().as_secs_f32(),
            source: "готовый MP4",
        });
    }

    if let Some(replacement) = &job.replacement {
        std::fs::copy(replacement, &raw)
            .map_err(|err| format!("{base_name}: copy replacement source failed: {err}"))?;
    } else {
        extract_media(&config.input, &job.name, &raw)?;
    }
    let input_info = probe_video(&raw)?;

    if input_info.width <= config.width + 8 && job.size < 8 * 1024 * 1024 {
        std::fs::copy(&raw, &out).map_err(|err| format!("{base_name}: copy failed: {err}"))?;
    } else {
        let use_videotoolbox = supports_h264_videotoolbox();
        let mut command = std::process::Command::new(paths::tool_path("ffmpeg"));
        command
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .arg("-i")
            .arg(&raw)
            .arg("-vf")
            .arg(format!("scale='min({},iw)':-2", config.width));
        if use_videotoolbox {
            command
                .args(["-c:v", "h264_videotoolbox"])
                .args(["-b:v", "850k"])
                .args(["-maxrate", "1200k"])
                .args(["-bufsize", "2400k"]);
        } else {
            command
                .args(["-c:v", "libx264"])
                .args(["-preset", &config.preset])
                .args(["-crf", &config.crf.to_string()]);
        }
        command
            .args(["-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac"])
            .args(["-b:a", &config.audio_bitrate])
            .args(["-movflags", "+faststart"])
            .args(["-f", "mp4"])
            .arg(&tmp)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        ffmpeg::hide_subprocess_window(&mut command);
        let output = command
            .output()
            .map_err(|err| format!("{base_name}: ffmpeg не запустился: {err}"))?;
        if !output.status.success() {
            return Err(format!(
                "{base_name}: ffmpeg error: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        std::fs::rename(&tmp, &out).map_err(|err| format!("{base_name}: rename failed: {err}"))?;
    }

    let _ = std::fs::remove_file(&raw);
    let _ = probe_video(&out)?;
    let after = out
        .metadata()
        .map_err(|err| format!("{base_name}: metadata failed: {err}"))?
        .len();
    Ok(OptimizedRow {
        name: job.name.clone(),
        before: job.size,
        after,
        seconds: started.elapsed().as_secs_f32(),
        source: "ffmpeg",
    })
}

fn list_media(source_pptx: &Path) -> Result<Vec<MediaJob>, String> {
    let file = File::open(source_pptx).map_err(|err| format!("PPTX не открылся: {err}"))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("PPTX не читается: {err}"))?;
    let mut jobs = Vec::new();
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|err| format!("PPTX entry #{index} не читается: {err}"))?;
        let name = file.name().to_string();
        if name.starts_with("ppt/media/") && name.to_lowercase().ends_with(".mp4") {
            jobs.push(MediaJob {
                name,
                size: file.size(),
                replacement: None,
            });
        }
    }
    Ok(jobs)
}

fn build_pptx(
    config: &OptimizerConfig,
    media_names: &[String],
) -> Result<OptimizerSummary, String> {
    let media_dir = config.work_dir.join("media");
    let input = File::open(&config.input).map_err(|err| format!("PPTX не открылся: {err}"))?;
    let mut source =
        ZipArchive::new(input).map_err(|err| format!("PPTX не читается как zip: {err}"))?;
    let tmp_output = config.output.with_extension("pptx.tmp");
    let output = File::create(&tmp_output).map_err(|err| format!("PPTX temp не создан: {err}"))?;
    let mut writer = ZipWriter::new(output);
    let deflated = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = FileOptions::default().compression_method(CompressionMethod::Stored);

    for index in 0..source.len() {
        let mut entry = source
            .by_index(index)
            .map_err(|err| format!("PPTX entry #{index} не читается: {err}"))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            writer
                .add_directory(name, deflated)
                .map_err(|err| format!("zip directory failed: {err}"))?;
            continue;
        }

        if media_names.iter().any(|media| media == &name) {
            let media_path = media_dir.join(
                Path::new(&name)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
            if !is_valid_optimized(&media_path, config.width) {
                return Err(format!("Оптимизированное видео не найдено: {name}"));
            }
            writer
                .start_file(name, stored)
                .map_err(|err| format!("zip media start failed: {err}"))?;
            let mut file = File::open(&media_path)
                .map_err(|err| format!("media не открылся {}: {err}", media_path.display()))?;
            std::io::copy(&mut file, &mut writer)
                .map_err(|err| format!("media не записался: {err}"))?;
        } else {
            writer
                .start_file(name, deflated)
                .map_err(|err| format!("zip entry start failed: {err}"))?;
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|err| format!("zip entry read failed: {err}"))?;
            writer
                .write_all(&data)
                .map_err(|err| format!("zip entry write failed: {err}"))?;
        }
    }
    writer
        .finish()
        .map_err(|err| format!("PPTX zip finish failed: {err}"))?;
    std::fs::rename(&tmp_output, &config.output)
        .map_err(|err| format!("PPTX temp rename failed: {err}"))?;

    let output =
        File::open(&config.output).map_err(|err| format!("Итоговый PPTX не открылся: {err}"))?;
    let mut archive =
        ZipArchive::new(output).map_err(|err| format!("Итоговый PPTX не читается: {err}"))?;
    let mut buffer = Vec::new();
    let mut mp4_count = 0usize;
    let mut mp4_size = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("Итоговый entry #{index} не читается: {err}"))?;
        buffer.clear();
        entry
            .read_to_end(&mut buffer)
            .map_err(|err| format!("Итоговый entry read failed: {err}"))?;
        let name = entry.name().to_lowercase();
        if name.starts_with("ppt/media/") && name.ends_with(".mp4") {
            mp4_count += 1;
            mp4_size += entry.size();
        }
    }

    Ok(OptimizerSummary {
        output_mb: config.output.metadata().map(|m| m.len()).unwrap_or(0) as f32 / 1024.0 / 1024.0,
        mp4_mb: mp4_size as f32 / 1024.0 / 1024.0,
        mp4_count,
    })
}

pub fn optimize_pptx(
    config: OptimizerConfig,
    mut on_event: impl FnMut(OptimizerEvent) + Send + 'static,
) -> Result<(), String> {
    std::fs::create_dir_all(config.work_dir.join("media"))
        .map_err(|err| format!("Не удалось создать work-dir: {err}"))?;

    let replacement_map = build_replacement_map(&config.input, &config.replacement_media_dirs)?;
    if !replacement_map.is_empty() {
        on_event(OptimizerEvent::Log(format!(
            "Быстрый режим: найдено готовых MP4 для замены: {}",
            replacement_map.len()
        )));
    }

    let mut media_jobs = list_media(&config.input)?;
    for job in &mut media_jobs {
        job.replacement = replacement_map.get(&job.name).cloned();
    }
    let media_names = media_jobs
        .iter()
        .map(|job| job.name.clone())
        .collect::<Vec<_>>();
    let mut pending = VecDeque::new();
    let mut skipped = 0usize;
    for job in media_jobs {
        let out = config.work_dir.join("media").join(
            Path::new(&job.name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        );
        if is_valid_optimized(&out, config.width) {
            skipped += 1;
        } else {
            let _ = std::fs::remove_file(&out);
            pending.push_back(job);
        }
    }

    let total = media_names.len();
    let remaining = pending.len();
    on_event(OptimizerEvent::Plan { total, remaining });

    let queue = Arc::new(Mutex::new(pending));
    let config = Arc::new(config);
    let results = Arc::new(Mutex::new(Vec::<Result<OptimizedRow, String>>::new()));
    let worker_count = config.workers.max(1);

    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let config = Arc::clone(&config);
        let results = Arc::clone(&results);
        handles.push(std::thread::spawn(move || {
            loop {
                let next = {
                    let mut queue = queue.lock().expect("optimizer queue poisoned");
                    queue.pop_front()
                };
                let Some(job) = next else {
                    break;
                };
                let result = optimize_one(&config, &job);
                let mut results = results.lock().expect("optimizer results poisoned");
                results.push(result);
            }
        }));
    }

    let mut reported = 0usize;
    loop {
        {
            let mut results = results.lock().expect("optimizer results poisoned");
            while !results.is_empty() {
                let result = results.remove(0);
                reported += 1;
                match result {
                    Ok(row) => {
                        let message = format!(
                            "{} [{}] {:.1}MB -> {:.1}MB {:.1}s",
                            Path::new(&row.name)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            row.source,
                            row.before as f32 / 1024.0 / 1024.0,
                            row.after as f32 / 1024.0 / 1024.0,
                            row.seconds
                        );
                        on_event(OptimizerEvent::Done {
                            index: reported,
                            total: remaining,
                            message,
                        });
                    }
                    Err(err) => return Err(err),
                }
            }
        }

        if reported >= remaining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "Поток оптимизации аварийно завершился".to_string())?;
    }

    if skipped > 0 {
        on_event(OptimizerEvent::Log(format!(
            "Использовано готовых оптимизированных видео из кэша: {skipped}"
        )));
    }

    let summary = build_pptx(&config, &media_names)?;
    on_event(OptimizerEvent::Summary(summary));
    Ok(())
}
