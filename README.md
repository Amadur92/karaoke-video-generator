# Karaoke Video Generator

Desktop app for creating karaoke MP4 videos from an MP3 track and lyrics.
It uses Whisper-based word timing, renders per-word highlighting, and ships as
portable builds for Windows and macOS.

## What It Does

- Imports `.mp3` audio.
- Lets you trim the beginning and end before generation.
- Plays audio previews inside the app.
- Generates karaoke video with word-by-word highlighting.
- Lets you choose colors, font, quality, and audio delay.
- Shows the actual video layout before rendering, with clickable lyric rows and adjustable text size. The batch editor includes the same preview and lyric editing controls.
- Grays out timestamped LRC rows outside the selected audio fragment and excludes them from the exported MP4, including the previous line before the fragment. Plain lyrics without timestamps need manual trimming.
- Normalizes music loudness per track using ReplayGain (89 dB by default, adjustable from 75 to 100 dB) for single and batch video generation. A constant gain preserves dynamics; a 1 dB sample-peak margin limits amplification before AAC encoding.
- Prepares audio and video preview sound in the background, caches decoded PCM, and seeks directly without extracting the remaining track on each playback. Trim timelines show actual peak bins; an optional normalized preview displays the applied gain and peak limitation.
- Saves batch projects automatically and supports explicit JSON project save/open, including trim ranges, fades, rendering settings and completed outputs. Running tasks return to the queue after restart.
- Runs one persistent alignment worker alongside one renderer. Graceful stop finishes the current operations; immediate cancellation terminates their process trees even when they produce no logs.
- Validates temporary MP4 files before replacing an earlier export. Timing, vocal and output caches use content fingerprints and settings to reject stale artifacts. FFmpeg capabilities select ASS when available and fall back to the frame renderer otherwise.

- Verifies the completed MP4 by probing and fully decoding it before publishing.
- Plays the generated video inside the app with play, pause, stop, seek, and time controls.
- Saves settings between launches.

## Download For Users

Open the latest GitHub Release and download the build for your system.

Use these files:

| System | Download |
| --- | --- |
| Windows x64 | `KaraokeGenerator-Windows-x64-portable.zip` |
| macOS Apple Silicon | `KaraokeGenerator-macOS-AppleSilicon-arm64-app.dmg` |
| macOS Intel | `KaraokeGenerator-macOS-Intel-x64-app.dmg` |

The DMG and portable archives contain the worker runtime and rendering tools.
macOS portable `.tar.gz` archives are also available as a fallback.

## What Is Included

Portable builds include:

- Rust desktop app.
- Python worker compiled with PyInstaller, including its own Python runtime.
- FFmpeg and FFprobe.
- Montserrat fonts.

Users do not need to install Python, pip packages, FFmpeg, or fonts.

Whisper model weights are intentionally not bundled. They are downloaded on the
first generation into the user's normal model cache. The first run can therefore
take longer and requires internet access.

## Windows Instructions

1. Download `KaraokeGenerator-Windows-x64-portable.zip`.
2. Unzip it.
3. Open the extracted folder.
4. Run `Karaoke Generator.exe`.

If Windows SmartScreen appears:

1. Click `More info`.
2. Click `Run anyway`.

## macOS Instructions

1. Download the correct DMG:
   - Apple Silicon: `KaraokeGenerator-macOS-AppleSilicon-arm64-app.dmg`
   - Intel: `KaraokeGenerator-macOS-Intel-x64-app.dmg`
2. Open the DMG.
3. Copy `Karaoke Generator.app` to Applications.
4. Run `Karaoke Generator.app`.

If macOS blocks the app because it is unsigned:

1. Right-click `Karaoke Generator`.
2. Choose `Open`.
3. Confirm `Open` again.

If macOS still blocks it, run this in Terminal from the unpacked folder:

```bash
xattr -dr com.apple.quarantine "Karaoke Generator"
```

Then open the app again.

## Basic Workflow

