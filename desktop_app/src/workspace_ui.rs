//! Compact native workspaces. Processing and project formats stay in their existing modules.
use crate::{
    ActiveTab, AuditLevel, BatchItem, BatchStatus, KaraokeApp, batch_project,
    batch_trim_timeline_static_ui, ffmpeg, font_display_name, lyrics, open_in_explorer, open_path,
    quality_display_name, refresh_batch_item_audit,
};
use desktop_app::cache;
use eframe::egui::{self, Color32, RichText};
use std::path::Path;

const ACCENT: Color32 = Color32::from_rgb(118, 166, 255);
const MUTED: Color32 = Color32::from_rgb(155, 169, 187);
const LINE: Color32 = Color32::from_rgb(43, 52, 66);
const GREEN: Color32 = Color32::from_rgb(129, 211, 176);
const AMBER: Color32 = Color32::from_rgb(239, 193, 116);
const RED: Color32 = Color32::from_rgb(246, 149, 157);

fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(Color32::from_rgb(23, 30, 40))
        .stroke(egui::Stroke::new(1.0, LINE))
        .rounding(8.0)
        .inner_margin(12.0)
}
fn heading(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .family(egui::FontFamily::Name("karaoke_ui_bold".into()))
            .size(16.0),
    );
}
fn caption(ui: &mut egui::Ui, value: impl Into<String>) {
    ui.label(RichText::new(value).size(12.0).color(MUTED));
}
fn action(text: &str) -> egui::Button<'_> {
    egui::Button::new(
        RichText::new(text)
            .strong()
            .color(Color32::from_rgb(16, 33, 60)),
    )
    .fill(ACCENT)
    .min_size(egui::vec2(150.0, 34.0))
}

#[derive(Default)]
pub(crate) struct FramePreview {
    key: String,
    requested: String,
    displayed: String,
    changed: Option<std::time::Instant>,
    pending: Option<std::sync::mpsc::Receiver<(String, Result<image::RgbaImage, String>)>>,
    texture: Option<egui::TextureHandle>,
    error: String,
    style_key: String,
    show_video: bool,
    pub(crate) line_time_ms: Option<i64>,
}
impl FramePreview {
    fn update(
        &mut self,
        ctx: &egui::Context,
        key: String,
        request: desktop_app::render::PreviewRequest,
    ) {
        if self.key != key {
            self.key = key.clone();
        }
        if let Some(result) = self
            .pending
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.pending = None;
            if result.0 == key {
                match result.1 {
                    Ok(frame) => {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width() as usize, frame.height() as usize],
                            frame.as_raw(),
                        );
                        if let Some(texture) = &mut self.texture {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.texture = Some(ctx.load_texture(
                                "composition_frame",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        self.error.clear();
                        self.displayed = result.0;
                    }
                    Err(error) => {
                        self.error = error;
                        self.texture = None;
                    }
                }
            }
        }
        if self.pending.is_none()
            && self.requested != key
            && self
                .changed
                .is_none_or(|time| time.elapsed() >= std::time::Duration::from_millis(160))
        {
            self.requested = key.clone();
            self.changed = Some(std::time::Instant::now());
            let request_key = key.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending = Some(rx);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send((request_key, desktop_app::render::preview_frame(request)));
                ctx.request_repaint();
            });
        }
        if self.pending.is_some() || self.requested != key {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}
fn preview_timings(source: &str, duration: f64) -> String {
    let lines = editable_lines(source);
    let timed = lines.iter().any(|line| !line.times.is_empty());
    let mut rows: Vec<(f64, String)> = lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            let times = if timed {
                line.times
                    .iter()
                    .map(|ms| *ms as f64 / 1000.0)
                    .collect::<Vec<_>>()
            } else {
                vec![index as f64 * 4.0]
            };
            times
                .into_iter()
                .filter(|_| !line.text.trim().is_empty() && !line.text.starts_with('['))
                .map(|time| (time, line.text.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    if rows.is_empty() {
        rows.push((0.0, "Здесь появится текст песни".into()));
    }
    let values: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, (start, text))| {
            let end = rows
                .get(index + 1)
                .map(|line| line.0)
                .unwrap_or(duration.max(start + 4.0))
                .max(*start);
            {
                let words: Vec<_> = text
                    .split_whitespace()
                    .map(|word| serde_json::json!({"word":word,"start":start,"end":end}))
                    .collect();
                serde_json::json!({"start":start,"end":end,"words":words})
            }
        })
        .collect();
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".into())
}

#[derive(Default, Clone, Copy, PartialEq)]
pub enum BatchFilter {
    #[default]
    All,
    Done,
    Review,
    Errors,
}
fn matches_filter(item: &BatchItem, filter: BatchFilter) -> bool {
    match filter {
        BatchFilter::All => true,
        BatchFilter::Done => item.status == BatchStatus::Done,
        BatchFilter::Review => {
            item.status == BatchStatus::MissingLyrics
                || item
                    .audit
                    .as_ref()
                    .is_some_and(|a| matches!(a.level, AuditLevel::Warning | AuditLevel::Error))
                || item.alignment_quality.as_ref().is_some_and(|q| {
                    matches!(
                        q.summary.as_str(),
                        "unverified" | "minor_warnings" | "suspicious" | "needs_repair"
                    )
                })
        }
        BatchFilter::Errors => matches!(item.status, BatchStatus::Error(_)),
    }
}
fn status(item: &BatchItem) -> (String, Color32) {
    match &item.status {
        BatchStatus::Ready => ("В очереди".into(), MUTED),
        BatchStatus::MissingLyrics => ("Нет текста".into(), AMBER),
        BatchStatus::Aligning => (
            format!("Синхронизация · {:.0}%", item.progress * 100.0),
            ACCENT,
        ),
        BatchStatus::ReadyToRender => ("Готов к рендеру".into(), ACCENT),
        BatchStatus::Rendering => (format!("Рендер · {:.0}%", item.progress * 100.0), ACCENT),
        BatchStatus::Done => ("Готово".into(), GREEN),
        BatchStatus::Error(_) => ("Ошибка".into(), RED),
    }
}

// Only valid leading timestamps are separated. Metadata, multiple tags, spacing,
// CRLF and final newlines remain byte-for-byte intact when another line is edited.
#[derive(Debug)]
struct EditableLine {
    prefix: String,
    text: String,
    ending: String,
    times: Vec<i64>,
}
fn editable_lines(source: &str) -> Vec<EditableLine> {
    source
        .split_inclusive('\n')
        .map(|raw| {
            let (body, ending) = if let Some(body) = raw.strip_suffix("\r\n") {
                (body, "\r\n")
            } else if let Some(body) = raw.strip_suffix('\n') {
                (body, "\n")
            } else {
                (raw, "")
            };
            let mut rest = body;
            let mut times = Vec::new();
            while let Some(tag) = rest.strip_prefix('[') {
                let Some(end) = tag.find(']') else { break };
                let Some(ms) = lyrics::parse_lrc_timestamp_ms(&tag[..end]).filter(|ms| *ms >= 0)
                else {
                    break;
                };
                times.push(ms);
                rest = &tag[end + 1..];
            }
            EditableLine {
                prefix: body[..body.len() - rest.len()].into(),
                text: rest.into(),
                ending: ending.into(),
                times,
            }
        })
        .collect()
}
fn join_lines(lines: &[EditableLine]) -> String {
    lines
        .iter()
        .map(|line| format!("{}{}{}", line.prefix, line.text, line.ending))
        .collect()
}
fn included_times(line: &EditableLine, trim: Option<(i64, i64)>) -> Vec<i64> {
    line.times
        .iter()
        .copied()
        .filter(|time| {
            trim.is_none_or(|(start, duration)| lyrics::timestamp_in_trim(*time, start, duration))
        })
        .collect()
}

