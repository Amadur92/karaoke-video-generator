#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use image::{ImageBuffer, Rgba, RgbaImage, imageops};
use imageproc::drawing::draw_text_mut;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const MONTSERRAT_BOLD: &[u8] = include_bytes!("../../assets/Montserrat-Bold.ttf");
const MONTSERRAT_BLACK: &[u8] = include_bytes!("../../assets/Montserrat-Black.ttf");
const ASS_BASE_FONT_SIZE: f32 = 52.0;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
fn hide_subprocess_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_subprocess_window(_cmd: &mut Command) {}

#[derive(Debug, Deserialize)]
struct KaraokeWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct KaraokeLine {
    start: f64,
    end: f64,
    words: Vec<KaraokeWord>,
}

#[derive(Clone)]
struct WordLayer {
    start: f64,
    end: f64,
    paste_x: i32,
    fill_width: u32,
    image: RgbaImage,
}

struct LineCache {
    inactive: RgbaImage,
    active_inactive: RgbaImage,
    active_plain: RgbaImage,
    words: Vec<WordLayer>,
    width: u32,
    height: u32,
}

fn line_highlight_is_live(words: &[WordLayer], t: f64) -> bool {
    words
        .first()
        .zip(words.last())
        .map(|(first, last)| t >= first.start && t <= last.end)
        .unwrap_or(false)
}

fn paint_line_highlight(img: &mut RgbaImage, words: &[WordLayer], t: f64) {
    for layer in words {
        if t < layer.start {
            continue;
        }
        if t > layer.end {
            paste_alpha(img, &layer.image, layer.paste_x, 0, 1.0, None);
        } else {
            let progress =
                ((t - layer.start) / (layer.end - layer.start).max(0.001)).clamp(0.0, 1.0);
            let fill = (layer.fill_width as f64 * progress).floor() as u32;
            paste_alpha(img, &layer.image, layer.paste_x, 0, 1.0, Some(fill));
        }
    }
}

fn blend_images(a: &RgbaImage, b: &RgbaImage, amount: f32) -> RgbaImage {
    if amount <= 0.001 {
        return a.clone();
    }
    if amount >= 0.999 {
        return b.clone();
    }
    let inv = 1.0 - amount;
    let mut out = a.clone();
    for (dst, src) in out.pixels_mut().zip(b.pixels()) {
        dst.0[0] = (dst.0[0] as f32 * inv + src.0[0] as f32 * amount).round() as u8;
        dst.0[1] = (dst.0[1] as f32 * inv + src.0[1] as f32 * amount).round() as u8;
        dst.0[2] = (dst.0[2] as f32 * inv + src.0[2] as f32 * amount).round() as u8;
        dst.0[3] = (dst.0[3] as f32 * inv + src.0[3] as f32 * amount).round() as u8;
    }
    out
}

struct RenderConfig {
    timings: PathBuf,
    audio: PathBuf,
    output: PathBuf,
    quality: String,
    active: Rgba<u8>,
    inactive: Rgba<u8>,
    background: Rgba<u8>,
    inactive_opacity: f32,
    audio_delay: f64,
    font: String,
    engine: String,
    scrolling: bool,
    plain_lines: bool,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    debug_frame_time: Option<f64>,
    debug_frame_output: Option<PathBuf>,
}

fn parse_color(value: &str) -> Result<Rgba<u8>, String> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!("Цвет должен быть в формате #RRGGBB: {value}"));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
    Ok(Rgba([r, g, b, 255]))
}

fn ass_color(color: Rgba<u8>, alpha: u8) -> String {
    format!(
        "&H{alpha:02X}{:02X}{:02X}{:02X}",
        color.0[2], color.0[1], color.0[0]
    )
}

fn ass_time(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    let cs = centis % 100;
    let total_seconds = centis / 100;
    let s = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let m = total_minutes % 60;
    let h = total_minutes / 60;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

fn escape_ass_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', " ")
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

fn usage() -> ! {
    eprintln!(
        "Usage: karaoke_render --timings timings.json --audio input.wav --output out.mp4 [--quality medium|high|ultra] [--font montserrat] [--color-active #000000] [--color-inactive #B4B9C3] [--color-bg #FFFFFF] [--inactive-opacity 0.65] [--audio-delay 0.0] [--plain-lines] [--no-scrolling]"
    );
    std::process::exit(2);
}

fn parse_args() -> Result<RenderConfig, String> {
    let mut args = std::env::args().skip(1);
    let mut timings = None;
    let mut audio = None;
    let mut output = None;
    let mut quality = "medium".to_string();
    let mut active = parse_color("#000000")?;
    let mut inactive = parse_color("#B4B9C3")?;
    let mut background = parse_color("#FFFFFF")?;
    let mut inactive_opacity = 0.65_f32;
    let mut audio_delay = 0.0;
    let mut font = "montserrat".to_string();
    let mut engine = "frames".to_string();
    let mut scrolling = true;
    let mut plain_lines = false;
    let mut ffmpeg = None;
    let mut ffprobe = None;
    let mut debug_frame_time = None;
    let mut debug_frame_output = None;

    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("Нет значения для {arg}"));
        match arg.as_str() {
            "--timings" => timings = Some(PathBuf::from(value()?)),
            "--audio" => audio = Some(PathBuf::from(value()?)),
            "--output" => output = Some(PathBuf::from(value()?)),
            "--quality" => quality = value()?,
            "--font" => font = value()?,
            "--color-active" => active = parse_color(&value()?)?,
            "--color-inactive" => inactive = parse_color(&value()?)?,
            "--color-bg" => background = parse_color(&value()?)?,
            "--inactive-opacity" => {
                inactive_opacity = value()?
                    .parse::<f32>()
                    .map_err(|e| format!("Некорректный inactive-opacity: {e}"))?
                    .clamp(0.2, 1.0)
            }
            "--engine" => engine = value()?,
            "--plain-lines" => plain_lines = true,
            "--no-scrolling" => scrolling = false,
            "--ffmpeg" => ffmpeg = Some(PathBuf::from(value()?)),
            "--ffprobe" => ffprobe = Some(PathBuf::from(value()?)),
            "--audio-delay" => {
                audio_delay = value()?
                    .parse::<f64>()
                    .map_err(|e| format!("Некорректный audio-delay: {e}"))?
            }
            "--debug-frame-time" => {
                debug_frame_time = Some(
                    value()?
                        .parse::<f64>()
                        .map_err(|e| format!("Некорректный debug-frame-time: {e}"))?,
                )
            }
            "--debug-frame-output" => debug_frame_output = Some(PathBuf::from(value()?)),
            "--help" | "-h" => usage(),
            _ => return Err(format!("Неизвестный аргумент: {arg}")),
        }
    }

    Ok(RenderConfig {
        timings: timings.ok_or_else(|| "Не указан --timings".to_string())?,
        audio: audio.ok_or_else(|| "Не указан --audio".to_string())?,
        output: output.ok_or_else(|| "Не указан --output".to_string())?,
        quality,
        active,
        inactive,
        background,
        inactive_opacity,
        audio_delay,
        font,
        engine,
        scrolling,
        plain_lines,
        ffmpeg,
        ffprobe,
        debug_frame_time,
        debug_frame_output,
    })
}

