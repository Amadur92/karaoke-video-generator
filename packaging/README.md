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
KaraokeGenerator-macOS-Intel-x64.tar.gz
KaraokeGenerator-macOS-AppleSilicon-arm64.tar.gz
```

Run:

```bash
./packaging/dist/KaraokeGenerator-macos/Karaoke\ Generator
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

This is the file to give to Windows users. It contains the app, worker,
FFmpeg, FFprobe, and bundled fonts.

## Notes

The first packaging target is a portable folder. Turning it into `.dmg`,
`.app`, or a signed Windows installer should be the next layer after this
folder build is stable.
