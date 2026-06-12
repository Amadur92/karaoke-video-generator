# Deployment Guide

Подробный гайд по тому, как устроена сборка, локальная проверка, GitHub Actions и выкладка релизных файлов для Karaoke Video Generator.

## Что деплоится

Проект состоит из нескольких рабочих частей:

- `desktop_app/` - Rust-десктоп приложение. Оно показывает UI, принимает аудио, текст и настройки, запускает worker, запускает рендерер и показывает готовый MP4.
- `worker/karaoke_worker.py` - Python-worker. Он готовит аудио, запускает Whisper, строит тайминги слов/строк и при необходимости может работать как fallback-рендерер.
- `desktop_app/src/bin/karaoke_render.rs` - Rust-рендерер MP4. Это основной быстрый рендерер видео.
- `packaging/` - скрипты упаковки portable-коробок для macOS и Windows.
- `.github/workflows/` - GitHub Actions workflow, который собирает релизные архивы.

Пользователь в нормальном случае не должен ставить Python, Rust, Cargo, pip, ffmpeg или ffprobe. Все это должно быть внутри portable-архива. Модель Whisper может скачиваться отдельно при первом запуске.

## Локальный цикл разработки

Перейти в репозиторий:

```bash
cd /Users/mihailsokolenko/karaoke-video-generator
```

Посмотреть состояние:

```bash
git status --short
git branch --show-current
```

После правок прогнать базовые проверки:

```bash
python3 -m py_compile worker/karaoke_worker.py
cargo fmt --manifest-path desktop_app/Cargo.toml
cargo check --manifest-path desktop_app/Cargo.toml --bin desktop_app
cargo check --manifest-path desktop_app/Cargo.toml --bin karaoke_render
```

Запустить приложение локально:

```bash
cargo run --manifest-path desktop_app/Cargo.toml --bin desktop_app
```

Если уже висит старое debug-окно, закрыть его или убить процесс:

```bash
pkill -f 'desktop_app/target/debug/desktop_app' || true
cargo run --manifest-path desktop_app/Cargo.toml --bin desktop_app
```

## Как работает генерация видео

Основной flow:

1. Пользователь выбирает аудиофайл.
2. Приложение читает длительность через `ffprobe`.
3. Пользователь вводит или вставляет текст песни.
4. Rust-приложение запускает Python-worker.
5. Worker готовит аудио, загружает или скачивает Whisper-модель и строит тайминги.
6. Worker возвращает JSON с таймингами.
7. Rust-приложение запускает `karaoke_render`.
8. `karaoke_render` собирает MP4.
9. Приложение показывает предпросмотр и кнопку сохранения.

Обычный режим рендера:

- строки плавно прокручиваются;
- текущая строка крупнее и заметнее;
- соседние строки остаются видимыми;
- слова подсвечиваются заливкой по мере исполнения.

Режим без заливки слов:

- строки должны двигаться и выглядеть так же, как в обычном режиме;
- предыдущая, текущая и следующая строки остаются в той же композиции;
- отключается только word-fill, то есть постепенная заливка слов;
- текущая строка показывается целиком активным цветом.

## Версия приложения

Версия хранится минимум в двух местах:

```text
desktop_app/Cargo.toml
desktop_app/Cargo.lock
```

Пример:

```toml
version = "0.2.5"
```

Перед релизной сборкой версию надо поднимать. Иначе невозможно надежно понять, какая сборка стоит у пользователя.

Рекомендуемый порядок:

```text
0.2.5 -> 0.2.6 -> 0.2.7
```

После изменения версии надо прогнать:

```bash
cargo check --manifest-path desktop_app/Cargo.toml --bin desktop_app
cargo check --manifest-path desktop_app/Cargo.toml --bin karaoke_render
```

## Коммит и push

Стадить только нужные файлы. Не добавлять временные файлы, тестовые видео и `worker/web_exports/`.

Пример:

```bash
git add \
  desktop_app/Cargo.toml \
  desktop_app/Cargo.lock \
  desktop_app/src/main.rs \
  desktop_app/src/bin/karaoke_render.rs \
  worker/karaoke_worker.py

git commit -m "Add plain lyric line mode"
git push origin main
```

