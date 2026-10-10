//! One persistent alignment worker and one renderer; ready tracks can render
//! while Whisper aligns the next track. Each stage has independent cancellation.

use crate::{
    AlignmentQuality, AppSettings, BatchItem, BatchStatus, CLIProgress, ProgressUpdate,
    batch_item_trim_window, batch_output_path, batch_timings_path_for_item, ffmpeg, lyrics, paths,
};
use desktop_app::{cache, process_runner};
use eframe::egui;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Sender},
};
use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    pub items: Vec<BatchItem>,
    pub settings: AppSettings,
    pub worker: PathBuf,
    pub renderer: PathBuf,
    pub engine: String,
    pub stop_after: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub stop_file: PathBuf,
}

#[derive(Clone)]
struct Task {
    index: usize,
    audio: PathBuf,
    timings: PathBuf,
    output: PathBuf,
    duration: i64,
    identity: cache::AlignmentIdentity,
    signature: String,
}

#[derive(Serialize)]
struct AlignEntry {
    index: usize,
    audio: PathBuf,
    artist: String,
    title: String,
    lyrics_file: PathBuf,
    timings_output: PathBuf,
}

fn metadata_path(output: &Path) -> PathBuf {
    output.with_file_name(format!(
        "{}.render.json",
        output.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn output_is_current(task: &Task) -> bool {
    let result = (|| -> Option<bool> {
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(metadata_path(&task.output)).ok()?).ok()?;
        Some(
            metadata["signature"].as_str()? == task.signature
                && metadata["mp4_sha256"].as_str()? == cache::file_sha256(&task.output).ok()?,
        )
    })();
    result.unwrap_or(false)
}

fn send_status(
    tx: &Sender<ProgressUpdate>,
    ctx: &egui::Context,
    index: usize,
    status: BatchStatus,
    progress: f32,
    message: String,
) {
    let _ = tx.send(ProgressUpdate::BatchProgress {
        index,
        status,
        progress,
        message,
        output_path: None,
        alignment: None,
    });
    ctx.request_repaint();
}

fn error(tx: &Sender<ProgressUpdate>, ctx: &egui::Context, index: usize, message: String) {
    send_status(
        tx,
        ctx,
        index,
        BatchStatus::Error(message.clone()),
        0.0,
        message,
    );
}

fn configure_command(command: &mut Command) {
    ffmpeg::hide_subprocess_window(command);
    if let Some(bin) = paths::bundled_bin_dir() {
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(value) = std::env::join_paths(paths) {
            command.env("PATH", value);
        }
    }
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
}

pub fn run(config: Config, tx: Sender<ProgressUpdate>, ctx: egui::Context) {
    let finish = || {
        let _ = tx.send(ProgressUpdate::BatchFinished);
        ctx.request_repaint();
    };
    let temp = paths::temp_dir().join("batch");
    let mut tasks = HashMap::new();
    let mut queue = Vec::new();
    let mut ready = Vec::new();
    let mut outputs = HashSet::new();
    for (index, item) in config.items.iter().enumerate() {
        if config.cancel.load(Ordering::Acquire) || config.stop_after.load(Ordering::Acquire) {
            break;
        }
        if item.status == BatchStatus::MissingLyrics {
            continue;
        }
        let prepare = || -> Result<(Task, PathBuf, bool), String> {
            let directory = temp.join(format!("{:03}", index + 1));
            std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            let lyrics = if let Some(text) = &item.lyrics_override {
                text.clone()
            } else {
                lyrics::read_lyrics_file(&item.lyrics_path)?
            };
            let (start, end, trim) = batch_item_trim_window(item);
            let lyrics = lyrics::shift_lrc_for_trim(&lyrics, start, end - start);
            let lyrics_file = directory.join("lyrics.txt");
            std::fs::write(&lyrics_file, &lyrics).map_err(|e| e.to_string())?;
            let audio = if trim {
                let output = directory.join("audio.wav");
                ffmpeg::render_trimmed_audio_cancellable(
                    &item.audio_path.to_string_lossy(),
                    start,
                    end,
                    item.fade_in_ms as i64,
                    item.fade_out_ms as i64,
                    &output,
                    config.cancel.clone(),
                )?;
                output
            } else {
                item.audio_path.clone()
            };
            let identity = cache::AlignmentIdentity::new(
                &audio,
                &lyrics,
                &config.settings.model,
                config.settings.plain_lines,
                config.settings.verify_lrc_with_whisper,
                config.settings.separate_vocals_for_word_level && !config.settings.plain_lines,
            )?;
            let timings = batch_timings_path_for_item(item);
            let current =
                !config.settings.batch_overwrite_timings && identity.matches_timings(&timings);
            let s = &config.settings;
            let signature = cache::bytes_sha256(serde_json::json!({
                "alignment": identity, "artist": item.artist, "title": item.title,
                "quality": s.quality, "font": s.font, "font_scale": s.font_scale, "active": s.color_active, "inactive": s.color_inactive,
                "background": s.color_bg, "opacity": s.inactive_opacity, "delay": s.audio_delay_ms,
                "normalize_audio": s.normalize_audio, "loudness": s.loudness_target_db, "engine": config.engine,
                "renderer_version": env!("CARGO_PKG_VERSION"),
            }).to_string().as_bytes());
            Ok((
                Task {
                    index,
                    audio,
                    timings,
                    output: batch_output_path(item, s.plain_lines),
                    duration: end - start,
                    identity,
                    signature,
                },
                lyrics_file,
                current,
            ))
        };
        match prepare() {
            Ok((task, lyrics_file, current)) => {
                if !outputs.insert(task.output.clone()) {
                    error(
                        &tx,
                        &ctx,
                        index,
                        "Повторяющееся имя выходного видео; измените название трека".into(),
                    );
                    continue;
                }
                if item.status == BatchStatus::Done && current && output_is_current(&task) {
                    continue;
                }
                if current {
                    send_status(
                        &tx,
                        &ctx,
                        index,
                        BatchStatus::ReadyToRender,
                        1.0,
                        "Аудио, текст и параметры совпадают: используем тайминги".into(),
                    );
                    ready.push(task.clone());
                } else {
                    send_status(
                        &tx,
                        &ctx,
                        index,
                        BatchStatus::Ready,
                        0.0,
                        "Тайминги отсутствуют или устарели: требуется синхронизация".into(),
                    );
                    queue.push(AlignEntry {
                        index,
                        audio: task.audio.clone(),
                        artist: item.artist.clone(),
                        title: item.title.clone(),
                        lyrics_file,
                        timings_output: task.timings.clone(),
                    });
                    tasks.insert(index, task);
                }
            }
            Err(message) => error(&tx, &ctx, index, message),
        }
    }
    if config.cancel.load(Ordering::Acquire) {
        finish();
        return;
    }
    let (ready_tx, ready_rx) = mpsc::channel();
    for task in ready {
        let _ = ready_tx.send(task);
    }
    let align_tx = tx.clone();
    let align_ctx = ctx.clone();
    let cancel = config.cancel.clone();
    let stop_file = config.stop_file.clone();
    let settings = config.settings.clone();
    let worker = config.worker.clone();
    let alignment = std::thread::spawn(move || {
        if queue.is_empty() {
            return;
        }
        let queue_file = temp.join("align_queue.json");
        let serialized = serde_json::to_vec(&queue)
            .map_err(|e| e.to_string())
            .and_then(|bytes| cache::atomic_write(&queue_file, &bytes));
        if let Err(message) = serialized {
            for index in tasks.keys() {
                error(&align_tx, &align_ctx, *index, message.clone());
            }
            return;
        }
        let mut command = if paths::is_python_worker(&worker) {
            let mut c = Command::new("python3");
            c.arg(worker);
            c
        } else {
            Command::new(worker)
        };
        configure_command(&mut command);
        command
            .args(["--cli", "--batch-align-queue"])
            .arg(queue_file)
            .arg("--stop-after-current-file")
            .arg(&stop_file)
            .arg("--model")
            .arg(&settings.model);
        if settings.plain_lines {
            command.arg("--plain-lines");
        }
        if settings.verify_lrc_with_whisper {
            command.arg("--verify-lrc-with-whisper");
        }
        if settings.separate_vocals_for_word_level && !settings.plain_lines {
            command.arg("--separate-vocals-for-alignment");
        }
        let mut completed = HashSet::new();
        let result = process_runner::run(command, cancel.clone(), |stream, line| {
            if let process_runner::Stream::Stdout = stream
                && let Ok(progress) = serde_json::from_str::<CLIProgress>(line)
                && let Some(index) = progress.batch_align_index
            {
                if completed.contains(&index) {
                    return;
                }
                let status = if let Some(message) = &progress.error {
                    BatchStatus::Error(message.clone())
                } else if progress.done {
                    BatchStatus::ReadyToRender
                } else {
                    BatchStatus::Aligning
                };
                let _ = align_tx.send(ProgressUpdate::BatchProgress {
                    index,
                    status,
                    progress: progress.progress,
                    message: progress.status.clone(),
                    output_path: None,
                    alignment: AlignmentQuality::from_progress(&progress),
                });
                if progress.done || progress.error.is_some() {
                    completed.insert(index);
                    if progress.error.is_none()
                        && let Some(task) = tasks.get(&index)
                    {
                        if task.identity.matches_timings(&task.timings) {
                            let _ = ready_tx.send(task.clone());
                        } else {
                            error(
                                &align_tx,
                                &align_ctx,
                                index,
                                "Worker не создал проверенные тайминги".into(),
                            );
                        }
                    }
                }
                align_ctx.request_repaint();
            } else if !line.is_empty() && !ffmpeg::is_ffmpeg_noise(line) {
                let _ = align_tx.send(ProgressUpdate::RawLog(format!("[Align] {line}")));
            }
        });
        if !cancel.load(Ordering::Acquire) && !stop_file.exists() {
            for index in tasks.keys().filter(|index| !completed.contains(index)) {
                error(
                    &align_tx,
                    &align_ctx,
                    *index,
                    format!("Синхронизация не завершена: {result:?}"),
                );
            }
        }
    });
    let mut rendered = 0;
    loop {
        if config.cancel.load(Ordering::Acquire)
            || (config.stop_after.load(Ordering::Acquire) && rendered > 0)
        {
            break;
        }
        let task = match ready_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(task) => task,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        send_status(
            &tx,
            &ctx,
            task.index,
            BatchStatus::Rendering,
            0.0,
            "Рендеринг".into(),
        );
        let s = &config.settings;
        let rgb = |color: [u8; 3]| format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]);
        let mut command = Command::new(&config.renderer);
        configure_command(&mut command);
        command
            .arg("--timings")
            .arg(&task.timings)
            .arg("--audio")
            .arg(&task.audio)
            .arg("--output")
            .arg(&task.output)
            .arg("--quality")
            .arg(&s.quality)
            .arg("--font")
            .arg(&s.font)
            .arg("--font-scale")
            .arg(s.font_scale.to_string())
            .arg("--engine")
            .arg(&config.engine)
            .arg("--color-active")
            .arg(rgb(s.color_active))
            .arg("--color-inactive")
            .arg(rgb(s.color_inactive))
            .arg("--color-bg")
            .arg(rgb(s.color_bg))
            .arg("--inactive-opacity")
            .arg(s.inactive_opacity.to_string())
            .arg("--audio-delay")
            .arg((s.audio_delay_ms as f64 / 1000.0).to_string())
            .arg("--loudness-target-db")
            .arg(s.loudness_target_db.to_string())
            .arg("--ffmpeg")
            .arg(paths::tool_path("ffmpeg"))
            .arg("--ffprobe")
            .arg(paths::tool_path("ffprobe"));
        if !s.normalize_audio {
            command.arg("--no-normalize-audio");
        }
        if s.plain_lines {
            command.arg("--plain-lines");
        }
        let mut diagnostics = std::collections::VecDeque::new();
        let result = process_runner::run(command, config.cancel.clone(), |_, line| {
            let progress = ffmpeg::parse_ffmpeg_time_ms(line)
                .map(|ms| ms as f32 / task.duration.max(1) as f32)
                .or_else(|| {
                    line.strip_prefix("render ")?
                        .trim_end_matches('%')
                        .parse::<f32>()
                        .ok()
                        .map(|value| value / 100.0)
                });
            if let Some(progress) = progress {
                send_status(
                    &tx,
                    &ctx,
                    task.index,
                    BatchStatus::Rendering,
                    progress.clamp(0.0, 0.98),
                    format!("Рендеринг: {:.0}%", progress * 100.0),
                );
            } else if !line.is_empty() && !ffmpeg::is_ffmpeg_progress_key(line) {
                if diagnostics.len() == 8 {
                    diagnostics.pop_front();
                }
                diagnostics.push_back(line.to_string());
                let _ = tx.send(ProgressUpdate::RawLog(format!("[Render] {line}")));
            }
        });
        rendered += 1;
        if result == Ok(true) && !config.cancel.load(Ordering::Acquire) {
            let hash = match cache::file_sha256(&task.output) {
                Ok(hash) => hash,
                Err(message) => {
                    error(
                        &tx,
                        &ctx,
                        task.index,
                        format!("Рендер не создал доступный MP4: {message}"),
                    );
                    continue;
                }
            };
            let metadata = serde_json::json!({"signature": task.signature, "mp4_sha256": hash});
            if let Err(message) = cache::atomic_write(
                &metadata_path(&task.output),
                metadata.to_string().as_bytes(),
            ) {
                let _ = tx.send(ProgressUpdate::RawLog(format!(
                    "Не удалось сохранить метаданные видео: {message}"
                )));
            }
            let _ = tx.send(ProgressUpdate::BatchProgress {
                index: task.index,
                status: BatchStatus::Done,
                progress: 1.0,
                message: "Готово".into(),
                output_path: Some(task.output.to_string_lossy().into()),
                alignment: None,
            });
        } else if !config.cancel.load(Ordering::Acquire) {
            error(
                &tx,
                &ctx,
                task.index,
                format!(
                    "Рендер не завершён: {result:?}\n{}",
                    diagnostics.into_iter().collect::<Vec<_>>().join("\n")
                ),
            );
        }
        ctx.request_repaint();
    }
    let _ = alignment.join();
    finish();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fixture(root: &Path, count: usize) -> Config {
        std::fs::create_dir_all(root).unwrap();
        let worker = root.join("worker.py");
        let renderer = root.join("renderer");
        let modules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../worker");
        std::fs::write(&worker, format!(r#"import sys, json, time, pathlib
sys.path.insert(0, {modules})
from cache_identity import alignment_identity, atomic_write_json, file_sha256
args = sys.argv
queue = json.load(open(args[args.index('--batch-align-queue')+1]))
stop = pathlib.Path(args[args.index('--stop-after-current-file')+1])
for item in queue:
    if stop.exists(): break
    pathlib.Path(__file__).with_name('received-lyrics-%d.txt' % item['index']).write_text(pathlib.Path(item['lyrics_file']).read_text())
    time.sleep(0.15 if item['index'] == 0 else 0.7)
    timings = pathlib.Path(item['timings_output'])
    atomic_write_json(str(timings), [])
    identity = alignment_identity(item['audio'], pathlib.Path(item['lyrics_file']).read_text(), 'base', False, True, False)
    atomic_write_json(str(timings.with_name(timings.stem + '_alignment_report.json')), {{'cache_identity': identity, 'timings_sha256': file_sha256(timings)}})
    with open(pathlib.Path(__file__).parent/'events', 'a') as events: events.write('align-%d\n' % item['index'])
    print(json.dumps({{'batch_align_index': item['index'], 'progress': 1.0, 'status': 'done', 'done': True}}), flush=True)
"#, modules=serde_json::to_string(&modules.to_string_lossy()).unwrap())).unwrap();
        std::fs::write(
            &renderer,
            r#"#!/usr/bin/env python3
import sys, time, pathlib
root = pathlib.Path(__file__).parent
output = pathlib.Path(sys.argv[sys.argv.index('--output')+1])
with open(root/'events', 'a') as events: events.write('render-start\n')
time.sleep(0.35)
output.write_bytes(b'encoded fixture')
with open(root/'events', 'a') as events: events.write('render-done\n')
"#,
        )
        .unwrap();
        std::fs::set_permissions(&renderer, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut items = Vec::new();
        for i in 0..count {
            let folder = root.join(format!("song-{i}"));
            std::fs::create_dir_all(&folder).unwrap();
            let audio = folder.join("audio.wav");
            let lyrics = folder.join("lyrics.txt");
            std::fs::write(&audio, format!("audio {i}")).unwrap();
            std::fs::write(&lyrics, "[00:00]hello").unwrap();
            items.push(BatchItem {
                folder,
                audio_path: audio,
                lyrics_path: lyrics,
                lyrics_override: None,
                artist: "A".into(),
                title: format!("T{i}"),
                duration_ms: 2000,
                trim_start_ms: 0,
                trim_end_ms: 2000,
                fade_in_ms: 0,
                fade_out_ms: 0,
                status: BatchStatus::Ready,
                progress: 0.0,
                output_path: None,
                alignment_quality: None,
                audit: None,
                waveform: None,
            });
        }
        Config {
            items,
            settings: AppSettings::default(),
            worker,
            renderer,
            engine: "frames".into(),
            stop_after: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            stop_file: root.join("stop"),
        }
    }

    #[test]
    fn alignment_and_render_overlap_and_resume_skips_valid_outputs() {
        let _guard = SERIAL.lock().unwrap();
        let root = std::env::temp_dir().join(format!("karaoke-overlap-{}", std::process::id()));
        let mut config = fixture(&root, 2);
        let mut items = config.items.clone();
        let (tx, rx) = mpsc::channel();
        run(config, tx, egui::Context::default());
        for message in rx {
            if let ProgressUpdate::BatchProgress {
                index,
                status: BatchStatus::Done,
                output_path,
                ..
            } = message
            {
                items[index].status = BatchStatus::Done;
                items[index].output_path = output_path;
            }
        }
        assert!(items.iter().all(|item| item.status == BatchStatus::Done));
        let events = std::fs::read_to_string(root.join("events")).unwrap();
        assert!(
            events.find("render-start").unwrap() < events.find("align-1").unwrap(),
            "{events}"
        );
        config = Config {
            items,
            settings: AppSettings::default(),
            worker: root.join("worker.py"),
            renderer: root.join("renderer"),
            engine: "frames".into(),
            stop_after: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            stop_file: root.join("stop"),
        };
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let retry = config.clone();
        run(config, tx, egui::Context::default());
        assert!(started.elapsed() < Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            events
        );
        assert!(
            rx.into_iter()
                .any(|message| matches!(message, ProgressUpdate::BatchFinished))
        );
        std::fs::write(&retry.items[0].audio_path, b"audio X").unwrap();
        let (tx, rx) = mpsc::channel();
        run(retry, tx, egui::Context::default());
        assert!(rx.into_iter().any(|message| matches!(
            message,
            ProgressUpdate::BatchProgress {
                index: 0,
                status: BatchStatus::Done,
                ..
            }
        )));
        let changed = std::fs::read_to_string(root.join("events")).unwrap();
        assert_eq!(changed.matches("align-0").count(), 2);
        assert_eq!(changed.matches("align-1").count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_lyrics_override_is_trimmed_before_alignment() {
        let _guard = SERIAL.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("karaoke-project-lyrics-{}", std::process::id()));
        let mut config = fixture(&root, 1);
        config.items[0].lyrics_override =
            Some("[00:00.00]Project edit\n[00:02.00]At end\n[00:02.20]After".into());
        let original = config.items[0].lyrics_path.clone();
        let (tx, rx) = mpsc::channel();
        run(config, tx, egui::Context::default());
        assert_eq!(
            std::fs::read_to_string(root.join("received-lyrics-0.txt")).unwrap(),
            "[00:00.00]Project edit"
        );
        assert_eq!(std::fs::read_to_string(original).unwrap(), "[00:00]hello");
        assert!(rx.into_iter().any(|message| matches!(
            message,
            ProgressUpdate::BatchProgress {
                status: BatchStatus::Done,
                ..
            }
        )));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graceful_stop_finishes_current_render_and_retains_next_timings() {
        let _guard = SERIAL.lock().unwrap();
        let root = std::env::temp_dir().join(format!("karaoke-graceful-{}", std::process::id()));
        let config = fixture(&root, 3);
        let stop = config.stop_after.clone();
        let stop_file = config.stop_file.clone();
        let second_timings = batch_timings_path_for_item(&config.items[1]);
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || run(config, tx, egui::Context::default()));
        let mut done = Vec::new();
        for message in rx {
            match message {
                ProgressUpdate::BatchProgress {
                    status: BatchStatus::Rendering,
                    ..
                } => {
                    stop.store(true, Ordering::Release);
                    std::fs::write(&stop_file, b"stop").unwrap();
                }
                ProgressUpdate::BatchProgress {
                    index,
                    status: BatchStatus::Done,
                    ..
                } => done.push(index),
                _ => {}
            }
        }
        thread.join().unwrap();
        assert_eq!(done, vec![0]);
        assert!(second_timings.is_file());
        let events = std::fs::read_to_string(root.join("events")).unwrap();
        assert!(!events.contains("align-2"));
        assert_eq!(events.matches("render-start").count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_renderer_output_is_reported_as_error() {
        let _guard = SERIAL.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("karaoke-missing-output-{}", std::process::id()));
        let config = fixture(&root, 1);
        std::fs::write(&config.renderer, "#!/bin/sh\nexit 0\n").unwrap();
        let (tx, rx) = mpsc::channel();
        run(config, tx, egui::Context::default());
        let messages = rx.into_iter().collect::<Vec<_>>();
        assert!(!messages.iter().any(|m| matches!(
            m,
            ProgressUpdate::BatchProgress {
                status: BatchStatus::Done,
                ..
            }
        )));
        assert!(messages.iter().any(|m| matches!(
            m,
            ProgressUpdate::BatchProgress {
                status: BatchStatus::Error(_),
                ..
            }
        )));
        std::fs::remove_dir_all(root).unwrap();
    }
}
