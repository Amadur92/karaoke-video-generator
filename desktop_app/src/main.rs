#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Скрывает консоль на Windows в релиз-сборке

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
        base.join(&worker_exe),
        base.join("worker").join(&worker_exe),
        base.join("../Resources/worker").join(&worker_exe),
        base.join("karaoke_worker.py"),
        base.join("worker/karaoke_worker.py"),
        base.join("../../../worker/karaoke_worker.py"),
        PathBuf::from("/Users/mihailsokolenko/wow_quiz/worker/karaoke_worker.py"),
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

fn format_time_ms(ms: i64) -> String {
    let total_seconds = (ms.max(0) as f32 / 1000.0).round() as i64;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}", minutes, seconds)
}

fn probe_audio_duration_ms(path: &str) -> Result<i64, String> {
    let output = std::process::Command::new(tool_path("ffprobe"))
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
    output: &Path,
) -> Result<(), String> {
    let duration_ms = end_ms - start_ms;
    if duration_ms < 1000 {
        return Err("Оставьте хотя бы 1 секунду аудио после обрезки.".to_string());
    }

    let status = std::process::Command::new(tool_path("ffmpeg"))
        .arg("-y")
        .arg("-ss")
        .arg(format!("{:.3}", start_ms as f64 / 1000.0))
        .arg("-t")
        .arg(format!("{:.3}", duration_ms as f64 / 1000.0))
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
        Err("ffmpeg не смог создать обрезанный аудиофайл.".to_string())
    }
}

