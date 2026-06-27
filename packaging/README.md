# Karaoke Generator Packaging

This folder builds a distributable desktop bundle where end users do not need
Python, pip packages, or a system FFmpeg installation.

The shipped app contains:

- Rust GUI (`desktop_app`)
- Python worker compiled with PyInstaller (`worker/karaoke_worker`)
- `ffmpeg` and `ffprobe`
- Montserrat font files

Whisper models are intentionally not bundled. They are downloaded by
`stable_whisper` on first use into the user's normal model cache.

## macOS

Prerequisites on the build machine:

- Rust toolchain
- Python 3.11 or 3.12
- FFmpeg available in `PATH`

Build:

```bash
./packaging/macos/build_box.sh
```

Output:

```text
packaging/dist/KaraokeGenerator-macos/
```

GitHub Actions release artifact names:

```text
KaraokeGenerator-macOS-Intel-x64-app.dmg
KaraokeGenerator-macOS-AppleSilicon-arm64-app.dmg
KaraokeGenerator-macOS-Intel-x64-portable.tar.gz
KaraokeGenerator-macOS-AppleSilicon-arm64-portable.tar.gz
```

Give normal macOS users the `.app.dmg` files. They contain a single
`Karaoke Generator.app` bundle with the PyInstaller worker, its own Python
runtime, FFmpeg, FFprobe, and bundled fonts inside
`Contents/Resources`, so users cannot easily separate the executable from its
runtime files. The portable `.tar.gz` files are kept as a fallback/debug
distribution.

Run:

```bash
./packaging/dist/KaraokeGenerator-macos/Karaoke\ Generator
```

Local `.app` DMG build:

```bash
./packaging/macos/build_app_dmg.sh
```

## Windows

Build on Windows, not cross-compiled from macOS:

```powershell
powershell -ExecutionPolicy Bypass -File .\packaging\windows\build_box.ps1
```

Output:

```text
packaging\dist\KaraokeGenerator-windows\
```

GitHub Actions release artifact name:

```text
KaraokeGenerator-Windows-x64-portable.zip
```

This is the file to give to Windows users. It contains the app, PyInstaller
worker with its own Python runtime, FFmpeg, FFprobe, and bundled fonts.

## Notes

The first packaging target is still a portable folder because it is useful for
debugging. The macOS user-facing layer is the `.app.dmg` wrapper built from
that folder.
