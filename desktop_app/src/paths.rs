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
    clear_bundled_runtime_quarantine();

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
            let worker = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            prepare_worker_runtime(&worker);
            return Some(worker);
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
    let worker_dirs = [
        base.join("worker"),
        base.join("../Resources/worker"),
        base.join("../../../worker"),
    ];
    for worker_dir in worker_dirs {
        clear_quarantine(&worker_dir);
        repair_python_shared_library(&worker_dir);
        repair_macos_codesign(&worker_dir);
    }
}

/// Готовит runtime рядом с конкретным worker-бинарником перед запуском.
/// Это держит защиту централизованной для parse-sheet, download, batch и генерации.
pub fn prepare_worker_runtime(worker_path: &Path) {
    if is_python_worker(worker_path) {
        return;
    }
    if let Some(worker_dir) = worker_path.parent() {
        clear_quarantine(worker_dir);
        repair_python_shared_library(worker_dir);
        repair_macos_codesign(worker_dir);
    }
}

/// Восстанавливает worker/_internal/Python из bundled Python.framework.
/// Безопасно для dev-режима (там worker — это .py скрипт, папки _internal нет).
#[cfg(target_os = "macos")]
fn repair_python_shared_library(worker_dir: &Path) {
    // Самовосстановление worker/_internal/Python: PyInstaller кладёт туда
    // симлинк на Python.framework/Versions/3.x/Python. Симлинк хрупок и ломается
    // при пересылке бокса через zip / Finder-копию / FAT-диск / облако, после
    // чего воркер падает с "Failed to load Python shared library: no such file".
    // Если файла нет или это сломанный симлинк — восстанавливаем из framework,
    // который лежит рядом и пересылку переживает.
    let internal = worker_dir.join("_internal");
    let py_link = internal.join("Python");

    // Если Python уже существует как обычный читаемый файл — чинить нечего.
    let needs_repair = if let Ok(meta) = std::fs::symlink_metadata(&py_link) {
        if meta.is_symlink() {
            // Симлинк (должен был быть заменён ещё при сборке, но перепроверим).
            std::fs::metadata(&py_link).is_err()
        } else {
            // Обычный файл: проверим, что он не нулевого размера (битые архивы
            // иногда оставляют пустышку).
            meta.len() == 0
        }
    } else {
        // Файла/ссылки нет совсем.
        true
    };

    if !needs_repair {
        return;
    }

    // Ищем настоящий Python внутри Python.framework в нескольких вариантах
    // расположения (PyInstaller может класть framework в _internal или рядом).
    let mut search_dirs = vec![internal.clone(), worker_dir.to_path_buf()];
    // Сканируем поддиректории _internal на случай вложенного framework.
    if let Ok(entries) = std::fs::read_dir(&internal) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                search_dirs.push(p);
            }
        }
    }

    for dir in &search_dirs {
        let framework_dir = dir.join("Python.framework");
        if !framework_dir.is_dir() {
            continue;
        }
        // Framework: .../Python.framework/Versions/<ver>/Python
        let versions = framework_dir.join("Versions");
        let version_dirs = std::fs::read_dir(&versions).into_iter().flatten();
        for vent in version_dirs.flatten() {
            let real = vent.path().join("Python");
            if real.is_file() {
                if let Err(e) = std::fs::copy(&real, &py_link) {
                    debug_log(format!(
                        "repair_python: copy failed from {}: {}",
                        real.display(),
                        e
                    ));
                } else {
                    let _ = std::fs::set_permissions(
                        &py_link,
                        std::os::unix::fs::PermissionsExt::from_mode(0o755),
                    );
                    debug_log(format!("repair_python: restored from {}", real.display()));
                }
                return;
            }
        }
    }

    debug_log(format!(
        "repair_python: Python.framework not found in {}",
        worker_dir.display()
    ));
}

#[cfg(not(target_os = "macos"))]
fn repair_python_shared_library(_worker_dir: &Path) {}

#[cfg(target_os = "macos")]
fn codesign_verify(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    std::process::Command::new("codesign")
        .args(["-v"])
        .arg(path)
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
fn codesign_ad_hoc(path: &Path, deep: bool) {
    if !path.exists() || codesign_verify(path) {
        return;
    }

    let mut cmd = std::process::Command::new("codesign");
    cmd.args(["--force", "--sign", "-"]);
    if deep {
        cmd.arg("--deep");
    }
    let status = cmd.arg(path).status();
    debug_log(format!(
        "codesign repair: {} deep={} status={:?}",
        path.display(),
        deep,
        status
    ));
}

#[cfg(target_os = "macos")]
fn should_codesign_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
    if name == "Python" || name == "karaoke_worker" || name == "karaoke_render" {
        return true;
    }
    let ext = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    matches!(ext, "dylib" | "so")
}

#[cfg(target_os = "macos")]
fn repair_macos_codesign(worker_dir: &Path) {
    if !worker_dir.exists() {
        return;
    }

    codesign_ad_hoc(&worker_dir.join("karaoke_worker"), true);
    codesign_ad_hoc(&worker_dir.join("karaoke_render"), true);
    codesign_ad_hoc(&worker_dir.join("_internal").join("Python"), false);

    let internal = worker_dir.join("_internal");
    if !internal.is_dir() {
        return;
    }

    let mut stack = vec![internal];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if should_codesign_file(&path) {
                codesign_ad_hoc(&path, false);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn repair_macos_codesign(_worker_dir: &Path) {}

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
