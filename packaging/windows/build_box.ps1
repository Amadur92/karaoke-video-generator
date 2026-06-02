param(
    [string]$Python = "py",
    [string]$FfmpegDir = ""
)

$ErrorActionPreference = "Stop"

$Root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$Venv = Join-Path $Root ".packaging-venv-win"
$WorkerDir = Join-Path $Root "worker"
$BuildDir = Join-Path $Root "packaging\build\windows-worker"
$BoxDir = Join-Path $Root "packaging\dist\KaraokeGenerator-windows"

if (!(Test-Path $Venv)) {
    & $Python -m venv $Venv
}

$Py = Join-Path $Venv "Scripts\python.exe"
$PyInstaller = Join-Path $Venv "Scripts\pyinstaller.exe"

& $Py -m pip install --upgrade pip wheel "setuptools<82"
& $Py -m pip install -r (Join-Path $WorkerDir "requirements.txt")

if (Test-Path $BuildDir) {
    Remove-Item $BuildDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null

& $PyInstaller `
    --noconfirm `
    --clean `
    --onedir `
    --name karaoke_worker `
    --distpath (Join-Path $BuildDir "dist") `
    --workpath (Join-Path $BuildDir "build") `
    --specpath $BuildDir `
    --collect-data whisper `
    --add-data "$Root\desktop_app\assets\Montserrat-Regular.ttf;." `
    --add-data "$Root\desktop_app\assets\Montserrat-Bold.ttf;." `
    (Join-Path $WorkerDir "karaoke_worker.py")

Push-Location (Join-Path $Root "desktop_app")
cargo build --release
$metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
$TargetDir = $metadata.target_directory
Pop-Location

if (Test-Path $BoxDir) {
    Remove-Item $BoxDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Join-Path $BoxDir "worker") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $BoxDir "bin") | Out-Null

Copy-Item (Join-Path $TargetDir "release\desktop_app.exe") (Join-Path $BoxDir "Karaoke Generator.exe")
Copy-Item (Join-Path $TargetDir "release\karaoke_render.exe") (Join-Path $BoxDir "worker\karaoke_render.exe")
Copy-Item (Join-Path $BuildDir "dist\karaoke_worker\*") (Join-Path $BoxDir "worker") -Recurse

if ($FfmpegDir -eq "") {
    $ffmpeg = Get-Command ffmpeg.exe -ErrorAction SilentlyContinue
    $ffprobe = Get-Command ffprobe.exe -ErrorAction SilentlyContinue
    if ($null -eq $ffmpeg -or $null -eq $ffprobe) {
        throw "Pass -FfmpegDir C:\path\to\ffmpeg\bin or add ffmpeg.exe and ffprobe.exe to PATH."
    }
    Copy-Item $ffmpeg.Source (Join-Path $BoxDir "bin\ffmpeg.exe")
    Copy-Item $ffprobe.Source (Join-Path $BoxDir "bin\ffprobe.exe")
} else {
    Copy-Item (Join-Path $FfmpegDir "ffmpeg.exe") (Join-Path $BoxDir "bin\ffmpeg.exe")
    Copy-Item (Join-Path $FfmpegDir "ffprobe.exe") (Join-Path $BoxDir "bin\ffprobe.exe")
}

@"
Karaoke Generator

Run:
  Karaoke Generator.exe

The first generation can take longer because the selected Whisper model is
downloaded into your user cache.
"@ | Set-Content -Encoding UTF8 (Join-Path $BoxDir "README.txt")

Write-Host "Box created: $BoxDir"