Проверить последний коммит:

```bash
git log -1 --oneline
```

## Запуск GitHub Actions

Workflow запускается вручную:

```bash
gh workflow run 284440734 \
  --repo Amadur92/karaoke-video-generator \
  --ref main
```

Посмотреть последние запуски:

```bash
gh run list \
  --repo Amadur92/karaoke-video-generator \
  --workflow 284440734 \
  --limit 5
```

Следить за конкретным запуском:

```bash
gh run watch RUN_ID \
  --repo Amadur92/karaoke-video-generator \
  --interval 30 \
  --exit-status
```

Пример:

```bash
gh run watch 27272831950 \
  --repo Amadur92/karaoke-video-generator \
  --interval 30 \
  --exit-status
```

## Что собирает GitHub Actions

Workflow должен собрать три portable-архива:

```text
KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz
KaraokeGenerator-macOS-Intel-x64-portable.tar.gz
KaraokeGenerator-Windows-x64-portable.zip
```

Назначение:

- `macOS-AppleSilicon-arm64` - Mac на M1, M2, M3, M4.
- `macOS-Intel-x64` - старые Intel Mac.
- `Windows-x64` - Windows x64.

Внутри portable-коробки должны быть:

- приложение;
- Python-worker;
- Python runtime или подготовленное worker-окружение;
- Python-зависимости;
- `ffmpeg`;
- `ffprobe`;
- шрифты и assets;
- Rust-рендерер.

## Скачивание artifacts

Удобно складывать artifacts на внешний диск, чтобы не забивать системный:

```bash
BASE="/Volumes/MIKE HDD 1/karaoke-release-artifacts/RUN_ID"
TMP="$BASE/tmp"
DL="$BASE/gh-download"

mkdir -p "$TMP" "$DL"
```

Обычный способ:

```bash
TMPDIR="$TMP" gh run download RUN_ID \
  --repo Amadur92/karaoke-video-generator \
  --dir "$DL"
```

Иногда `gh run download` долго молчит или зависает. Тогда artifacts лучше скачать через API.

Получить список artifact id:

```bash
gh api repos/Amadur92/karaoke-video-generator/actions/runs/RUN_ID/artifacts \
  --jq '.artifacts[] | [.id,.name,.size_in_bytes,.expired] | @tsv'
```

Скачать и распаковать каждый artifact:

```bash
BASE="/Volumes/MIKE HDD 1/karaoke-release-artifacts/RUN_ID"
ZIPDIR="$BASE/artifact-zips"
DL="$BASE/gh-download"

mkdir -p "$ZIPDIR" "$DL"

gh api "repos/Amadur92/karaoke-video-generator/actions/artifacts/ARTIFACT_ID/zip" > "$ZIPDIR/ARTIFACT_NAME.zip"
mkdir -p "$DL/ARTIFACT_NAME"
unzip -q -o "$ZIPDIR/ARTIFACT_NAME.zip" -d "$DL/ARTIFACT_NAME"
```

Проверить, что файлы реально есть:

```bash
find "$DL" -maxdepth 2 -type f -print -exec ls -lh {} \;
```

## Загрузка в GitHub Release

Текущий технический release-tag:

```text
dev
```

Загрузка трех актуальных portable-файлов поверх старых:

```bash
BASE="/Volumes/MIKE HDD 1/karaoke-release-artifacts/RUN_ID/gh-download"

gh release upload dev \
  "$BASE/KaraokeGenerator-macOS-AppleSilicon-arm64-portable/KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz" \
  "$BASE/KaraokeGenerator-macOS-Intel-x64-portable/KaraokeGenerator-macOS-Intel-x64-portable.tar.gz" \
  "$BASE/KaraokeGenerator-Windows-x64-portable/KaraokeGenerator-Windows-x64-portable.zip" \
  --repo Amadur92/karaoke-video-generator \
  --clobber
```

`--clobber` означает, что файл с таким именем в release будет заменен.

Проверить release после upload:

