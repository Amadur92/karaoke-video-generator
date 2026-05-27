# 🎤 Караоке-Видео Генератор — Desktop App

Нативное настольное приложение на **Rust + egui** для генерации караоке-видео с пословной синхронизацией через Whisper AI.

## Возможности

- 🎵 **Drag & Drop** аудиофайлов (`.mp3`)
- ✂️ **Обрезка аудио по краям** перед генерацией
- ▶️ **Прослушивание обрезанного фрагмента** через системный плеер
- 🤖 Автоматическая **пословная транскрипция** через Stable Whisper
- 🎬 Генерация караоке-видео с подсветкой слов в реальном времени
- 🎨 Кастомизация цветов, шрифтов и качества видео
- 💾 **Автосохранение** всех настроек между сессиями
- 🖥️ Кроссплатформенность: **macOS, Windows, Linux**

## Коробочная сборка

Для пользователей собирается portable-папка: Rust GUI, bundled Python worker,
FFmpeg/FFprobe и шрифты. Python и FFmpeg на компьютере пользователя не нужны.
Модель Whisper скачивается при первом запуске выбранного режима.

macOS:

```bash
./packaging/macos/build_box.sh
```

Результат:

```text
packaging/dist/KaraokeGenerator-macos/
```

## Сборка из исходников для разработки

### Зависимости

- [Rust](https://www.rust-lang.org/tools/install) 1.85+
- Python 3.11+ для dev-запуска `worker/karaoke_worker.py`
- FFmpeg

### Компиляция

```bash
cd desktop_app
cargo build --release
```

Готовый бинарник будет в release-папке Cargo target directory.

### Запуск

В dev-режиме приложение ищет `worker/karaoke_worker.py` в корне репозитория.
В release-коробке оно запускает bundled executable `worker/karaoke_worker`.

```bash
cargo run --release
```

## Скриншоты

Современная тёмная рабочая тема, двухколоночный layout, встроенный прогресс-бар и просмотр логов.

## Технологии

| Компонент | Технология |
|-----------|-----------|
| GUI-фреймворк | [egui](https://github.com/emilk/egui) / [eframe](https://github.com/emilk/egui/tree/master/crates/eframe) |
| Диалоги файлов | [rfd](https://github.com/PolyMeilex/rfd) |
| Шрифт | Montserrat (вкомпилирован в бинарник) |
| Транскрипция | Stable Whisper (Python) |
| Видео | FFmpeg |

## Лицензия

MIT