1. Choose an `.mp3` file.
2. Adjust trim start/end if needed.
3. Preview the selected audio range.
4. Paste or edit lyrics.
5. Set artist, title, colors, font, and quality.
6. Adjust **Выравнивать громкость** if needed (default: 89 dB), then click generate. Audio previews can switch between the original and normalized level.
7. The MP4 is saved to the `exports` folder next to the app in portable builds.
8. Preview the MP4 inside the app.
9. Save a copy somewhere else if needed.

## Development

Requirements for local development:

- Rust toolchain.
- Python 3.11+.
- FFmpeg and FFprobe in `PATH`.

Build and run the desktop app:

```bash
cd desktop_app
cargo run --release
```

Run the complete local quality gate:

```bash
python3 worker/test_karaoke_logic.py
python3 worker/test_resolver_logic.py
python3 worker/test_runtime_integrity.py
cargo fmt --manifest-path desktop_app/Cargo.toml -- --check
cargo clippy --manifest-path desktop_app/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path desktop_app/Cargo.toml --all-targets
cargo test --manifest-path desktop_app/Cargo.toml --lib -- --ignored  # actual PCM/waveform check, requires FFmpeg
cargo build --manifest-path desktop_app/Cargo.toml --bin karaoke_render
python3 scripts/render_smoke.py \
  --renderer desktop_app/target/debug/karaoke_render \
  --ffmpeg "$(command -v ffmpeg)" \
  --ffprobe "$(command -v ffprobe)" \
  --engine frames
python3 scripts/loudness_smoke.py --renderer desktop_app/target/debug/karaoke_render
```

Both renderers normalize audio by default. CLI overrides:

```bash
# Change the target level (ReplayGain reference, not measured speaker SPL):
karaoke_render --timings timings.json --audio input.wav --output out.mp4 --loudness-target-db 92
# Keep the original audio level:
karaoke_render --timings timings.json --audio input.wav --output out.mp4 --no-normalize-audio
```

The Python worker accepts the same loudness flags. Peak protection can keep a
track below the requested target; the renderer logs when that happens. Source
files are unchanged and normalization does not affect lyric timing.

## Search And Verification Only

The resolver has a dry-run mode that searches public catalog/pages, scores
candidates, checks public URLs, and stops before requesting media URLs:

```bash
python3 -m worker.unified_resolver resolve \
  "Земфира Хочешь" \
  --title "Хочешь" \
  --artist "Земфира" \
  --duration 222
```

The report includes every provider, candidate score, rejection flags, public
URL status, and an explicit safe-mode boundary. Qobuz search is disabled unless
officially supplied `QOBUZ_APP_ID` and `QOBUZ_APP_SECRET` are present.

The desktop app exposes the same boundary in **Поиск аудио** →
**Найти и проверить — без загрузки**. It presents a verified best-match card
and scored alternatives; downloading remains a separate optional action.

In development mode the app runs `worker/karaoke_worker.py` from the repository.
In portable builds it runs the PyInstaller executable from the bundled `worker`
folder.

## Packaging

Windows portable build:

```powershell
powershell -ExecutionPolicy Bypass -File .\packaging\windows\build_box.ps1
```

macOS portable build:

```bash
./packaging/macos/build_box.sh
```

GitHub Actions builds release-ready portable artifacts for Windows, macOS Intel,
and macOS Apple Silicon.

Batch projects use a versioned JSON format. The last batch is restored on startup, but processing starts only when you choose Continue or Start again. Continue verifies the timing report and output fingerprints; older artifacts without provenance are recalculated once. Opening a saved project also restores its rendering settings. The default preview remains at the original level; enable **Прослушивать с выравниванием** and restart playback to compare the selected loudness target.

Preview WAV files are limited to approximately 512 MiB on disk (the current file is retained), while the UI stores only peak bins. Application settings and project files are written through temporary files. Set `KARAOKE_APP_DATA_DIR` to isolate application data and settings for tests or a portable configuration. CI runs the logic checks, both rendering engines and real loudness checks on every relevant push and pull request, as well as before packaging.