fn probe_video_size(path: &str) -> Result<(usize, usize), String> {
    let output = std::process::Command::new(tool_path("ffprobe"))
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

fn render_video_preview_audio(input: &str, output: &Path) -> Result<(), String> {
    let status = std::process::Command::new(tool_path("ffmpeg"))
        .arg("-y")
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
    let exports = app_data_dir().join("exports");
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct AppSettings {
    model: String,
    quality: String,
    font: String,
    color_active: [u8; 3],
    color_inactive: [u8; 3],
    color_bg: [u8; 3],
    audio_delay_ms: i32,
    artist: String,
    title: String,
    lyrics: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model: "small".to_string(),
            quality: "medium".to_string(),
            font: "montserrat".to_string(),
            color_active: [0, 0, 0],
            color_inactive: [180, 185, 195],
            color_bg: [255, 255, 255],
            audio_delay_ms: 0,
            artist: String::new(),
            title: String::new(),
            lyrics: String::new(),
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
}

enum ProgressUpdate {
    Progress(CLIProgress),
    RawLog(String),
    Error(String),
    Finished(bool),
}

struct VideoFrame {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

struct KaraokeApp {
    audio_path: Option<String>,
    audio_duration_ms: Option<i64>,
    trim_start_ms: i64,
    trim_end_ms: i64,
    trim_playhead_ms: i64,
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

    // Сдвиг аудио (мс)
    audio_delay_ms: i32,

    // Статус выполнения
    is_generating: bool,
    progress: f32,
    status_text: String,
    log_output: String,

    // Канал получения прогресса из фонового потока
    rx: Option<Receiver<ProgressUpdate>>,

    // Путь к сгенерированному видео-файлу
    generated_file: Option<String>,
    video_rx: Option<Receiver<VideoFrame>>,
    video_stop: Option<Arc<AtomicBool>>,
    video_texture: Option<egui::TextureHandle>,
    video_status: String,
    video_stream: Option<rodio::OutputStream>,
    video_sink: Option<rodio::Sink>,
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
            audio_delay_ms: settings.audio_delay_ms,
            is_generating: false,
            progress: 0.0,
            status_text: "Готов к работе".to_string(),
            log_output: String::new(),
            rx: None,
            generated_file: None,
            video_rx: None,
            video_stop: None,
            video_texture: None,
            video_status: String::new(),
            video_stream: None,
            video_sink: None,
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

        sink.append(source);
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
        self.video_stop.is_some()
            || self
                .video_sink
                .as_ref()
                .map(|sink| !sink.empty())
                .unwrap_or(false)
    }

    fn stop_video_preview(&mut self) {
        if let Some(stop) = self.video_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(sink) = self.video_sink.take() {
            sink.stop();
        }
        self.video_stream = None;
        self.video_rx = None;
        self.video_status = "Предпросмотр видео остановлен.".to_string();
    }

    fn start_video_preview(&mut self, path: &str, ctx: &egui::Context) -> Result<(), String> {
        self.stop_video_preview();
        self.video_status = "Готовим встроенный предпросмотр...".to_string();

        let (width, height) = preview_video_size(path)?;
        let audio_path = temp_dir().join("karaoke_video_preview.wav");
        render_video_preview_audio(path, &audio_path)?;

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
            let mut child = match std::process::Command::new(tool_path("ffmpeg"))
                .arg("-v")
                .arg("error")
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
        self.video_status = "Видео воспроизводится внутри приложения.".to_string();
        Ok(())
    }

    fn sync_video_preview(&mut self, ctx: &egui::Context) {
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
            if !sink.empty() {
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
            self.video_status = "Предпросмотр видео завершен.".to_string();
        }
    }

    fn set_audio_file(&mut self, path: PathBuf) {
        self.stop_preview();
        let path_str = path.to_string_lossy().to_string();
        self.audio_path = Some(path_str.clone());
        self.generated_file = None;

        match probe_audio_duration_ms(&path_str) {
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
        match render_trimmed_audio(&audio_path, play_start, end, &preview_path)
            .and_then(|_| self.play_audio_preview(&preview_path, play_start, end))
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

        let desired_size = egui::vec2(ui.available_width(), 64.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());
        let track_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.center().y - 3.0),
            egui::pos2(rect.right() - 8.0, rect.center().y + 5.0),
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

                let target_ms = from_x(pointer.x);
                let nearest = if pointer.y <= track_rect.center().y {
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
                    _ => {
                        self.trim_playhead_ms =
                            target_ms.clamp(self.trim_start_ms, self.trim_end_ms)
                    }
                }
                self.normalize_trim_state();
            }
        }

        let painter = ui.painter();
        painter.rect_filled(track_rect, 4.0, egui::Color32::from_rgb(34, 40, 52));

        let selected_rect = egui::Rect::from_min_max(
            egui::pos2(to_x(self.trim_start_ms), track_rect.top()),
            egui::pos2(to_x(self.trim_end_ms), track_rect.bottom()),
        );
        painter.rect_filled(selected_rect, 4.0, egui::Color32::from_rgb(37, 78, 125));

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

        let draw_pin = |x: f32, color: egui::Color32, label: &str, above: bool| {
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
        };

        draw_pin(start_x, success, "S", true);
        draw_pin(end_x, success, "E", true);
        draw_pin(play_x, accent, "▶", false);

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

        self.is_generating = true;
        self.progress = 0.0;
        self.status_text = "Запуск CLI-генерации...".to_string();
        self.log_output = "🚀 Инициализация фонового процесса...\n".to_string();
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
        let audio_delay_seconds = self.audio_delay_ms as f32 / 1000.0;
        let trim_bounds = self.clamped_trim_bounds();
        let temp = temp_dir();
        let exports = exports_dir();
        let uploads = upload_dir();
        let worker_path_for_log = worker_path.to_string_lossy().to_string();

        let (tx, rx) = channel::<ProgressUpdate>();
        self.rx = Some(rx);

        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&temp);
            let temp_lyrics_path = temp.join("temp_lyrics.txt");
            if let Err(e) = std::fs::write(&temp_lyrics_path, &lyrics) {
                let _ = tx.send(ProgressUpdate::Error(format!(
                    "Не удалось записать временный файл текста: {}",
                    e
                )));
                return;
            }

            let active_hex = format!(
                "#{:02X}{:02X}{:02X}",
                active_color[0], active_color[1], active_color[2]
            );
            let inactive_hex = format!(
                "#{:02X}{:02X}{:02X}",
                inactive_color[0], inactive_color[1], inactive_color[2]
            );
            let bg_hex = format!("#{:02X}{:02X}{:02X}", bg_color[0], bg_color[1], bg_color[2]);

            let _ = tx.send(ProgressUpdate::RawLog(format!(
                "📝 Временные файлы подготовлены. Запуск worker: {}",
                worker_path_for_log
            )));
            ctx.request_repaint();

            let worker_audio_path = if let Some((start, end)) = trim_bounds {
                let should_trim = start > 0
                    || probe_audio_duration_ms(&audio_path)
                        .map(|duration| end < duration - 250)
                        .unwrap_or(false);

                if should_trim {
                    let trimmed_path = temp.join("karaoke_trimmed_generation.wav");
                    let _ = tx.send(ProgressUpdate::RawLog(format!(
                        "✂️ Обрезка аудио: {} - {}",
                        format_time_ms(start),
                        format_time_ms(end)
                    )));
                    ctx.request_repaint();

                    match render_trimmed_audio(&audio_path, start, end, &trimmed_path) {
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

            let mut cmd = if is_python_worker(&worker_path) {
                let mut cmd = std::process::Command::new("python3");
                cmd.arg(&worker_path);
                cmd
            } else {
                std::process::Command::new(&worker_path)
            };

            if let Some(bin_dir) = bundled_bin_dir() {
                let old_path = std::env::var_os("PATH").unwrap_or_default();
                let mut paths = vec![bin_dir];
                paths.extend(std::env::split_paths(&old_path));
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("PATH", joined);
                }
            }

            cmd.env("KARAOKE_EXPORT_DIR", &exports)
                .env("KARAOKE_UPLOAD_DIR", &uploads)
                .env("PYTHONUTF8", "1")
                .arg("--cli")
                .arg("--audio")
                .arg(&worker_audio_path)
                .arg("--artist")
                .arg(if artist.is_empty() {
                    "Исполнитель"
                } else {
                    &artist
                })
                .arg("--title")
                .arg(if title.is_empty() {
                    "Песня"
                } else {
                    &title
                })
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

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
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
                let exports_str = exports.to_string_lossy().to_string();
                for line in reader.lines() {
                    if let Ok(line_str) = line {
                        let trimmed = line_str.trim();
                        if trimmed.starts_with('{') && trimmed.ends_with('}') {
                            if let Ok(mut update) = serde_json::from_str::<CLIProgress>(trimmed) {
                                // Конвертируем относительное имя файла в полный путь
                                if let Some(ref filename) = update.file {
                                    let full = format!("{}/{}", exports_str, filename);
                                    update.file = Some(full);
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
            let success = status.map(|s| s.success()).unwrap_or(false);

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
            audio_delay_ms: self.audio_delay_ms,
            artist: self.artist.clone(),
            title: self.title.clone(),
            lyrics: self.lyrics.clone(),
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
                    if let Some(err) = prog.error {
                        self.log_output
                            .push_str(&format!("❌ Ошибка ИИ: {}\n", err));
                    }
                    if let Some(full_path) = prog.file {
                        self.log_output
                            .push_str(&format!("🎉 Успешно сохранено: {}\n", full_path));
                        self.stop_video_preview();
                        self.video_texture = None;
                        self.video_status = String::new();
                        self.generated_file = Some(full_path);
                    }
                }
                ProgressUpdate::RawLog(log) => {
                    self.log_output.push_str(&format!("{}\n", log));
                }
                ProgressUpdate::Error(err) => {
                    self.is_generating = false;
                    self.status_text = "Ошибка".to_string();
                    self.log_output.push_str(&format!("❌ Ошибка: {}\n", err));
                }
                ProgressUpdate::Finished(success) => {
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
            }
        }

        // Поддержка Drag-and-Drop аудиофайла прямо на окно приложения
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                if let Some(file) = i.raw.dropped_files.first() {
                    if let Some(path) = &file.path {
                        let path_str = path.to_string_lossy().to_string();
                        if path_str.ends_with(".mp3") {
                            self.set_audio_file(path.clone());
                        }
                    }
                }
            }
        });

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
                        egui::RichText::new("Караоке-Видео Генератор")
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
                        egui::RichText::new(status)
                            .strong()
                            .size(13.0)
                            .color(status_color)
                    );
                });
            });
            ui.add_space(18.0);

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
                                                    self.set_audio_file(path);
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

                                            let selected_ms = self.trim_end_ms - self.trim_start_ms;
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{} - {} · фрагмент {} · слушаем с {}",
                                                        format_time_ms(self.trim_start_ms),
                                                        format_time_ms(self.trim_end_ms),
                                                        format_time_ms(selected_ms),
                                                        format_time_ms(self.trim_playhead_ms)
                                                    ))
                                                    .size(12.0)
                                                    .color(muted),
                                                );

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
                                            ui.label(egui::RichText::new("Каждая строка помогает алгоритму точнее собрать фразы").size(12.0).color(muted));
                                        });
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            let lines = self.lyrics.lines().filter(|line| !line.trim().is_empty()).count();
                                            ui.label(egui::RichText::new(format!("{} строк", lines)).size(12.0).color(muted));
                                        });
                                    });
                                    ui.add_space(8.0);

                                    ui.add(egui::TextEdit::multiline(&mut self.lyrics)
                                        .hint_text("Вставьте текст песни построчно...")
                                        .desired_width(ui.available_width())
                                        .desired_rows(16)
                                        .font(egui::TextStyle::Monospace));
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

                                            ui.add_space(10.0);

                                            let is_video_playing = self.is_video_playing();
                                            let play_btn = egui::Button::new(
                                                egui::RichText::new(if is_video_playing {
                                                    "СТОП"
                                                } else {
                                                    "ВОСПРОИЗВЕСТИ"
                                                })
                                                    .strong()
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(45, 118, 255))
                                            .rounding(8.0)
                                            .min_size(egui::vec2(150.0, 36.0));

                                            if ui.add(play_btn).clicked() {
                                                if is_video_playing {
                                                    self.stop_video_preview();
                                                } else if let Err(err) =
                                                    self.start_video_preview(&file_path, ctx)
                                                {
                                                    self.video_status = err;
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

            if self.generated_file.is_some() {
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

                            if let Some(texture) = &self.video_texture {
                                ui.add(
                                    egui::Image::new(texture)
                                        .fit_to_exact_size(preview_size)
                                        .rounding(egui::Rounding::same(8.0)),
                                );
                            } else {
                                let (preview_rect, _) =
                                    ui.allocate_exact_size(preview_size, egui::Sense::hover());
                                let painter = ui.painter();
                                painter.rect_filled(
                                    preview_rect,
                                    8.0,
                                    egui::Color32::from_rgb(8, 10, 14),
                                );
                                painter.rect_stroke(
                                    preview_rect,
                                    8.0,
                                    egui::Stroke::new(
                                        1.0,
                                        egui::Color32::from_rgb(48, 58, 74),
                                    ),
                                );
                                painter.circle_filled(
                                    preview_rect.center(),
                                    28.0,
                                    egui::Color32::from_rgb(45, 118, 255),
                                );
                                painter.text(
                                    preview_rect.center() + egui::vec2(2.0, 0.0),
                                    egui::Align2::CENTER_CENTER,
                                    "▶",
                                    egui::FontId::proportional(26.0),
                                    egui::Color32::WHITE,
                                );
                                painter.text(
                                    egui::pos2(
                                        preview_rect.center().x,
                                        preview_rect.center().y + 52.0,
                                    ),
                                    egui::Align2::CENTER_CENTER,
                                    "Нажмите «Воспроизвести», чтобы посмотреть видео здесь",
                                    egui::FontId::proportional(13.0),
                                    muted,
                                );
                            }
                        });
                    });
                    ui.add_space(page_margin);
                });
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
            audio_delay_ms: self.audio_delay_ms,
            artist: self.artist.clone(),
            title: self.title.clone(),
            lyrics: self.lyrics.clone(),
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
            .with_title("Караоке-Видео Генератор (Word-Level)")
            .with_inner_size([1100.0, 750.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Караоке-Видео Генератор",
        options,
        Box::new(|cc| Ok(Box::new(KaraokeApp::new(cc)))),
    )
}