fn selected_montserrat_font(
    font: &str,
) -> Result<(&'static [u8], &'static str, &'static str), String> {
    match font.trim().to_lowercase().as_str() {
        "montserrat_black" | "montserrat black" | "montserrat-black" => {
            Ok((MONTSERRAT_BLACK, "Montserrat Black", "Montserrat Black"))
        }
        "montserrat" | "montserrat_bold" | "montserrat bold" | "montserrat-bold" => {
            Ok((MONTSERRAT_BOLD, "Montserrat", "Montserrat Bold"))
        }
        _ => Ok((MONTSERRAT_BOLD, "Montserrat", "Montserrat Bold")),
    }
}

fn default_ffmpeg_path(config: &RenderConfig) -> PathBuf {
    config
        .ffmpeg
        .clone()
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

fn default_ffprobe_path(config: &RenderConfig) -> PathBuf {
    if let Some(ffprobe) = &config.ffprobe {
        return ffprobe.clone();
    }
    if let Some(ffmpeg) = &config.ffmpeg
        && let Some(parent) = ffmpeg.parent()
    {
        let ffprobe_name = if cfg!(target_os = "windows") {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        return parent.join(ffprobe_name);
    }
    PathBuf::from("ffprobe")
}

fn audio_duration_seconds(audio: &PathBuf, ffprobe: &PathBuf) -> Result<f64, String> {
    let mut cmd = Command::new(ffprobe);
    hide_subprocess_window(&mut cmd);
    let output = cmd
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(audio)
        .output()
        .map_err(|e| format!("Не удалось запустить ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("ffprobe вернул некорректную длительность: {e}"))
}

fn text_width(font: &FontArc, size: f32, text: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(prev) = previous {
            width += scaled.kern(prev, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

fn safe_text_width(frame_width: u32, size_scale: f32) -> f32 {
    let side_margin = 64.0_f32 * size_scale;
    (frame_width as f32 - side_margin * 2.0).max(frame_width as f32 * 0.72)
}

fn paste_alpha(
    dst: &mut RgbaImage,
    src: &RgbaImage,
    x: i32,
    y: i32,
    opacity: f32,
    crop_w: Option<u32>,
) {
    let max_w = crop_w.unwrap_or(src.width()).min(src.width());
    for sy in 0..src.height() {
        let dy = y + sy as i32;
        if dy < 0 || dy >= dst.height() as i32 {
            continue;
        }
        for sx in 0..max_w {
            let dx = x + sx as i32;
            if dx < 0 || dx >= dst.width() as i32 {
                continue;
            }
            let s = src.get_pixel(sx, sy).0;
            let src_alpha = (s[3] as f32 / 255.0) * opacity;
            if src_alpha <= 0.0 {
                continue;
            }
            let d = dst.get_pixel_mut(dx as u32, dy as u32);
            let dst_alpha = d.0[3] as f32 / 255.0;
            let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
            if out_alpha <= 0.0 {
                continue;
            }
            let blend = |src: u8, dst: u8| {
                ((src as f32 * src_alpha + dst as f32 * dst_alpha * (1.0 - src_alpha)) / out_alpha)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            d.0[0] = blend(s[0], d.0[0]);
            d.0[1] = blend(s[1], d.0[1]);
            d.0[2] = blend(s[2], d.0[2]);
            d.0[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn paste_alpha_onto_opaque(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32, opacity: f32) {
    for sy in 0..src.height() {
        let dy = y + sy as i32;
        if dy < 0 || dy >= dst.height() as i32 {
            continue;
        }
        for sx in 0..src.width() {
            let dx = x + sx as i32;
            if dx < 0 || dx >= dst.width() as i32 {
                continue;
            }
            let s = src.get_pixel(sx, sy).0;
            let alpha = (s[3] as f32 / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }
            let d = dst.get_pixel_mut(dx as u32, dy as u32);
            let inv = 1.0 - alpha;
            d.0[0] = (s[0] as f32 * alpha + d.0[0] as f32 * inv).round() as u8;
            d.0[1] = (s[1] as f32 * alpha + d.0[1] as f32 * inv).round() as u8;
            d.0[2] = (s[2] as f32 * alpha + d.0[2] as f32 * inv).round() as u8;
            d.0[3] = 255;
        }
    }
}

// TODO: объединить параметры отрисовки в структуру Theme/RenderConfig при распиле рендерера.
#[allow(clippy::too_many_arguments)]
fn build_line_cache(
    lines: &[KaraokeLine],
    inactive_font: &FontArc,
    active_font: &FontArc,
    font_size: f32,
    line_height: u32,
    y_draw: i32,
    active: Rgba<u8>,
    inactive: Rgba<u8>,
    size_scale: f32,
    text_supersample: f32,
) -> Vec<LineCache> {
    let render_scale = size_scale * text_supersample;
    let render_font_size = font_size * text_supersample;
    let render_line_height = (line_height as f32 * text_supersample).round() as u32;
    let render_y_draw = (y_draw as f32 * text_supersample).round() as i32;
    let word_pad = (20.0 * render_scale).round() as u32;
    let word_active_offset = (10.0 * render_scale).round() as i32;
    let line_pad_x = (40.0 * render_scale).round() as u32;
    let line_text_x = (20.0 * render_scale).round() as i32;
    let inactive_space_w = text_width(inactive_font, render_font_size, " ");
    let active_space_w = text_width(active_font, render_font_size, " ");

    lines
        .iter()
        .map(|line| {
            let inactive_widths: Vec<f32> = line
                .words
                .iter()
                .map(|word| text_width(inactive_font, render_font_size, &word.word))
                .collect();
            let active_widths: Vec<f32> = line
                .words
                .iter()
                .map(|word| text_width(active_font, render_font_size, &word.word))
                .collect();
            let inactive_total_w = inactive_widths.iter().sum::<f32>()
                + inactive_space_w * line.words.len().saturating_sub(1) as f32;
            let active_total_w = active_widths.iter().sum::<f32>()
                + active_space_w * line.words.len().saturating_sub(1) as f32;
            let content_w = inactive_total_w.max(active_total_w);
            let line_w = (content_w.ceil() as u32 + line_pad_x).max(1);
            let base_line_w = (line_w as f32 / text_supersample).round().max(1.0) as u32;
            let mut inactive_img =
                ImageBuffer::from_pixel(line_w, render_line_height, Rgba([0, 0, 0, 0]));
            let mut active_inactive_img =
                ImageBuffer::from_pixel(line_w, render_line_height, Rgba([0, 0, 0, 0]));
            let mut active_plain_img =
                ImageBuffer::from_pixel(line_w, render_line_height, Rgba([0, 0, 0, 0]));

            let mut inactive_x =
                line_text_x + ((content_w - inactive_total_w) / 2.0).round() as i32;
            let mut active_x = line_text_x + ((content_w - active_total_w) / 2.0).round() as i32;
            let mut layers = Vec::with_capacity(line.words.len());
            for (idx, word) in line.words.iter().enumerate() {
                draw_text_mut(
                    &mut inactive_img,
                    inactive,
                    inactive_x,
                    render_y_draw,
                    PxScale::from(render_font_size),
                    inactive_font,
                    &word.word,
                );
                draw_text_mut(
                    &mut active_inactive_img,
                    inactive,
                    active_x,
                    render_y_draw,
                    PxScale::from(render_font_size),
                    active_font,
                    &word.word,
                );
                draw_text_mut(
                    &mut active_plain_img,
                    active,
                    active_x,
                    render_y_draw,
                    PxScale::from(render_font_size),
                    active_font,
                    &word.word,
                );

                let word_w = active_widths[idx].ceil() as u32;
                let mut active_img = ImageBuffer::from_pixel(
                    word_w + word_pad,
                    render_line_height,
                    Rgba([0, 0, 0, 0]),
                );
                draw_text_mut(
                    &mut active_img,
                    active,
                    word_active_offset,
                    render_y_draw,
                    PxScale::from(render_font_size),
                    active_font,
                    &word.word,
                );

                let base_active_w = (active_img.width() as f32 / text_supersample)
                    .round()
                    .max(1.0) as u32;
                let base_active_img = imageops::resize(
                    &active_img,
                    base_active_w,
                    line_height,
                    imageops::FilterType::Lanczos3,
                );
                layers.push(WordLayer {
                    start: word.start,
                    end: word.end,
                    paste_x: ((active_x - word_active_offset) as f32 / text_supersample).round()
                        as i32,
                    fill_width: ((word_active_offset.max(0) as f32 + word_w as f32)
                        / text_supersample)
                        .round()
                        .max(1.0) as u32,
                    image: base_active_img,
                });
                inactive_x += (inactive_widths[idx] + inactive_space_w).round() as i32;
                active_x += (active_widths[idx] + active_space_w).round() as i32;
            }

            let base_inactive_img = imageops::resize(
                &inactive_img,
                base_line_w,
                line_height,
                imageops::FilterType::Lanczos3,
            );
            let base_active_inactive_img = imageops::resize(
                &active_inactive_img,
                base_line_w,
                line_height,
                imageops::FilterType::Lanczos3,
            );
            let base_active_plain_img = imageops::resize(
                &active_plain_img,
                base_line_w,
                line_height,
                imageops::FilterType::Lanczos3,
            );

            LineCache {
                inactive: base_inactive_img,
                active_inactive: base_active_inactive_img,
                active_plain: base_active_plain_img,
                words: layers,
                width: base_line_w,
                height: line_height,
            }
        })
        .collect()
}

fn transition_times(lines: &[KaraokeLine], visual_lead: f64) -> Vec<f64> {
    lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                f64::NEG_INFINITY
            } else {
                let prev = &lines[idx - 1];
                if line.start > prev.end + 0.25 {
                    line.start + visual_lead
                } else {
                    line.start
                }
            }
        })
        .collect()
}

fn visual_lag_seconds() -> f64 {
    std::env::var("KARAOKE_VISUAL_LAG_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
        .clamp(0.0, 2.0)
}

fn visual_lead_seconds() -> f64 {
    std::env::var("KARAOKE_VISUAL_LEAD_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.35)
        .clamp(0.0, 1.0)
}

fn active_line(transitions: &[f64], t: f64) -> usize {
    transitions
        .partition_point(|value| *value <= t)
        .saturating_sub(1)
        .min(transitions.len().saturating_sub(1))
}

fn scroll_positions(
    transitions: &[f64],
    total_frames: usize,
    fps: f64,
    audio_delay: f64,
    line_spacing: f32,
) -> Vec<f32> {
    let mut positions = Vec::with_capacity(total_frames);
    let mut scroll_y = 0.0_f32;

    let transition_duration = 0.80_f64; // 0.80 seconds
    let transition_total_frames = (transition_duration * fps).round().max(1.0) as usize;

    let mut last_target_y = 0.0_f32;
    let mut transition_start_y = 0.0_f32;
    let mut transition_frame = transition_total_frames; // initially not in transition

    for frame_idx in 0..total_frames {
        let t = frame_idx as f64 / fps - audio_delay;
        let active_idx = if transitions.is_empty() {
            0
        } else {
            active_line(transitions, t)
        };
        let target_scroll_y = active_idx as f32 * line_spacing;

        if target_scroll_y != last_target_y {
            transition_start_y = scroll_y;
            transition_frame = 0;
            last_target_y = target_scroll_y;
        }

        if transition_frame < transition_total_frames {
            transition_frame += 1;
            let x = transition_frame as f32 / transition_total_frames as f32;
            let p = x * x * x * (x * (x * 6.0 - 15.0) + 10.0); // Perlin's smootherstep
            scroll_y = transition_start_y + (target_scroll_y - transition_start_y) * p;
        } else {
            scroll_y = target_scroll_y;
        }

        positions.push(scroll_y);
    }
    positions
}

struct AssWordMetric {
    start: f64,
    end: f64,
    paste_x: f32,
    width: f32,
}

struct AssLineMetric {
    text: String,
    width: f32,
    words: Vec<AssWordMetric>,
}

fn build_ass_metrics(lines: &[KaraokeLine], font: &FontArc, font_size: f32) -> Vec<AssLineMetric> {
    let space_w = text_width(font, font_size, " ");
    lines
        .iter()
        .map(|line| {
            let mut text = String::new();
            let mut words = Vec::with_capacity(line.words.len());
            let mut x = 0.0_f32;
            for (idx, word) in line.words.iter().enumerate() {
                if idx > 0 {
                    text.push(' ');
                    x += space_w;
                }
                text.push_str(&escape_ass_text(&word.word));
                let width = text_width(font, font_size, &word.word);
                words.push(AssWordMetric {
                    start: word.start,
                    end: word.end,
                    paste_x: x,
                    width,
                });
                x += width;
            }
            AssLineMetric {
                text,
                width: x.max(1.0),
                words,
            }
        })
        .collect()
}

fn ass_fill_width(metric: &AssLineMetric, t: f64) -> f32 {
    let mut fill = 0.0_f32;
    for word in &metric.words {
        if t < word.start {
            break;
        }
        if t >= word.end {
            fill = fill.max(word.paste_x + word.width);
        } else {
            let progress =
                ((t - word.start) / (word.end - word.start).max(0.001)).clamp(0.0, 1.0) as f32;
            fill = fill.max(word.paste_x + word.width * progress);
            break;
        }
    }
    fill
}

fn ass_line_highlight_is_live(metric: &AssLineMetric, t: f64) -> bool {
    metric
        .words
        .first()
        .zip(metric.words.last())
        .map(|(first, last)| t >= first.start && t <= last.end)
        .unwrap_or(false)
}

// TODO: объединить параметры в структуру конфига асс-рендера при распиле рендерера.
#[allow(clippy::too_many_arguments)]
fn write_ass_file(
    path: &Path,
    lines: &[KaraokeLine],
    transitions: &[f64],
    duration: f64,
    width: u32,
    height: u32,
    active: Rgba<u8>,
    inactive: Rgba<u8>,
    inactive_opacity: f32,
    display_delay: f64,
    highlight_delay: f64,
    event_fps: f64,
    size_scale: f32,
    scrolling: bool,
    plain_lines: bool,
    font: &str,
) -> Result<(), String> {
    let (font_bytes, ass_font_name, font_log_name) = selected_montserrat_font(font)?;
    let mut ass = String::new();
    ass.push_str("[Script Info]\n");
    ass.push_str("ScriptType: v4.00+\n");
    ass.push_str(&format!("PlayResX: {width}\nPlayResY: {height}\n"));
    ass.push_str("ScaledBorderAndShadow: yes\nWrapStyle: 2\n\n");
    ass.push_str("[V4+ Styles]\n");
    ass.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    ass.push_str(&format!(
        "Style: Dynamic,{},{:.0},{},{},{},{},1,0,0,0,100,100,0,0,1,0,0,5,0,0,0,1\n\n",
        ass_font_name,
        ASS_BASE_FONT_SIZE * size_scale,
        ass_color(active, 0),
        ass_color(active, 0),
        ass_color(active, 255),
        ass_color(active, 255)
    ));
    ass.push_str("[Events]\n");
    ass.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );

    let font = FontArc::try_from_slice(font_bytes)
        .map_err(|_| format!("Не удалось загрузить {font_log_name}"))?;
    let base_font_size = ASS_BASE_FONT_SIZE * size_scale;
    let min_font_size = base_font_size * (26.0 / 42.0);
    let line_spacing = 62.0_f32 * size_scale;
    let y_center = height as f32 / 2.0;
    let line_y_cutoff = 110.0_f32 * size_scale;
    let dist_cutoff = 95.0_f32 * size_scale;
    let safe_line_w = safe_text_width(width, size_scale);
    let metrics = build_ass_metrics(lines, &font, base_font_size);
    let total_frames = (duration * event_fps).ceil() as usize;
    let scrolls = scroll_positions(
        transitions,
        total_frames,
        event_fps,
        display_delay,
        line_spacing,
    );

    // ==================== EVENT COALESCING ====================
    // Instead of emitting one Dialogue per frame per line (~180K events),
    // we track per-line visual state and only emit a new event when properties
    // change by more than a sub-pixel threshold. This reduces events by 10-30x
    // while keeping pixel-identical output.

    // Sub-pixel thresholds — changes below these are invisible
    const POS_THRESH: f32 = 0.05; // position: ±0.05px
    const FS_THRESH: f32 = 0.05; // font size: ±0.05px
    const ALPHA_THRESH: u8 = 1; // alpha: ±1/255
    const CLIP_THRESH: f32 = 0.1; // clip boundary: ±0.1px

    #[derive(Clone)]
    struct LineEvent {
        start_frame: usize,
        y: f32,
        fs: f32,
        alpha: u8,
        is_active: bool,
        clip_left: f32,
        clip_right: f32,
    }

    let num_lines = metrics.len();
    let mut open_base: Vec<Option<LineEvent>> = vec![None; num_lines];
    let mut open_clip: Vec<Option<LineEvent>> = vec![None; num_lines];
    let x = width as f32 / 2.0;

    let flush_base = |ev: &LineEvent,
                      end_frame: usize,
                      ass: &mut String,
                      metrics: &[AssLineMetric],
                      idx: usize,
                      color: Rgba<u8>| {
        let start = ev.start_frame as f64 / event_fps;
        let end = (end_frame as f64 / event_fps).min(duration);
        if end <= start {
            return;
        }
        ass.push_str(&format!(
            "Dialogue: 0,{},{},Dynamic,,0,0,0,,{{\\pos({:.1},{:.1})\\fs{:.1}\\1c{}\\alpha&H{:02X}&}}{}\n",
            ass_time(start), ass_time(end), x, ev.y, ev.fs,
            ass_color(color, 0), ev.alpha, &metrics[idx].text
        ));
    };

    let flush_clip = |ev: &LineEvent,
                      end_frame: usize,
                      ass: &mut String,
                      metrics: &[AssLineMetric],
                      idx: usize,
                      height: u32| {
        let start = ev.start_frame as f64 / event_fps;
        let end = (end_frame as f64 / event_fps).min(duration);
        if end <= start {
            return;
        }
        ass.push_str(&format!(
            "Dialogue: 1,{},{},Dynamic,,0,0,0,,{{\\pos({:.1},{:.1})\\fs{:.1}\\1c{}\\alpha&H{:02X}&\\clip({:.0},0,{:.0},{height})}}{}\n",
            ass_time(start), ass_time(end), x, ev.y, ev.fs,
            ass_color(active, 0), ev.alpha, ev.clip_left.max(0.0), ev.clip_right,
            &metrics[idx].text
        ));
    };

    for (frame_idx, &scroll_y) in scrolls.iter().enumerate() {
        let display_t = frame_idx as f64 / event_fps - display_delay;
        let highlight_t = frame_idx as f64 / event_fps - highlight_delay;
        let active_idx = if transitions.is_empty() {
            0
        } else {
            active_line(transitions, display_t)
        };

        for (idx, metric) in metrics.iter().enumerate() {
            if !scrolling && idx != active_idx {
                if let Some(ev) = open_base[idx].take() {
                    let color = if plain_lines {
                        if ev.is_active { active } else { inactive }
                    } else {
                        inactive
                    };
                    flush_base(&ev, frame_idx, &mut ass, &metrics, idx, color);
                }
                if let Some(ev) = open_clip[idx].take() {
                    flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                }
                continue;
            }

            let line_y = if scrolling {
                y_center + idx as f32 * line_spacing - scroll_y
            } else {
                y_center
            };
            let visible = !scrolling
                || (line_y >= y_center - line_y_cutoff && line_y <= y_center + line_y_cutoff);

            if !visible {
                // Flush any open events for this line — it left the screen
                if let Some(ev) = open_base[idx].take() {
                    let color = if plain_lines {
                        if ev.is_active { active } else { inactive }
                    } else {
                        inactive
                    };
                    flush_base(&ev, frame_idx, &mut ass, &metrics, idx, color);
                }
                if let Some(ev) = open_clip[idx].take() {
                    flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                }
                continue;
            }

            let dist = (line_y - y_center).abs();
            let weight = if scrolling {
                (1.0 - dist / line_spacing).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let is_display_active = idx == active_idx;
            let is_highlight_live = !plain_lines && ass_line_highlight_is_live(metric, highlight_t);
            let is_active_for_event = if plain_lines {
                is_display_active
            } else {
                is_highlight_live
            };

            let target_font_size = min_font_size + (base_font_size - min_font_size) * weight;
            let fit_font_size = base_font_size * (safe_line_w / metric.width).min(1.0);
            let font_size = target_font_size.min(fit_font_size);
            let scale = font_size / base_font_size;
            let mut opacity = if scrolling {
                (1.0 - dist / dist_cutoff).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if !is_active_for_event {
                opacity *= inactive_opacity;
            }
            if opacity <= 0.01 {
                if let Some(ev) = open_base[idx].take() {
                    let color = if plain_lines {
                        if ev.is_active { active } else { inactive }
                    } else {
                        inactive
                    };
                    flush_base(&ev, frame_idx, &mut ass, &metrics, idx, color);
                }
                if let Some(ev) = open_clip[idx].take() {
                    flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                }
                continue;
            }
            let alpha = ((1.0 - opacity) * 255.0).round().clamp(0.0, 255.0) as u8;

            // --- Base event (inactive text) ---
            let need_new_base = match &open_base[idx] {
                None => true,
                Some(prev) => {
                    (line_y - prev.y).abs() >= POS_THRESH
                        || (font_size - prev.fs).abs() >= FS_THRESH
                        || alpha.abs_diff(prev.alpha) >= ALPHA_THRESH
                        || is_active_for_event != prev.is_active
                }
            };
            if need_new_base {
                if let Some(ev) = open_base[idx].take() {
                    let color = if plain_lines {
                        if ev.is_active { active } else { inactive }
                    } else {
                        inactive
                    };
                    flush_base(&ev, frame_idx, &mut ass, &metrics, idx, color);
                }
                open_base[idx] = Some(LineEvent {
                    start_frame: frame_idx,
                    y: line_y,
                    fs: font_size,
                    alpha,
                    is_active: is_active_for_event,
                    clip_left: 0.0,
                    clip_right: 0.0,
                });
            }

            // --- Clip event (active word fill) ---
            if is_highlight_live {
                let fill = ass_fill_width(metric, highlight_t) * scale;
                if fill > 0.0 {
                    let left = x - metric.width * scale / 2.0;
                    let clip_left = (left - 1.0 * size_scale).max(0.0);
                    let clip_right = (left + fill).clamp(0.0, width as f32);

                    let need_new_clip = match &open_clip[idx] {
                        None => true,
                        Some(prev) => {
                            (line_y - prev.y).abs() >= POS_THRESH
                                || (font_size - prev.fs).abs() >= FS_THRESH
                                || alpha.abs_diff(prev.alpha) >= ALPHA_THRESH
                                || (clip_right - prev.clip_right).abs() >= CLIP_THRESH
                                || (clip_left - prev.clip_left).abs() >= CLIP_THRESH
                        }
                    };
                    if need_new_clip {
                        if let Some(ev) = open_clip[idx].take() {
                            flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                        }
                        open_clip[idx] = Some(LineEvent {
                            start_frame: frame_idx,
                            y: line_y,
                            fs: font_size,
                            alpha,
                            is_active: true,
                            clip_left,
                            clip_right,
                        });
                    }
                } else {
                    // fill == 0, no clip event needed
                    if let Some(ev) = open_clip[idx].take() {
                        flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                    }
                }
            } else {
                // Not active anymore — flush any open clip event
                if let Some(ev) = open_clip[idx].take() {
                    flush_clip(&ev, frame_idx, &mut ass, &metrics, idx, height);
                }
            }
        }
    }

    // Flush all remaining open events at end of video
    for idx in 0..num_lines {
        if let Some(ev) = open_base[idx].take() {
            let color = if plain_lines { active } else { inactive };
            flush_base(&ev, total_frames, &mut ass, &metrics, idx, color);
        }
        if let Some(ev) = open_clip[idx].take() {
            flush_clip(&ev, total_frames, &mut ass, &metrics, idx, height);
        }
    }

    std::fs::write(path, ass).map_err(|e| format!("Не удалось записать ASS: {e}"))
}

// TODO: объединить width/height/crf/preset/fps/scrolling/plain_lines в RenderConfig при распиле рендерера.
#[allow(clippy::too_many_arguments)]
fn render_ass(
    config: &RenderConfig,
    lines: &[KaraokeLine],
    transitions: &[f64],
    duration: f64,
    width: u32,
    height: u32,
    crf: &str,
    preset: &str,
    size_scale: f32,
    fps: f64,
    scrolling: bool,
    plain_lines: bool,
) -> Result<(), String> {
    let ass_path = config.output.with_extension("ass");
    let event_fps = 100.0_f64;
    let display_delay = config.audio_delay - visual_lead_seconds();
    let highlight_delay = config.audio_delay + visual_lag_seconds();
    write_ass_file(
        &ass_path,
        lines,
        transitions,
        duration,
        width,
        height,
        config.active,
        config.inactive,
        config.inactive_opacity,
        display_delay,
        highlight_delay,
        event_fps,
        size_scale,
        scrolling,
        plain_lines,
        &config.font,
    )?;

    let ffmpeg = default_ffmpeg_path(config);
    let font_dir = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent().and_then(|parent| {
                [
                    parent.join("../assets"),
                    parent.join("assets"),
                    parent.join("../../assets"),
                ]
                .into_iter()
                .find(|candidate| candidate.exists())
            })
        })
        .or_else(|| {
            [
                PathBuf::from("assets"),
                PathBuf::from("desktop_app/assets"),
                PathBuf::from("../assets"),
            ]
            .into_iter()
            .find(|candidate| candidate.exists())
        })
        .unwrap_or_else(|| PathBuf::from("assets"));
    let ass_filter = format!(
        "ass=filename='{}':fontsdir='{}'",
        escape_filter_path(&ass_path),
        escape_filter_path(&font_dir)
    );

    let mut cmd = Command::new(ffmpeg);
    hide_subprocess_window(&mut cmd);
    let status = cmd
        .arg("-y")
        .arg("-nostats")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg(format!(
            "color=c=0x{:02X}{:02X}{:02X}:s={}x{}:r={:.0}:d={:.3}",
            config.background.0[0],
            config.background.0[1],
            config.background.0[2],
            width,
            height,
            fps,
            duration
        ))
        .arg("-i")
        .arg(&config.audio)
        .arg("-vf")
        .arg(ass_filter)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-preset")
        .arg(preset)
        .arg("-crf")
        .arg(crf)
        .arg("-bf")
        .arg("0")
        .arg("-vsync")
        .arg("cfr")
        .arg("-avoid_negative_ts")
        .arg("make_zero")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("160k")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg(&config.output)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("Не удалось запустить ffmpeg ASS-render: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("ffmpeg ASS-render завершился с ошибкой".to_string())
    }
}

fn ffmpeg_supports_ass_filter(ffmpeg: &Path) -> bool {
    let output = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-filters")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let filters = String::from_utf8_lossy(&output.stdout);
    filters.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let _flags = parts.next();
        matches!(parts.next(), Some("ass" | "subtitles"))
    })
}

/// Проверяет, доступен ли аппаратный энкодер h264_videotoolbox (macOS).
/// Используется для ускорения энкода в ~3-5× относительно libx264 на CPU.
fn ffmpeg_supports_h264_videotoolbox(ffmpeg: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    // Проверяем не только наличие энкодера, но и реальную работоспособность.
    // В ffmpeg 8.x h264_videotoolbox может быть в списке, но не работать
    // с quality-based (-q:v) режимом — используем bitrate-based (-b:v).
    let test = Command::new(ffmpeg)
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x240:d=0.04:r=30",
            "-c:v",
            "h264_videotoolbox",
            "-b:v",
            "1M",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    test.is_ok_and(|o| o.status.success())
}

fn tail_text(value: &str, max_chars: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(max_chars).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn output_file_was_written(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.len() > 1024)
        .unwrap_or(false)
}

fn render(config: RenderConfig) -> Result<(), String> {
    let timings = std::fs::read_to_string(&config.timings)
        .map_err(|e| format!("Не удалось прочитать timings: {e}"))?;
    let lines: Vec<KaraokeLine> =
        serde_json::from_str(&timings).map_err(|e| format!("Некорректный JSON timings: {e}"))?;
    let ffprobe = default_ffprobe_path(&config);
    let duration = audio_duration_seconds(&config.audio, &ffprobe)?;

    let (size_scale, crf, preset) = match config.quality.as_str() {
        "high" => (1.0_f32, "19", "medium"),
        "ultra" => (2.0_f32, "16", "slow"),
        _ => (1.0_f32, "23", "fast"),
    };

    let width = (1352.0 * size_scale).round() as u32;
    let height = (224.0 * size_scale).round() as u32;
    let line_spacing = 62.0 * size_scale;
    let font_size_max = 42.0 * size_scale;
    let font_size_min = 26.0 * size_scale;
    let y_center = height as f32 / 2.0;
    let y_text_center = 31.0 * size_scale;
    let line_y_cutoff = 110.0 * size_scale;
    let dist_cutoff = 95.0 * size_scale;
    let safe_line_w = safe_text_width(width, size_scale);
    let line_h = (75.0 * size_scale).round() as u32;
    let y_draw = (8.0 * size_scale).round() as i32;
    let ffmpeg = default_ffmpeg_path(&config);
    let use_ass_renderer = config.engine == "ass" && ffmpeg_supports_ass_filter(&ffmpeg);
    let fps = if use_ass_renderer { 60.0_f64 } else { 30.0_f64 };
    let total_frames = ((duration * fps).ceil() as usize).max(1);
    let visual_lead = visual_lead_seconds();
    let display_delay = config.audio_delay - visual_lead;
    let highlight_delay = config.audio_delay + visual_lag_seconds();
    let transitions = transition_times(&lines, visual_lead);

    if use_ass_renderer {
        return render_ass(
            &config,
            &lines,
            &transitions,
            duration,
            width,
            height,
            crf,
            preset,
            size_scale,
            fps,
            config.scrolling,
            config.plain_lines,
        );
    } else if config.engine == "ass" {
        eprintln!("[Rust] ffmpeg ASS/subtitles filter not found; falling back to frame renderer");
    }

    let inactive_font = FontArc::try_from_slice(MONTSERRAT_BOLD)
        .map_err(|_| "Не удалось загрузить Montserrat Bold".to_string())?;
    let active_font = FontArc::try_from_slice(MONTSERRAT_BLACK)
        .map_err(|_| "Не удалось загрузить Montserrat Black".to_string())?;
    let text_supersample = 3.0_f32;
    // Pillow/FreeType and ab_glyph expose slightly different perceived pixel sizes.
    // This compensation keeps Montserrat visually aligned with the legacy renderer.
    let render_font_size_max = font_size_max * 1.18;
    let cache = build_line_cache(
        &lines,
        &inactive_font,
        &active_font,
        render_font_size_max,
        line_h,
        y_draw,
        config.active,
        config.inactive,
        size_scale,
        text_supersample,
    );
    let scrolls = scroll_positions(
        &transitions,
        total_frames.max(
            config
                .debug_frame_time
                .map(|time| (time * fps).round().max(0.0) as usize + 1)
                .unwrap_or(0),
        ),
        fps,
        display_delay,
        line_spacing,
    );

    // Кэш квантованных ресайзов: ключ (line_idx, variant, new_w, new_h), значение — RgbaImage.
    // Используем RefCell, т.к. замыкание захватывает по ссылке, а кадры рендерятся последовательно.
    // Кэшируем только статичные варианты (inactive / active_plain), highlight меняется каждый кадр.
    let resize_cache: RefCell<HashMap<(usize, u8, u32, u32), RgbaImage>> =
        RefCell::new(HashMap::new());
    // Шаг квантования: 2px. Визуально неотличимо, но резко повышает hit-rate при мелких
    // колебаниях scale в scrolling-режиме.
    const QUANTUM: u32 = 2;
    fn quantize(v: u32, q: u32) -> u32 {
        (v + q / 2) / q * q
    }

    let render_frame = |frame_idx: usize, scroll_y: f32| -> RgbaImage {
        let display_t = frame_idx as f64 / fps - display_delay;
        let highlight_t = frame_idx as f64 / fps - highlight_delay;
        let mut frame = ImageBuffer::from_pixel(width, height, config.background);
        let active_idx = if transitions.is_empty() {
            0
        } else {
            active_line(&transitions, display_t)
        };

        for (idx, line_cache) in cache.iter().enumerate() {
            if !config.scrolling && idx != active_idx {
                continue;
            }
            let line_y = if config.scrolling {
                y_center + idx as f32 * line_spacing - scroll_y
            } else {
                y_center
            };
            if config.scrolling
                && (line_y < y_center - line_y_cutoff || line_y > y_center + line_y_cutoff)
            {
                continue;
            }
            let dist = (line_y - y_center).abs();
            let weight = if config.scrolling {
                (1.0 - dist / line_spacing).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let center_strength = weight * weight * (3.0 - 2.0 * weight);
            let is_highlight_live =
                !config.plain_lines && line_highlight_is_live(&line_cache.words, highlight_t);

            let (target_img, target_variant) = if config.plain_lines {
                (line_cache.active_plain.clone(), 2_u8)
            } else if is_highlight_live {
                let mut img = line_cache.active_inactive.clone();
                paint_line_highlight(&mut img, &line_cache.words, highlight_t);
                (img, 3_u8)
            } else {
                (line_cache.active_inactive.clone(), 1_u8)
            };
            let (mut line_img, cache_variant) = if center_strength <= 0.001 {
                (line_cache.inactive.clone(), 0_u8)
            } else if center_strength >= 0.999 {
                (target_img, target_variant)
            } else {
                (
                    blend_images(&line_cache.inactive, &target_img, center_strength),
                    4_u8,
                )
            };
            let is_cacheable = cache_variant != 3 && cache_variant != 4;

            let target_scale =
                (font_size_min + (font_size_max - font_size_min) * weight) / font_size_max;
            let fit_scale = (safe_line_w / line_cache.width as f32).min(1.0);
            let scale = target_scale.min(fit_scale);
            let raw_w = ((line_cache.width as f32 * scale).round() as u32).max(1);
            let raw_h = ((line_cache.height as f32 * scale).round() as u32).max(1);

            if is_cacheable {
                let qw = quantize(raw_w, QUANTUM).max(1);
                let qh = quantize(raw_h, QUANTUM).max(1);
                let key = (idx, cache_variant, qw, qh);
                let cache = resize_cache.borrow();
                if let Some(cached) = cache.get(&key) {
                    line_img = cached.clone();
                } else {
                    drop(cache);
                    let resized =
                        imageops::resize(&line_img, qw, qh, imageops::FilterType::Triangle);
                    resize_cache.borrow_mut().insert(key, resized.clone());
                    line_img = resized;
                }
            } else {
                line_img =
                    imageops::resize(&line_img, raw_w, raw_h, imageops::FilterType::Triangle);
            }

            let mut opacity = if config.scrolling {
                (1.0 - dist / dist_cutoff).clamp(0.0, 1.0)
            } else {
                1.0
            };
            opacity *= config.inactive_opacity + (1.0 - config.inactive_opacity) * center_strength;

            let x = width as i32 / 2 - raw_w as i32 / 2;
            let y_center_resized = y_text_center * scale;
            let y = (line_y - y_center_resized).floor() as i32;
            paste_alpha_onto_opaque(&mut frame, &line_img, x, y, opacity);
        }

        frame
    };

    if let Some(debug_time) = config.debug_frame_time {
        let output = config.debug_frame_output.ok_or_else(|| {
            "--debug-frame-output обязателен вместе с --debug-frame-time".to_string()
        })?;
        let frame_idx = (debug_time * fps).round().max(0.0) as usize;
        let frame = render_frame(frame_idx, scrolls[frame_idx]);
        frame
            .save(&output)
            .map_err(|e| format!("Не удалось записать PNG: {e}"))?;
        return Ok(());
    }

    let hardware_encoder_enabled = std::env::var("KARAOKE_USE_HARDWARE_ENCODER")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "no" | "NO"))
        .unwrap_or(true);
    let use_hardware_encoder =
        hardware_encoder_enabled && ffmpeg_supports_h264_videotoolbox(&ffmpeg);
    // h264_videotoolbox в ffmpeg 8.x не поддерживает quality-based (-q:v).
    // Для нашего узкого текстового видео высокий битрейт только раздувает файл,
    // поэтому держим аппаратный энкодер быстрым, но ограничиваем средний поток.
    let (hw_bitrate, hw_bufsize) = match config.quality.as_str() {
        "ultra" => ("3M", "6M"),
        "high" => ("1500k", "3M"),
        _ => ("900k", "1800k"),
    };

    let mut ffmpeg_cmd = Command::new(ffmpeg);
    hide_subprocess_window(&mut ffmpeg_cmd);
    let child = ffmpeg_cmd
        .arg("-y")
        .arg("-f")
        .arg("rawvideo")
        // Подаём кадры напрямую в RGBA (нативный формат RgbaImage), без конверсии в rgb24.
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-s")
        .arg(format!("{width}x{height}"))
        .arg("-r")
        .arg("30")
        .arg("-i")
        .arg("-")
        .arg("-i")
        .arg(&config.audio)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c:v")
        .arg(if use_hardware_encoder {
            "h264_videotoolbox"
        } else {
            "libx264"
        })
        .arg("-pix_fmt")
        .arg("yuv420p");
    if use_hardware_encoder {
        child
            .arg("-b:v")
            .arg(hw_bitrate)
            .arg("-maxrate")
            .arg(hw_bitrate)
            .arg("-bufsize")
            .arg(hw_bufsize);
    } else {
        child
            .arg("-preset")
            .arg(preset)
            .arg("-crf")
            .arg(crf)
            .arg("-bf")
            .arg("0");
    }
    let mut child = child
        .arg("-vsync")
        .arg("cfr")
        .arg("-avoid_negative_ts")
        .arg("make_zero")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("160k")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg(&config.output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Не удалось запустить ffmpeg: {e}"))?;
    if use_hardware_encoder {
        eprintln!("[Rust] Энкодер: h264_videotoolbox (аппаратный, b:v={hw_bitrate})");
    } else {
        eprintln!("[Rust] Энкодер: libx264 (CPU, preset={preset}, crf={crf})");
    }

    let mut stderr = child.stderr.take();
    let stderr_handle = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut stderr) = stderr.take() {
            let _ = stderr.read_to_end(&mut buffer);
        }
        String::from_utf8_lossy(&buffer).to_string()
    });

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Не удалось открыть stdin ffmpeg".to_string())?;
    let mut last_active_idx = None;
    let mut cached_frame: Option<RgbaImage> = None;

    for (frame_idx, &scroll_y) in scrolls.iter().enumerate().take(total_frames) {
        let display_t = frame_idx as f64 / fps - display_delay;
        let active_idx = if transitions.is_empty() {
            0
        } else {
            active_line(&transitions, display_t)
        };

        let can_use_cache = config.plain_lines && !config.scrolling;
        let frame = if can_use_cache && last_active_idx == Some(active_idx) {
            if let Some(ref cached) = cached_frame {
                cached.clone()
            } else {
                let rendered = render_frame(frame_idx, scroll_y);
                cached_frame = Some(rendered.clone());
                last_active_idx = Some(active_idx);
                rendered
            }
        } else {
            let rendered = render_frame(frame_idx, scroll_y);
            if can_use_cache {
                cached_frame = Some(rendered.clone());
                last_active_idx = Some(active_idx);
            }
            rendered
        };

        // Подаём сырые RGBA-байты кадра напрямую в ffmpeg (pix_fmt rgba),
        // минуя конверсию в rgb24.
        if let Err(err) = stdin.write_all(frame.as_raw()) {
            drop(stdin);
            let _ = child.wait();
            let stderr_text = stderr_handle.join().unwrap_or_default();
            let late_in_render = frame_idx + 5 >= total_frames;
            if late_in_render && output_file_was_written(&config.output) {
                eprintln!("[Rust] ffmpeg closed pipe at the end, keeping written output: {err}");
                return Ok(());
            }
            let stderr_tail = tail_text(&stderr_text, 1600);
            return Err(format!(
                "ffmpeg pipe write failed: {err}. ffmpeg stderr: {}",
                stderr_tail.trim()
            ));
        }

        if frame_idx % 30 == 0 {
            eprintln!(
                "render {:.1}%",
                frame_idx as f64 / total_frames.max(1) as f64 * 100.0
            );
        }
    }

    drop(stdin);
    let status = child
        .wait()
        .map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    if !status.success() {
        if output_file_was_written(&config.output) {
            eprintln!(
                "[Rust] ffmpeg exited with a non-zero status after writing output; keeping file"
            );
            return Ok(());
        }
        let stderr_tail = tail_text(&stderr_text, 1600);
        return Err(format!(
            "ffmpeg не смог собрать mp4: {}",
            stderr_tail.trim()
        ));
    }
    Ok(())
}

fn main() {
    let result = parse_args().and_then(render);
    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
