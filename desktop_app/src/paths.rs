//! Поиск bundled-инструментов (worker, рендерер, ffmpeg/ffprobe) и вычисление
//! путей к рабочим папкам приложения (настройки, временные файлы, экспорт).
//!
//! В portable-сборке всё лежит рядом с исполняемым файлом; в dev-режиме пути
//! берутся из стандартных config/data директорий ОС.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Вычисляет рабочую директорию рядом с исполняемым файлом
pub fn app_base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("karaoke-generator");
    let _ = std::fs::create_dir_all(&p);
    p.push("settings.json");
    p
}

pub fn app_data_dir() -> PathBuf {
    let mut p = dirs::data_dir()
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    p.push("karaoke-generator");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn debug_log(message: impl AsRef<str>) {
    let path = app_data_dir().join("karaoke_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", message.as_ref());
    }
}

fn executable_name(base_name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.exe", base_name)
    } else {
        base_name.to_string()
    }
}

/// Ищет bundled worker рядом с приложением или Python-скрипт в dev-режиме.
pub fn find_worker() -> Option<PathBuf> {
    let base = app_base_dir();
    let worker_exe = executable_name("karaoke_worker");
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../worker/karaoke_worker.py"),
        base.join(&worker_exe),
        base.join("worker").join(&worker_exe),
        base.join("../Resources/worker").join(&worker_exe),
        base.join("karaoke_worker.py"),
        base.join("worker/karaoke_worker.py"),
        base.join("../../../worker/karaoke_worker.py"),
        base.join("../../../../worker/karaoke_worker.py"),
        PathBuf::from("/Users/mihailsokolenko/wow_quiz/worker/karaoke_worker.py"),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

pub fn find_rust_renderer() -> Option<PathBuf> {
    let base = app_base_dir();
    let renderer_exe = executable_name("karaoke_render");
    let candidates = [
        base.join(&renderer_exe),
        base.join("worker").join(&renderer_exe),
        base.join("../Resources/worker").join(&renderer_exe),
        base.join("../release").join(&renderer_exe),
        base.join("../../../target/release").join(&renderer_exe),
        base.join("../../../desktop_app/target/release")
            .join(&renderer_exe),
        base.join("../../../target/debug").join(&renderer_exe),
        base.join("../../../desktop_app/target/debug")
            .join(&renderer_exe),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

pub fn is_python_worker(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("py"))
        .unwrap_or(false)
}

pub fn bundled_bin_dir() -> Option<PathBuf> {
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

pub fn tool_path(base_name: &str) -> PathBuf {
    let name = tool_name(base_name);
    bundled_bin_dir()
        .map(|dir| dir.join(&name))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) {
    if path.exists() {
        let _ = std::process::Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(path)
            .status();
    }
}

#[cfg(not(target_os = "macos"))]
fn clear_quarantine(_path: &Path) {}

pub fn clear_bundled_runtime_quarantine() {
    if let Some(bin_dir) = bundled_bin_dir() {
        clear_quarantine(&bin_dir);
    }

    let base = app_base_dir();
    clear_quarantine(&base.join("worker"));
}

pub fn exports_dir() -> PathBuf {
    let base = app_base_dir();
    let portable_exports = base.join("exports");
    let exports = if base.join("worker").exists() && base.join("bin").exists() {
        portable_exports
    } else {
        app_data_dir().join("exports")
    };
    let _ = std::fs::create_dir_all(&exports);
    exports
}

/// Путь к папке для временных файлов
pub fn temp_dir() -> PathBuf {
    let temp = app_data_dir().join("tmp");
    let _ = std::fs::create_dir_all(&temp);
    temp
}

pub fn upload_dir() -> PathBuf {
    let uploads = app_data_dir().join("uploads");
    let _ = std::fs::create_dir_all(&uploads);
    uploads
}
