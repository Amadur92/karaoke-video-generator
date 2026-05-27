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
- Plays the generated video inside the app with play, pause, stop, seek, and time controls.
- Saves settings between launches.

## Download For Users

Open the latest GitHub Release and download the portable build for your system.

Use these files:

| System | Download |
| --- | --- |
| Windows x64 | `KaraokeGenerator-Windows-x64-portable.zip` |
| macOS Apple Silicon | `KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz` |
| macOS Intel | `KaraokeGenerator-macOS-Intel-x64-portable.tar.gz` |

Do not use the plain binary archives unless you are debugging. The portable
archives contain everything regular users need.

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

1. Download the correct archive:
   - Apple Silicon: `KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz`
   - Intel: `KaraokeGenerator-macOS-Intel-x64-portable.tar.gz`
2. Unpack it.
3. Open `KaraokeGenerator-macos`.
4. Run `Karaoke Generator`.

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
6. Click generate.
7. Preview the MP4 inside the app.
8. Save a copy where you want it.

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