```bash
gh release view dev \
  --repo Amadur92/karaoke-video-generator \
  --json tagName,name,assets \
  --jq '{tag:.tagName,name:.name,assets:[.assets[]|{name:.name,size:.size,updatedAt:.updatedAt}]}'
```

Нужно проверить:

- имена файлов;
- размеры;
- `updatedAt`, чтобы убедиться, что assets действительно обновились.

## Что пользователь должен скачать

Для Mac на Apple Silicon:

```text
KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz
```

Для Intel Mac:

```text
KaraokeGenerator-macOS-Intel-x64-portable.tar.gz
```

Для Windows:

```text
KaraokeGenerator-Windows-x64-portable.zip
```

Старые `.dmg`, если они лежат в release, не считать основным вариантом, пока они отдельно не пересобраны и не проверены. Актуальный путь сейчас - portable `.tar.gz` и `.zip`.

## Частые проблемы у пользователей

### macOS пишет, что приложение повреждено

Обычно это quarantine/Gatekeeper, особенно если архив передавали через Telegram или распаковывали сторонним архиватором.

Проверочная команда:

```bash
xattr -dr com.apple.quarantine "$HOME/Downloads/KaraokeGenerator-macos"
```

Если есть `Permission denied` на `ffmpeg` или `ffprobe`, может понадобиться `sudo`, но лучше не делать это первым шагом без понимания, что именно распаковано.

### Ошибка AVFoundation на macOS Monterey

Пример симптома:

```text
expected in: /System/Library/Frameworks/AVFoundation.framework/Versions/A/AVFoundation
```

Это обычно совместимость `ffmpeg` с очень старой версией macOS. Например, на macOS Monterey 12.2.1 часть современных сборок ffmpeg может требовать системные символы, которых в этой версии еще нет.

Возможные решения:

- обновить macOS;
- положить в коробку ffmpeg, собранный с более старым deployment target;
- переключить такого пользователя на сборку/архив, где ffmpeg совместим с его macOS.

### Модель Whisper не скачалась

Модель должна скачиваться при первом использовании. Если этого не произошло, надо смотреть:

- логи worker;
- наличие интернета;
- папку кэша Whisper;
- выбранный размер модели.

### На Windows открывается терминал

В Rust-приложении используется:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

И для дочерних процессов должен использоваться `CREATE_NO_WINDOW`.

Если терминал все равно появляется, вероятные причины:

- пользователь запускает не тот `.exe`;
- запускается debug-сборка;
- один из дочерних процессов создается без скрытия окна.

### Пользователь скачал старую версию

Если версия не поднята, это почти невозможно быстро выяснить. Поэтому:

- поднимать версию при каждом релизе;
- показывать версию в UI;
- называть release понятно;
- не держать вечный `dev` как единственный источник правды.

## Что надо улучшить в релизном процессе

Текущий `dev` release удобен для быстрых тестов, но для нормальной раздачи лучше перейти на версионированные релизы:

```text
v0.2.6
v0.2.7
v0.2.8
```

Рекомендуемый порядок:

1. Внести изменения.
2. Поднять версию.
3. Прогнать локальные проверки.
4. Закоммитить.
5. Запушить `main`.
6. Запустить GitHub Actions.
7. Скачать artifacts.
8. Создать или обновить release.
9. Проверить assets.
10. Дать пользователю ссылку на конкретный файл под его ОС.

## Минимальный чеклист перед тем, как давать ссылку человеку

- [ ] `git status --short` не содержит случайного мусора в коммите.
- [ ] Версия поднята.
- [ ] `python3 -m py_compile worker/karaoke_worker.py` прошел.
- [ ] `cargo check --bin desktop_app` прошел.
- [ ] `cargo check --bin karaoke_render` прошел.
- [ ] GitHub Actions завершился зеленым.
- [ ] В release лежат свежие `macOS arm64`, `macOS Intel`, `Windows x64`.
- [ ] `updatedAt` у assets свежий.
- [ ] Название файла понятно по платформе.
- [ ] Если это macOS-пользователь, понятно, Apple Silicon у него или Intel.