#[cfg(test)]
fn preview_lines(source: &str, position_ms: i64) -> (String, String) {
    let lines = editable_lines(source);
    let mut timed: Vec<_> = lines
        .iter()
        .flat_map(|line| line.times.iter().map(|ms| (*ms, line.text.trim())))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    timed.sort_by_key(|(ms, _)| *ms);
    if !timed.is_empty() {
        let index = timed
            .iter()
            .rposition(|(ms, _)| *ms <= position_ms)
            .unwrap_or(0);
        return (
            timed[index].1.into(),
            timed
                .get(index + 1)
                .map(|(_, text)| *text)
                .unwrap_or("")
                .into(),
        );
    }
    let mut text = lines
        .iter()
        .map(|line| line.text.trim())
        .filter(|s| !s.is_empty() && !s.starts_with('['));
    (
        text.next().unwrap_or("Здесь появится текст песни").into(),
        text.next().unwrap_or("").into(),
    )
}

impl KaraokeApp {
    pub(crate) fn workspace_header(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
        ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
        ui.spacing_mut().interact_size = egui::vec2(30.0, 30.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Караоке").strong().size(19.0));
            caption(ui, "Генератор видео");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Настройки").clicked() {
                    self.ui_advanced_open = true;
                }
                if ui.button("Логи").clicked() {
                    self.ui_logs_open = true;
                }
            });
        });
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            for (tab, label) in [
                (ActiveTab::SingleTrack, "Один трек"),
                (ActiveTab::Batch, "Пакетный рендеринг"),
                (ActiveTab::Downloader, "Поиск аудио"),
                (ActiveTab::PptxOptimizer, "Оптимизация PPTX"),
            ] {
                if ui
                    .add_enabled(
                        !(tab == ActiveTab::SingleTrack && self.batch_running),
                        egui::Button::new(label).selected(self.active_tab == tab),
                    )
                    .clicked()
                {
                    self.active_tab = tab;
                }
            }
        });
        ui.separator();
    }

    pub(crate) fn workspace_windows(&mut self, ctx: &egui::Context) {
        let mut advanced = self.ui_advanced_open;
        egui::Window::new("Настройки генерации")
            .open(&mut advanced)
            .default_width(480.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(560.0)
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
                            self.render_settings_ui(ui, true);
                            ui.separator();
                            heading(ui, "Оформление");
                            egui::Grid::new("appearance_grid")
                                .spacing([20.0, 10.0])
                                .show(ui, |ui| {
                                    ui.label("Подсветка слов");
                                    ui.color_edit_button_srgb(&mut self.color_active);
                                    ui.end_row();
                                    ui.label("Следующие слова");
                                    ui.color_edit_button_srgb(&mut self.color_inactive);
                                    ui.end_row();
                                    ui.label("Фон");
                                    ui.color_edit_button_srgb(&mut self.color_bg);
                                    ui.end_row();
                                });
                            ui.add(
                                egui::Slider::new(&mut self.inactive_opacity, 0.0..=1.0)
                                    .text("Соседние строки"),
                            );
                            ui.add(
                                egui::Slider::new(&mut self.audio_delay_ms, -500..=500)
                                    .text("Сдвиг аудио")
                                    .suffix(" мс"),
                            );
                            ui.separator();
                            heading(ui, "Синхронизация");
                            ui.checkbox(&mut self.plain_lines, "Только строки без подсветки слов");
                            ui.checkbox(
                                &mut self.verify_lrc_with_whisper,
                                "Проверять LRC через Whisper",
                            );
                            ui.checkbox(
                                &mut self.separate_vocals_for_word_level,
                                "Выделять вокал через Demucs",
                            );
                            ui.checkbox(
                                &mut self.batch_overwrite_timings,
                                "Пересчитывать тайминги пакета",
                            );
                            self.loudness_controls(ui);
                        });
                        if self.batch_running {
                            caption(ui, "Настройки доступны после завершения обработки пакета.");
                        }
                        ui.separator();
                        caption(
                            ui,
                            format!(
                                "Версия {} · настройки сохраняются автоматически",
                                crate::APP_VERSION
                            ),
                        );
                    });
            });
        self.ui_advanced_open = advanced;
        let mut logs = self.ui_logs_open;
        egui::Window::new("Журнал обработки")
            .open(&mut logs)
            .default_size([720.0, 400.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.log_output)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
            });
        self.ui_logs_open = logs;
    }

    fn render_settings_ui(&mut self, ui: &mut egui::Ui, include_model: bool) {
        let control_width = (ui.available_width() - 120.0).clamp(120.0, 350.0);
        egui::Grid::new(ui.id().with("render_controls"))
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                ui.label("Качество");
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(quality_display_name(&self.quality))
                    .width(control_width)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for q in ["medium", "high", "ultra"] {
                            ui.selectable_value(
                                &mut self.quality,
                                q.into(),
                                quality_display_name(q),
                            );
                        }
                    });
                ui.end_row();
                ui.label("Шрифт");
                egui::ComboBox::from_id_salt("font")
                    .selected_text(font_display_name(&self.font))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.font, "montserrat".into(), "Montserrat");
                    });
                ui.end_row();
                if include_model {
                    ui.label("Модель Whisper");
                    egui::ComboBox::from_id_salt("model")
                        .selected_text(&self.model)
                        .show_ui(ui, |ui| {
                            for m in ["base", "small", "medium"] {
                                ui.selectable_value(&mut self.model, m.into(), m);
                            }
                        });
                    ui.end_row();
                    ui.label("Размер текста");
                    let mut percent = self.font_scale * 100.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut percent, 70.0..=130.0)
                                .suffix(" %")
                                .step_by(1.0),
                        )
                        .changed()
                    {
                        self.font_scale = percent / 100.0;
                    }
                    ui.end_row();
                }
            });
    }

    fn source_picker_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let card_width = ui.available_width() - 24.0;
        card().show(ui, |ui| {
            ui.set_min_width(card_width);
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 130.0).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_width(width);
                        let name = self
                            .audio_path
                            .as_ref()
                            .map(|p| lyrics::display_file_name(Path::new(p)))
                            .unwrap_or_else(|| "Добавьте аудиофайл".into());
                        ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                        caption(
                            ui,
                            self.audio_duration_ms
                                .map(|ms| {
                                    format!("{} · {}", self.artist, ffmpeg::format_time_ms(ms))
                                })
                                .unwrap_or_else(|| "Перетащите MP3 в окно".into()),
                        );
                    },
                );
                if ui
                    .add_enabled(
                        !self.is_generating && !self.batch_running,
                        egui::Button::new("Выбрать файл"),
                    )
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Аудио", &["mp3"])
                        .pick_file()
                {
                    self.set_audio_file(path, ctx);
                }
            });
        });
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                ui.add_sized(
                    [width, 32.0],
                    egui::TextEdit::singleline(&mut self.artist).hint_text("Исполнитель"),
                );
                ui.add_sized(
                    [width, 32.0],
                    egui::TextEdit::singleline(&mut self.title).hint_text("Название песни"),
                );
            });
        });
    }

    fn audio_editor_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(duration) = self.audio_duration_ms {
            let compact = ui.available_width() < 360.0;
            card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    caption(ui, "ФРАГМЕНТ");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{} → {} · {:.1} с",
                                    ffmpeg::format_time_ms(self.trim_start_ms),
                                    ffmpeg::format_time_ms(self.trim_end_ms),
                                    (self.trim_end_ms - self.trim_start_ms) as f64 / 1000.0
                                ))
                                .size(12.0)
                                .color(MUTED),
                            )
                            .truncate(),
                        );
                    });
                });
                self.trim_timeline_ui(ui, duration, ACCENT, ACCENT, MUTED);
                ui.horizontal(|ui| {
                    let playing = self.is_preview_playing();
                    if ui
                        .add_enabled(
                            !self.is_generating,
                            egui::Button::new(if playing {
                                "Стоп"
                            } else {
                                "Слушать"
                            }),
                        )
                        .clicked()
                    {
                        self.ui_frame_preview.line_time_ms = None;
                        if playing {
                            self.stop_preview();
                        } else {
                            self.preview_trimmed_audio(ctx);
                        }
                    }
                    if !compact {
                        caption(ui, ffmpeg::format_time_ms(self.trim_playhead_ms));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let before = self.preview_normalized;
                        ui.add_enabled_ui(self.normalize_audio, |ui| {
                            ui.selectable_value(
                                &mut self.preview_normalized,
                                true,
                                RichText::new("Выровнено").size(12.0),
                            );
                            ui.selectable_value(
                                &mut self.preview_normalized,
                                false,
                                RichText::new("Оригинал").size(12.0),
                            );
                        });
                        if before != self.preview_normalized && playing {
                            self.preview_trimmed_audio(ctx);
                        }
                    });
                });
            });
            let before = (
                self.trim_start_ms,
                self.trim_end_ms,
                self.fade_in_ms,
                self.fade_out_ms,
            );
            ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
                ui.columns(2, |columns| {
                    caption(&mut columns[0], "Начало");
                    let width = columns[0].available_width();
                    columns[0].add_sized(
                        [width, 30.0],
                        time_drag(&mut self.trim_start_ms)
                            .range(0..=self.trim_end_ms.saturating_sub(duration.min(1000)).max(0)),
                    );
                    caption(&mut columns[1], "Конец");
                    let width = columns[1].available_width();
                    columns[1].add_sized(
                        [width, 30.0],
                        time_drag(&mut self.trim_end_ms)
                            .range(self.trim_start_ms + duration.min(1000)..=duration),
                    );
                });
                egui::CollapsingHeader::new("Плавный вход / выход")
                    .id_salt(ui.id().with("fades"))
                    .show(ui, |ui| {
                        ui.columns(2, |columns| {
                            caption(&mut columns[0], "Вход");
                            columns[0].add(
                                egui::DragValue::new(&mut self.fade_in_ms)
                                    .range(0..=30_000)
                                    .suffix(" мс")
                                    .speed(50),
                            );
                            caption(&mut columns[1], "Выход");
                            columns[1].add(
                                egui::DragValue::new(&mut self.fade_out_ms)
                                    .range(0..=30_000)
                                    .suffix(" мс")
                                    .speed(-50),
                            ).on_hover_text("Тяните вправо, чтобы затухание начиналось позже; влево — раньше. Двойной клик — ввод длительности.");
                        });
                        if ui.button("Весь трек").clicked() {
                            self.trim_start_ms = 0;
                            self.trim_end_ms = duration;
                        }
                    });
            });
            self.normalize_trim_state();
            if before
                != (
                    self.trim_start_ms,
                    self.trim_end_ms,
                    self.fade_in_ms,
                    self.fade_out_ms,
                )
            {
                self.stop_preview();
            }
            if !self.trim_status.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&self.trim_status).size(12.0).color(MUTED))
                        .wrap(),
                );
            }
        } else if self.audio_path.is_some() {
            ui.spinner();
            caption(ui, "Читаем длительность аудио…");
        }
    }

    fn select_preview_line(&mut self, time_ms: i64, ctx: &egui::Context) {
        self.stop_preview();
        self.trim_playhead_ms =
            time_ms.clamp(self.trim_start_ms, self.trim_end_ms.max(self.trim_start_ms));
        self.ui_frame_preview.line_time_ms = Some(time_ms);
        self.ui_frame_preview.show_video = false;
        ctx.request_repaint();
    }

    fn lyrics_editor_ui(&mut self, ui: &mut egui::Ui, height: f32) -> bool {
        let mut changed = false;
        let mut lines = editable_lines(&self.lyrics);
        let timed = lines.iter().any(|line| !line.times.is_empty());
        let trim = self
            .audio_duration_ms
            .map(|_| (self.trim_start_ms, self.trim_end_ms - self.trim_start_ms));
        let total_timed = lines.iter().filter(|line| !line.times.is_empty()).count();
        let included = lines
            .iter()
            .filter(|line| !included_times(line, trim).is_empty())
            .count();
        ui.horizontal(|ui| {
            heading(ui, "Текст песни");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.ui_lyrics_source, true, "LRC / исходник");
                ui.selectable_value(&mut self.ui_lyrics_source, false, "Строки");
            });
        });
        egui::Frame::none()
            .fill(Color32::from_rgb(18, 25, 35))
            .stroke(egui::Stroke::new(1.0, LINE))
            .rounding(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_min_height(height);
                ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
                    if self.ui_lyrics_source || !timed {
                        egui::ScrollArea::vertical()
                            .id_salt("lyrics_source")
                            .max_height(height)
                            .show(ui, |ui| {
                                changed = ui
                                    .add_sized(
                                        [ui.available_width(), height],
                                        egui::TextEdit::multiline(&mut self.lyrics)
                                            .hint_text(
                                                "Вставьте текст песни или загрузите .txt / .lrc",
                                            )
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY),
                                    )
                                    .changed();
                            });
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("lyrics_lines")
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show_rows(ui, 32.0, lines.len(), |ui, range| {
                                for index in range {
                                    let line = &mut lines[index];
                                    let kept = included_times(line, trim);
                                    let excluded = !line.times.is_empty() && kept.is_empty();
                                    let partial = !excluded && kept.len() < line.times.len();
                                    let selected_time = kept.first().or(line.times.first()).copied();
                                    ui.push_id(index, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [23.0, 32.0],
                                                egui::Label::new(
                                                    RichText::new(format!("{}", index + 1))
                                                        .size(12.0)
                                                        .color(MUTED),
                                                ),
                                            );
                                            let stamp = line
                                                .times.first()
                                                .map(|ms| ffmpeg::format_time_ms(*ms))
                                                .unwrap_or_else(|| "—".into());
                                            if ui
                                                .add_sized(
                                                    [68.0, 32.0],
                                                    egui::Button::new(
                                                        RichText::new(stamp).small().color(if excluded { MUTED } else { ACCENT }),
                                                    )
                                                    .frame(false),
                                                )
                                                .on_hover_text(if excluded { "Эта строка за границами фрагмента и не попадёт в видео" } else { "Показать строку в предпросмотре" })
                                                .clicked()
                                                && let Some(ms) = selected_time
                                            {
                                                self.select_preview_line(ms, ui.ctx());
                                            }
                                            if partial {
                                                ui.label(RichText::new(format!("{}/{}", kept.len(), line.times.len())).size(11.0).color(MUTED))
                                                    .on_hover_text("В видео попадут только повторы внутри фрагмента.");
                                            }
                                            let response = ui.add_sized(
                                                [ui.available_width(), 32.0],
                                                egui::TextEdit::singleline(&mut line.text)
                                                    .text_color(if excluded { Color32::from_rgb(108, 120, 138) } else { ui.visuals().text_color() })
                                                    .frame(false),
                                            );
                                            changed |= response.changed();
                                            if (response.clicked()
                                                || response.gained_focus()
                                                || response.changed())
                                                && let Some(ms) = selected_time
                                            {
                                                self.select_preview_line(ms, ui.ctx());
                                            }
                                        });
                                    });
                                }
                            });
                        if changed {
                            self.lyrics = join_lines(&lines);
                        }
                    }
                });
            });
        ui.horizontal(|ui| {
            caption(
                ui,
                if timed && trim.is_some() {
                    format!(
                        "{included} из {total_timed} строк в видео · {} вне фрагмента",
                        total_timed - included
                    )
                } else {
                    format!(
                        "{} строк{}",
                        self.lyrics.lines().filter(|s| !s.trim().is_empty()).count(),
                        if timed {
                            " · метки LRC сохранены"
                        } else {
                            ""
                        }
                    )
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.is_generating && !self.batch_running,
                        egui::Button::new("Загрузить текст"),
                    )
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Текст песни", &["txt", "lrc"])
                        .pick_file()
                {
                    self.set_lyrics_file(path);
                    changed = true;
                }
            });
        });
        if !timed
            && trim.is_some_and(|(start, duration)| {
                start > 0 || duration < self.audio_duration_ms.unwrap_or(0)
            })
            && !self.lyrics.trim().is_empty()
        {
            caption(
                ui,
                "Без меток LRC границы текста неизвестны — оставьте текст выбранного фрагмента.",
            );
        }
        changed
    }

    fn composition_preview_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            heading(
                ui,
                if self.ui_frame_preview.show_video {
                    "Предпросмотр видео"
                } else {
                    "Предпросмотр оформления"
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Оформление").clicked() {
                    self.ui_advanced_open = true;
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Размер текста");
            let mut percent = self.font_scale * 100.0;
            ui.add_enabled_ui(!self.is_generating && !self.batch_running, |ui| {
                if ui
                    .add(
                        egui::Slider::new(&mut percent, 70.0..=130.0)
                            .suffix(" %")
                            .step_by(1.0),
                    )
                    .changed()
                {
                    self.font_scale = percent / 100.0;
                }
                if ui
                    .button("Сброс")
                    .on_hover_text("Вернуть размер текста к 100 %")
                    .clicked()
                {
                    self.font_scale = 1.0;
                }
            });
        });
        let dimensions = if self.quality == "ultra" {
            "2704 × 448"
        } else {
            "1352 × 224"
        };
        caption(ui, format!("Видео {dimensions} · соотношение 6.04:1"));
        let style_key = format!(
            "{:?}",
            (
                self.font_scale,
                self.color_active,
                self.color_inactive,
                self.color_bg,
                self.inactive_opacity,
                &self.quality
            )
        );
        if style_key != self.ui_frame_preview.style_key {
            self.ui_frame_preview.show_video = false;
            self.ui_frame_preview.style_key = style_key.clone();
        }
        let selected = self.ui_frame_preview.line_time_ms;
        let has_audio = self.audio_duration_ms.is_some();
        let duration = (self.trim_end_ms - self.trim_start_ms).max(0) as f64 / 1000.0;
        let source = if has_audio {
            lyrics::shift_lrc_for_trim(
                &self.lyrics,
                self.trim_start_ms,
                self.trim_end_ms - self.trim_start_ms,
            )
        } else {
            self.lyrics.clone()
        };
        let focus_time = selected
            .map(|time| (time - if has_audio { self.trim_start_ms } else { 0 }) as f64 / 1000.0);
        let position = if let Some(time) = focus_time {
            (time + 0.7 + self.audio_delay_ms as f64 / 1000.0).clamp(0.0, duration.max(0.1))
        } else {
            (self.trim_playhead_ms - self.trim_start_ms).max(0) as f64 / 1000.0
        };
        let excluded = has_audio
            && selected.is_some_and(|time| {
                !lyrics::timestamp_in_trim(
                    time,
                    self.trim_start_ms,
                    self.trim_end_ms - self.trim_start_ms,
                )
            });
        let no_included_lines = has_audio
            && editable_lines(&self.lyrics)
                .iter()
                .any(|line| !line.times.is_empty())
            && !editable_lines(&source)
                .iter()
                .any(|line| !line.times.is_empty());
        let timings = preview_timings(&source, duration);
        let key = cache::bytes_sha256(
            format!(
                "{style_key}:{timings}:{:.1}:{}:{focus_time:?}",
                position, self.audio_delay_ms
            )
            .as_bytes(),
        );
        if !self.ui_frame_preview.show_video && !excluded && !no_included_lines {
            self.ui_frame_preview.update(
                ctx,
                key,
                desktop_app::render::PreviewRequest {
                    timings,
                    focus_time,
                    duration,
                    position: (position * 10.0).round() / 10.0,
                    quality: self.quality.clone(),
                    active: self.color_active,
                    inactive: self.color_inactive,
                    background: self.color_bg,
                    inactive_opacity: self.inactive_opacity,
                    audio_delay: self.audio_delay_ms as f64 / 1000.0,
                    font_scale: self.font_scale,
                    engine: crate::paths::render_engine().unwrap_or_else(|_| "frames".into()),
                    ffmpeg: crate::paths::tool_path("ffmpeg"),
                },
            );
        }
        let width = ui.available_width();
        let size = egui::vec2(width, width * 224.0 / 1352.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            0.0,
            Color32::from_rgb(self.color_bg[0], self.color_bg[1], self.color_bg[2]),
        );
        let texture = if excluded || no_included_lines {
            None
        } else if self.ui_frame_preview.show_video {
            self.video_texture.as_ref()
        } else if self.ui_frame_preview.displayed == self.ui_frame_preview.key {
            self.ui_frame_preview.texture.as_ref()
        } else {
            None
        };
        if let Some(texture) = texture {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if excluded || no_included_lines {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if excluded {
                    "Эта строка не попадёт в видео"
                } else {
                    "В этом фрагменте нет строк LRC"
                },
                egui::FontId::proportional(13.0),
                Color32::GRAY,
            );
        } else if (texture.is_none()
            || (!self.ui_frame_preview.show_video
                && self.ui_frame_preview.displayed != self.ui_frame_preview.key))
            && self.ui_frame_preview.error.is_empty()
        {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Обновляем предпросмотр…",
                egui::FontId::proportional(13.0),
                Color32::GRAY,
            );
        }
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, LINE));
        if !excluded && !no_included_lines && !self.ui_frame_preview.error.is_empty() {
            ui.label(
                RichText::new(&self.ui_frame_preview.error)
                    .small()
                    .color(RED),
            );
        }
        if let Some(path) = self.generated_file.clone() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.is_generating,
                        egui::Button::new(if self.is_video_playing() {
                            "Пауза"
                        } else {
                            "Смотреть видео"
                        }),
                    )
                    .clicked()
                {
                    if self.is_video_playing() {
                        self.pause_video_preview();
                    } else if let Err(err) = self.start_video_preview(&path, ctx) {
                        self.video_status = err;
                    } else {
                        self.ui_frame_preview.show_video = true;
                    }
                }
                if ui.button("Оформление").clicked() {
                    self.ui_frame_preview.show_video = false;
                }
                if ui.button("Стоп").clicked() {
                    self.stop_video_preview();
                }
                if ui.button("Папка").clicked() {
                    open_in_explorer(Path::new(&path));
                }
                if ui.button("Сохранить как").clicked()
                    && let Some(dest) = rfd::FileDialog::new()
                        .set_file_name(lyrics::display_file_name(Path::new(&path)))
                        .add_filter("Видео", &["mp4"])
                        .save_file()
                    && let Err(error) = std::fs::copy(&path, dest)
                {
                    self.video_status = format!("Ошибка сохранения: {error}");
                }
            });
            ui.horizontal(|ui| {
                let mut position = self.video_position_ms;
                let response = ui.add(
                    egui::Slider::new(&mut position, 0..=self.video_duration_ms.max(1))
                        .show_value(false)
                        .text("Позиция"),
                );
                if response.changed() {
                    self.pause_video_preview();
                    self.video_position_ms = position;
                }
                caption(
                    ui,
                    format!(
                        "{} / {}",
                        ffmpeg::format_time_ms(self.video_position_ms),
                        ffmpeg::format_time_ms(self.video_duration_ms)
                    ),
                );
            });
            if self
                .generated_video_trim_bounds()
                .is_some_and(|(_, _, _, trim)| trim)
                && ui
                    .add_enabled(
                        !self.is_generating,
                        egui::Button::new("Обрезать готовый MP4 по фрагменту"),
                    )
                    .clicked()
            {
                self.start_generated_video_trim(ctx.clone());
            }
            if !self.video_status.is_empty() {
                caption(ui, self.video_status.clone());
            }
        } else {
            caption(
                ui,
                "Размер и оформление как в MP4 · подсветка слов появится после генерации",
            );
        }
    }

    pub(crate) fn single_workspace_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let height = ui.available_height();
        let content_height = (height - 12.0).max(180.0);
        let (width, _) = crate::split_panel_widths(ui.available_width() - 22.0, 8.0, 0.5);
        ui.horizontal_top(|ui| {
            ui.add_space(6.0);
            ui.allocate_ui_with_layout(egui::vec2(width,content_height),egui::Layout::top_down(egui::Align::Min),|ui| {
                egui::ScrollArea::vertical().id_salt("single_audio").max_height(content_height).auto_shrink([false,false]).show(ui,|ui| {
                    heading(ui,"Трек и фрагмент"); self.source_picker_ui(ui,ctx); ui.add_space(12.0);
                    self.audio_editor_ui(ui,ctx); ui.add_space(12.0);
                    card().show(ui,|ui|self.loudness_controls(ui));
                    if self.ui_batch_editing.is_some() { caption(ui,"Правки выбранного трека применяются кнопкой «Сохранить в пакет»."); }
                });
            });
            ui.add_space(8.0);
            ui.allocate_ui_with_layout(egui::vec2(width,content_height),egui::Layout::top_down(egui::Align::Min),|ui| {
                egui::ScrollArea::vertical().id_salt("single_text").max_height(content_height).auto_shrink([false,false]).show(ui,|ui| {
                    self.composition_preview_ui(ui,ctx); ui.add_space(10.0);
                    self.lyrics_editor_ui(ui,(content_height-300.0).clamp(130.0,260.0));
                    ui.separator();
                    ui.add_enabled_ui(!self.is_generating && !self.batch_running,|ui|self.render_settings_ui(ui,false));
                    if ui.button("Дополнительные настройки").clicked() { self.ui_advanced_open=true; }
                });
            });
        });
    }

    fn fragment_has_lyrics(&self) -> bool {
        let lines = editable_lines(&self.lyrics);
        let timed = lines.iter().any(|line| !line.times.is_empty());
        if timed && self.audio_duration_ms.is_some() {
            lines.iter().any(|line| {
                !line.text.trim().is_empty()
                    && !included_times(
                        line,
                        Some((self.trim_start_ms, self.trim_end_ms - self.trim_start_ms)),
                    )
                    .is_empty()
            })
        } else {
            !self.lyrics.trim().is_empty()
        }
    }

    pub(crate) fn single_footer_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal_top(|ui| {
            let ready = self.audio_path.is_some() && self.fragment_has_lyrics();
            let status_width = (ui.available_width() - 470.0).max(180.0);
            ui.allocate_ui_with_layout(
                egui::vec2(status_width, 48.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(if self.is_generating {
                                &self.status_text
                            } else if self.audio_path.is_some()
                                && !self.lyrics.trim().is_empty()
                                && !ready
                            {
                                "В выбранном фрагменте нет строк LRC"
                            } else if !ready {
                                "Добавьте аудио и текст песни"
                            } else if self.progress > 0.0 {
                                &self.status_text
                            } else {
                                "Готово к генерации"
                            })
                            .size(12.0)
                            .color(if ready { GREEN } else { MUTED }),
                        )
                        .truncate(),
                    );
                    if self.is_generating {
                        ui.add(
                            egui::ProgressBar::new(self.progress)
                                .desired_width(260.0)
                                .show_percentage(),
                        );
                    } else {
                        caption(
                            ui,
                            format!(
                                "Фрагмент {:.1} с · {}",
                                (self.trim_end_ms - self.trim_start_ms).max(0) as f64 / 1000.0,
                                if self.normalize_audio {
                                    format!("громкость {} дБ", self.loudness_target_db)
                                } else {
                                    "исходная громкость".into()
                                }
                            ),
                        );
                    }
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 40.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if ui
                        .add_enabled(
                            ready && !self.is_generating && !self.batch_running,
                            action(if self.is_generating {
                                "Генерация…"
                            } else {
                                "Создать видео"
                            }),
                        )
                        .clicked()
                    {
                        self.start_generation(ctx.clone());
                    }
                    if self.ui_batch_editing.is_some()
                        && ui
                            .add_enabled(
                                !self.is_generating && !self.batch_running,
                                egui::Button::new("Сохранить в пакет"),
                            )
                            .clicked()
                    {
                        self.save_selected_edits();
                    }
                    if ui
                        .add_enabled(
                            !self.lyrics.trim().is_empty(),
                            egui::Button::new("Сохранить текст"),
                        )
                        .clicked()
                    {
                        let timed = editable_lines(&self.lyrics)
                            .iter()
                            .any(|l| !l.times.is_empty());
                        let ext = if timed { "lrc" } else { "txt" };
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name(format!(
                                "{}.{}",
                                if self.title.is_empty() {
                                    "lyrics"
                                } else {
                                    &self.title
                                },
                                ext
                            ))
                            .add_filter("Текст песни", &[ext])
                            .save_file()
                            && let Err(error) = cache::atomic_write(&path, self.lyrics.as_bytes())
                        {
                            self.log_output
                                .push_str(&format!("Ошибка сохранения текста: {error}\n"));
                            self.ui_logs_open = true;
                        }
                    }
                },
            );
        });
    }

    fn save_selected_edits(&mut self) {
        let Some(index) = self.ui_batch_editing else {
            return;
        };
        let Some(item) = self.batch_items.get_mut(index) else {
            return;
        };
        if self.audio_path.as_deref() != Some(item.audio_path.to_string_lossy().as_ref()) {
            self.batch_status_text = "Аудио изменено: откройте трек из очереди заново.".into();
            return;
        }
        item.lyrics_override = Some(self.lyrics.clone());
        item.artist = self.artist.clone();
        item.title = self.title.clone();
        item.trim_start_ms = self.trim_start_ms;
        item.trim_end_ms = self.trim_end_ms;
        item.fade_in_ms = self.fade_in_ms;
        item.fade_out_ms = self.fade_out_ms;
        item.status = if self.lyrics.trim().is_empty() {
            BatchStatus::MissingLyrics
        } else {
            BatchStatus::Ready
        };
        item.progress = 0.0;
        item.output_path = None;
        refresh_batch_item_audit(item, self.plain_lines);
        item.status = if self.lyrics.trim().is_empty() {
            BatchStatus::MissingLyrics
        } else {
            BatchStatus::Ready
        };
        item.output_path = None;
        self.batch_status_text = "Правки трека сохранены. Можно продолжить пакет.".into();
        self.ui_batch_editing = None;
        self.active_tab = ActiveTab::Batch;
        self.persist_state();
    }

    fn save_project_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("karaoke-project.json")
            .add_filter("Караоке-проект", &["json"])
            .save_file()
        {
            let project = batch_project::Project::new(
                self.batch_root.clone(),
                self.batch_items.clone(),
                self.settings_snapshot(),
            );
            self.batch_status_text = match project
                .bytes()
                .and_then(|bytes| cache::atomic_write(&path, &bytes))
            {
                Ok(()) => "Проект сохранён".into(),
                Err(error) => format!("Не удалось сохранить проект: {error}"),
            };
        }
    }

    pub(crate) fn batch_workspace_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.batch_selected_index.is_none()
            && !self.batch_items.is_empty()
            && !self.is_generating
            && self.apply_batch_item_to_single_state(0).is_ok()
        {
            self.batch_selected_index = Some(0);
        }
        let busy = self.batch_running || self.is_generating || self.batch_is_scanning;
        ui.horizontal(|ui| {
            heading(
                ui,
                &format!(
                    "{} / {} треков",
                    self.batch_root
                        .as_ref()
                        .map(|p| lyrics::display_file_name(p))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Пакет".into()),
                    self.batch_items.len()
                ),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("Проект", |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Открыть проект"))
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Караоке-проект", &["json"])
                            .pick_file()
                        {
                            self.open_batch_project(&path);
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.batch_items.is_empty(),
                            egui::Button::new("Сохранить проект"),
                        )
                        .clicked()
                    {
                        ui.close_menu();
                        self.save_project_dialog();
                    }
                });
                if ui
                    .add_enabled(!busy, egui::Button::new("Добавить папку"))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    self.load_batch_folder(path, ctx);
                }
                if ui
                    .button(format!(
                        "{} · {} дБ",
                        quality_display_name(&self.quality),
                        self.loudness_target_db
                    ))
                    .clicked()
                {
                    self.ui_advanced_open = true;
                }
            });
        });
        ui.add_space(6.0);
        let content_height = (ui.available_height() - 8.0).max(180.0);
        let (table_width, inspector_width) =
            crate::split_panel_widths(ui.available_width(), 24.0, 0.5);
        let mut select = None;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(table_width, content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.horizontal(|ui| {
                        for (filter, label) in [
                            (BatchFilter::All, "Все"),
                            (BatchFilter::Done, "Готово"),
                            (BatchFilter::Review, "Проверить"),
                            (BatchFilter::Errors, "Ошибки"),
                        ] {
                            let n = self
                                .batch_items
                                .iter()
                                .filter(|item| matches_filter(item, filter))
                                .count();
                            ui.selectable_value(
                                &mut self.ui_batch_filter,
                                filter,
                                format!("{label} {n}"),
                            );
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ui_batch_search)
                            .hint_text("Найти трек или исполнителя")
                            .desired_width(f32::INFINITY),
                    );
                    let query = self.ui_batch_search.trim().to_lowercase();
                    let indices: Vec<usize> = self
                        .batch_items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            matches_filter(item, self.ui_batch_filter)
                                && (query.is_empty()
                                    || format!("{} {}", item.artist, item.title)
                                        .to_lowercase()
                                        .contains(&query))
                        })
                        .map(|(index, _)| index)
                        .collect();
                    let track_width = (table_width - 260.0).max(80.0);
                    let (header, _) =
                        ui.allocate_exact_size(egui::vec2(table_width, 24.0), egui::Sense::hover());
                    for (offset, label) in [
                        (4.0, "№"),
                        (40.0, "Трек"),
                        (40.0 + track_width + 10.0, "Фрагмент"),
                        (40.0 + track_width + 108.0, "Состояние"),
                    ] {
                        ui.painter().text(
                            header.min + egui::vec2(offset, 4.0),
                            egui::Align2::LEFT_TOP,
                            label,
                            egui::FontId::proportional(12.0),
                            MUTED,
                        );
                    }
                    ui.separator();
                    if indices.is_empty() {
                        caption(
                            ui,
                            if self.batch_items.is_empty() {
                                "Добавьте папку с MP3 и текстами песен или откройте проект."
                            } else {
                                "По этому фильтру треков нет."
                            },
                        );
                    }
                    ui.spacing_mut().item_spacing.y = 0.0;
                    egui::ScrollArea::vertical()
                        .id_salt("queue_table")
                        .auto_shrink([false, false])
                        .max_height((ui.available_height() - 26.0).max(80.0))
                        .show_rows(ui, 46.0, indices.len(), |ui, range| {
                            for row in range {
                                let index = indices[row];
                                let item = &self.batch_items[index];
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(table_width, 46.0),
                                    egui::Sense::click(),
                                );
                                if self.batch_selected_index == Some(index) {
                                    ui.painter().rect_filled(
                                        rect,
                                        4.0,
                                        Color32::from_rgb(32, 52, 79),
                                    );
                                } else if response.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        4.0,
                                        Color32::from_rgb(28, 41, 58),
                                    );
                                }
                                let p = ui.painter();
                                let top = rect.top() + 10.0;
                                p.text(
                                    egui::pos2(rect.left() + 4.0, top),
                                    egui::Align2::LEFT_TOP,
                                    format!("{:02}", index + 1),
                                    egui::FontId::proportional(11.0),
                                    MUTED,
                                );
                                let track_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect.left() + 40.0, top),
                                    egui::vec2(track_width, 36.0),
                                );
                                let clipped = p.with_clip_rect(track_rect);
                                clipped.text(
                                    track_rect.min,
                                    egui::Align2::LEFT_TOP,
                                    &item.title,
                                    egui::FontId::proportional(14.0),
                                    Color32::from_rgb(237, 242, 250),
                                );
                                clipped.text(
                                    track_rect.min + egui::vec2(0.0, 18.0),
                                    egui::Align2::LEFT_TOP,
                                    &item.artist,
                                    egui::FontId::proportional(12.0),
                                    MUTED,
                                );
                                p.text(
                                    egui::pos2(track_rect.right() + 10.0, top),
                                    egui::Align2::LEFT_TOP,
                                    format!(
                                        "{}\n{:.1} с",
                                        ffmpeg::format_time_ms(item.trim_start_ms),
                                        (item.trim_end_ms - item.trim_start_ms) as f64 / 1000.0
                                    ),
                                    egui::FontId::proportional(12.0),
                                    MUTED,
                                );
                                let (label, color) = status(item);
                                let status_x = track_rect.right() + 108.0;
                                let badge_rect = egui::Rect::from_min_size(
                                    egui::pos2(status_x, top),
                                    egui::vec2((rect.right() - status_x - 4.0).max(25.0), 27.0),
                                );
                                p.rect_filled(badge_rect, 5.0, color.gamma_multiply(0.16));
                                p.with_clip_rect(badge_rect).text(
                                    badge_rect.left_center() + egui::vec2(7.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    label,
                                    egui::FontId::proportional(12.0),
                                    color,
                                );
                                p.line_segment(
                                    [rect.left_bottom(), rect.right_bottom()],
                                    egui::Stroke::new(1.0, LINE),
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::SelectableLabel,
                                        true,
                                        format!("{} — {}", item.artist, item.title),
                                    )
                                });
                                if response.clicked() && !self.is_generating {
                                    select = Some(index);
                                }
                                if let BatchStatus::Error(error) = &item.status {
                                    response.on_hover_text(error);
                                } else if let Some(audit) = &item.audit {
                                    response.on_hover_text(audit.tooltip());
                                }
                            }
                        });
                    caption(
                        ui,
                        format!(
                            "Показано {} из {} · проект сохраняется автоматически",
                            indices.len(),
                            self.batch_items.len()
                        ),
                    );
                },
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(inspector_width, content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("queue_inspector")
                        .max_height(content_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.batch_inspector_ui(ui, ctx);
                        });
                },
            );
        });
        if let Some(index) = select {
            self.ui_batch_editing = None;
            match self.apply_batch_item_to_single_state(index) {
                Ok(()) => self.batch_selected_index = Some(index),
                Err(error) => self.batch_status_text = error,
            }
        }
    }

    fn batch_inspector_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(index) = self.batch_selected_index else {
            heading(ui, "Выбранный трек");
            caption(
                ui,
                "Нажмите строку в очереди, чтобы увидеть фрагмент и настройки.",
            );
            return;
        };
        let Some(item) = self.batch_items.get(index).cloned() else {
            return;
        };
        caption(
            ui,
            format!(
                "ВЫБРАННЫЙ ТРЕК · {} / {}",
                index + 1,
                self.batch_items.len()
            ),
        );
        heading(ui, &item.title);
        caption(ui, &item.artist);
        let busy = self.batch_running || self.is_generating;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Открыть в редакторе"))
                .clicked()
            {
                self.ui_batch_editing = Some(index);
                self.active_tab = ActiveTab::SingleTrack;
            }
            if ui
                .add_enabled(
                    !busy && self.fragment_has_lyrics(),
                    action("Создать выбранный"),
                )
                .clicked()
            {
                self.start_single_batch_item(index, ctx);
            }
        });
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!(
            "Фрагмент · {} — {} · {:.1} с",
            ffmpeg::format_time_ms(self.trim_start_ms),
            ffmpeg::format_time_ms(self.trim_end_ms),
            (self.trim_end_ms - self.trim_start_ms) as f64 / 1000.0
        ))
        .id_salt("batch_fragment_editor")
        .default_open(false)
        .show(ui, |ui| {
            if busy {
                batch_trim_timeline_static_ui(ui, &item, ACCENT, ACCENT, MUTED);
            } else {
                let before = (
                    self.trim_start_ms,
                    self.trim_end_ms,
                    self.fade_in_ms,
                    self.fade_out_ms,
                );
                self.audio_editor_ui(ui, ctx);
                if before
                    != (
                        self.trim_start_ms,
                        self.trim_end_ms,
                        self.fade_in_ms,
                        self.fade_out_ms,
                    )
                    && let Some(item) = self.batch_items.get_mut(index)
                {
                    item.trim_start_ms = self.trim_start_ms;
                    item.trim_end_ms = self.trim_end_ms;
                    item.fade_in_ms = self.fade_in_ms;
                    item.fade_out_ms = self.fade_out_ms;
                    item.status = if item.status == BatchStatus::MissingLyrics {
                        BatchStatus::MissingLyrics
                    } else {
                        BatchStatus::Ready
                    };
                    item.output_path = None;
                    item.progress = 0.0;
                    refresh_batch_item_audit(item, self.plain_lines);
                    item.status = if item.status == BatchStatus::MissingLyrics {
                        BatchStatus::MissingLyrics
                    } else {
                        BatchStatus::Ready
                    };
                    item.output_path = None;
                }
            }
        });
        ui.add_space(10.0);
        self.composition_preview_ui(ui, ctx);
        caption(ui, "Размер текста и оформление общие для пакета");
        ui.add_space(8.0);
        let text_changed = self.lyrics_editor_ui(ui, 160.0);
        if text_changed && !busy {
            if let Some(item) = self.batch_items.get_mut(index) {
                item.lyrics_override = Some(self.lyrics.clone());
                let status = if self.lyrics.trim().is_empty() {
                    BatchStatus::MissingLyrics
                } else {
                    BatchStatus::Ready
                };
                item.status = status.clone();
                item.output_path = None;
                item.progress = 0.0;
                item.alignment_quality = None;
                refresh_batch_item_audit(item, self.plain_lines);
                item.status = status;
                item.output_path = None;
            }
            self.generated_file = None;
            self.ui_frame_preview.show_video = false;
            self.batch_status_text = "Текст выбранного трека сохранён в проекте".into();
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Громкость пакета · {} дБ", self.loudness_target_db))
            .id_salt("batch_loudness_editor")
            .show(ui, |ui| self.loudness_controls(ui));
        ui.add_space(8.0);
        let (label, color) = status(&item);
        ui.label(RichText::new(label).color(color));
        if let Some(audit) = &item.audit {
            ui.label(
                RichText::new(format!("{} · {}", audit.level.label(), audit.summary))
                    .small()
                    .color(audit.level.color()),
            )
            .on_hover_text(audit.tooltip());
        }
        if let Some(quality) = &item.alignment_quality {
            ui.label(
                RichText::new(format!("{} {}", quality.icon(), quality.label()))
                    .small()
                    .color(quality.color()),
            )
            .on_hover_text(quality.tooltip());
            if let Some(report) = &quality.report_path
                && ui.button("Отчёт синхронизации").clicked()
            {
                open_path(Path::new(report));
            }
        }
        if let BatchStatus::Error(error) = &item.status {
            ui.label(RichText::new(error).small().color(RED));
        }
        if let Some(path) = &item.output_path {
            ui.horizontal(|ui| {
                if ui.button("Открыть видео").clicked() {
                    open_path(Path::new(path));
                }
                if ui.button("Папка").clicked() {
                    open_in_explorer(Path::new(path));
                }
            });
        }
        ui.add_enabled_ui(!busy, |ui| {
            ui.menu_button("Действия с треком", |ui| {
                if ui.button("Открыть папку трека").clicked() {
                    open_in_explorer(&item.folder);
                    ui.close_menu();
                }
                if ui.button("Проверить файлы").clicked() {
                    if let Some(item) = self.batch_items.get_mut(index) {
                        refresh_batch_item_audit(item, self.plain_lines);
                    }
                    ui.close_menu();
                }
                if ui.button("Убрать из очереди").clicked() {
                    self.batch_items.remove(index);
                    self.batch_selected_index = None;
                    self.stop_preview();
                    ui.close_menu();
                }
            });
        });
    }

    pub(crate) fn batch_footer_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let total = self.batch_items.len();
        let done = self
            .batch_items
            .iter()
            .filter(|i| i.status == BatchStatus::Done)
            .count();
        ui.horizontal_top(|ui| {
            let width = (ui.available_width() - 330.0).max(120.0) / 2.0;
            for (stage, title) in [
                (BatchStatus::Aligning, "Синхронизация"),
                (BatchStatus::Rendering, "Рендер видео"),
            ] {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 48.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.label(RichText::new(title).size(12.0));
                        let current = self
                            .batch_items
                            .iter()
                            .enumerate()
                            .find(|(_, i)| i.status == stage);
                        let fraction = current.map(|(_, i)| i.progress).unwrap_or(if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        });
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(width)
                                .desired_height(5.0)
                                .animate(current.is_some()),
                        );
                        if let Some((index, item)) = current {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} / {} · {} · {:.0}%",
                                        index + 1,
                                        total,
                                        item.title,
                                        item.progress * 100.0
                                    ))
                                    .size(12.0)
                                    .color(MUTED),
                                )
                                .truncate(),
                            );
                        } else {
                            caption(ui, format!("{} / {} готово", done, total));
                        }
                    },
                );
            }
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 48.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if self.batch_running {
                        if ui
                            .button("Прервать")
                            .on_hover_text("Прервать текущие процессы немедленно")
                            .clicked()
                        {
                            self.cancel_batch_now();
                        }
                        if ui
                            .add_enabled(
                                !self.batch_stop_requested,
                                egui::Button::new(if self.batch_stop_requested {
                                    "Завершаем текущие…"
                                } else {
                                    "Остановить после текущих"
                                }),
                            )
                            .clicked()
                        {
                            self.request_stop_batch();
                        }
                    } else {
                        let ready = !self.batch_items.is_empty()
                            && !self.is_generating
                            && !self.batch_is_scanning;
                        if ui.add_enabled(ready, action("Продолжить пакет")).clicked()
                        {
                            self.continue_batch_queue(ctx);
                        }
                        ui.menu_button("Ещё", |ui| {
                            if ui
                                .add_enabled(ready, egui::Button::new("Начать весь пакет заново"))
                                .clicked()
                            {
                                self.start_batch_queue(ctx);
                                ui.close_menu();
                            }
                        });
                    }
                },
            );
        });
        ui.add(
            egui::Label::new(
                RichText::new(&self.batch_status_text)
                    .size(12.0)
                    .color(MUTED),
            )
            .truncate(),
        );
    }
}
fn time_drag(ms: &mut i64) -> egui::DragValue<'_> {
    egui::DragValue::new(ms)
        .speed(100)
        .range(0..=i64::MAX)
        .custom_formatter(|value, _| {
            format!(
                "{:02}:{:02}.{:03}",
                value as i64 / 60_000,
                value as i64 / 1000 % 60,
                value as i64 % 1000
            )
        })
        .custom_parser(|s| lyrics::parse_lrc_timestamp_ms(&s.replace(',', ".")).map(|ms| ms as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_keeps_multiple_timestamps_metadata_and_line_endings() {
        let source = "[ar:Исполнитель]\r\n[00:01.230][00:12.00]  Привет\r\n\r\n[00:14.5]Мир\n";
        let mut lines = editable_lines(source);
        assert_eq!(join_lines(&lines), source);
        lines[3].text = "Новый текст".into();
        assert_eq!(
            join_lines(&lines),
            "[ar:Исполнитель]\r\n[00:01.230][00:12.00]  Привет\r\n\r\n[00:14.5]Новый текст\n"
        );
    }
    #[test]
    fn preview_uses_all_timestamps_in_time_order() {
        let source = "[00:01.0][00:09.0]Повтор\n[00:05.0]Середина\n[ar:Автор]";
        assert_eq!(
            preview_lines(source, 6000),
            ("Середина".into(), "Повтор".into())
        );
        assert_eq!(preview_lines(source, 9000), ("Повтор".into(), "".into()));
    }
    #[test]
    fn plain_text_round_trip_and_preview() {
        for text in ["", "Первая\nВторая", "Строка\n", "[bad]Текст\r\n"] {
            assert_eq!(join_lines(&editable_lines(text)), text);
        }
        assert_eq!(
            preview_lines("Первая\nВторая", 0),
            ("Первая".into(), "Вторая".into())
        );
    }

    #[test]
    fn trim_markers_and_preview_exclude_the_same_rows_and_repeats() {
        let source = "[00:00.00]Previous\n[00:04.00][00:08.00]Kept\n[00:06.00]After";
        let lines = editable_lines(source);
        assert!(included_times(&lines[0], Some((2500, 3500))).is_empty());
        assert_eq!(included_times(&lines[1], Some((2500, 3500))), vec![4000]);
        assert!(included_times(&lines[2], Some((2500, 3500))).is_empty());
        let clipped = lyrics::shift_lrc_for_trim(source, 2500, 3500);
        let timings = preview_timings(&clipped, 3.5);
        assert!(!timings.contains("Previous"));
        assert!(!timings.contains("After"));
        let value: serde_json::Value = serde_json::from_str(&timings).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1);
        assert_eq!(value[0]["start"], 1.5);
        let frame = desktop_app::render::preview_frame(desktop_app::render::PreviewRequest {
            timings,
            focus_time: Some(1.5),
            duration: 3.5,
            position: 2.2,
            quality: "medium".into(),
            active: [0, 0, 0],
            inactive: [180, 185, 195],
            background: [255, 255, 255],
            inactive_opacity: 0.65,
            audio_delay: 0.0,
            font_scale: 1.0,
            engine: "frames".into(),
            ffmpeg: "ffmpeg".into(),
        })
        .unwrap();
        assert!(
            frame
                .enumerate_pixels()
                .filter(|(_, y, _)| *y < 72)
                .all(|(_, _, pixel)| pixel[0] == 255),
            "previous row must be empty after trim"
        );
        assert!(
            frame
                .enumerate_pixels()
                .any(|(_, y, pixel)| (72..152).contains(&y) && pixel[0] < 128)
        );
    }
}
