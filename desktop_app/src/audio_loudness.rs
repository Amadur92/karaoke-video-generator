//! ReplayGain track normalization: one constant gain, with 1 dB peak headroom
//! before AAC encoding. The scanner and output use the same stereo conversion.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};

pub const DEFAULT_TARGET_DB: i32 = 89;
pub const MIN_TARGET_DB: i32 = 75;
pub const MAX_TARGET_DB: i32 = 100;

pub fn validate_target(target: i32) -> Result<(), String> {
    if (MIN_TARGET_DB..=MAX_TARGET_DB).contains(&target) {
        Ok(())
    } else {
        Err(format!(
            "Громкость должна быть от {MIN_TARGET_DB} до {MAX_TARGET_DB} дБ"
        ))
    }
}

fn gain_from_report(report: &str, target: i32) -> Result<(f64, f64), String> {
    validate_target(target)?;
    let read = |key: &str| {
        report.lines().rev().find_map(|line| {
            line.split_once(key)?
                .1
                .split_whitespace()
                .next()?
                .parse::<f64>()
                .ok()
        })
    };
    let gain = read("track_gain =")
        .filter(|value| value.is_finite())
        .ok_or("FFmpeg не сообщил ReplayGain track_gain")?;
    let peak = read("track_peak =")
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or("FFmpeg не сообщил ReplayGain track_peak")?;
    if peak == 0.0 {
        return Ok((0.0, 0.0)); // Silence must remain silence.
    }
    let requested = gain + f64::from(target - DEFAULT_TARGET_DB);
    let safe = -1.0 - 20.0 * peak.log10();
    Ok((requested, requested.min(safe)))
}

pub struct NormalizationPlan {
    pub requested_db: f64,
    pub applied_db: f64,
}

pub fn normalization_filter(ffmpeg: &Path, audio: &Path, target: i32) -> Result<String, String> {
    let plan = normalization_plan(ffmpeg, audio, target)?;
    Ok(format!(
        "aformat=channel_layouts=stereo,volume={:.6}dB:replaygain=drop",
        plan.applied_db
    ))
}

pub fn normalization_plan(
    ffmpeg: &Path,
    audio: &Path,
    target: i32,
) -> Result<NormalizationPlan, String> {
    normalization_plan_cancellable(ffmpeg, audio, target, Arc::new(AtomicBool::new(false)))
}

pub fn normalization_plan_cancellable(
    ffmpeg: &Path,
    audio: &Path,
    target: i32,
    cancelled: Arc<AtomicBool>,
) -> Result<NormalizationPlan, String> {
    validate_target(target)?;
    eprintln!("[Audio] Анализ громкости: цель {target} дБ ReplayGain...");
    let executable = resolved_executable(ffmpeg);
    let tool_metadata = std::fs::metadata(&executable)
        .ok()
        .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()));
    let key = crate::cache::bytes_sha256(
        format!(
            "replaygain-stereo-v1:{}:{}:{tool_metadata:?}",
            crate::cache::file_sha256_cancellable(audio, &cancelled)?,
            executable.display()
        )
        .as_bytes(),
    );
    let cache_path = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("karaoke-generator/loudness")
        .join(format!("{key}.txt"));
    if let Ok(report) = std::fs::read_to_string(&cache_path)
        && let Ok((requested_db, applied_db)) = gain_from_report(&report, target)
    {
        eprintln!("[Audio] Используем сохранённый анализ громкости.");
        log_gain(requested_db, applied_db);
        return Ok(NormalizationPlan {
            requested_db,
            applied_db,
        });
    }
    let mut cmd = Command::new(ffmpeg);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.args([
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "info",
        "-i",
    ])
    .arg(audio)
    .args([
        "-map",
        "0:a:0",
        "-af",
        "aformat=channel_layouts=stereo,replaygain",
        "-f",
        "null",
        "-",
    ]);
    let mut lines = std::collections::VecDeque::new();
    let success = crate::process_runner::run(cmd, cancelled.clone(), |_, line| {
        if lines.len() == 12 {
            lines.pop_front();
        }
        lines.push_back(line.to_string());
    })
    .map_err(|error| format!("Не удалось запустить анализ громкости: {error}"))?;
    let report = lines.into_iter().collect::<Vec<_>>().join("\n");
    if !success {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Анализ громкости отменён".into());
        }
        let tail = report
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("Анализ громкости не удался:\n{tail}"));
    }
    let (requested, applied) = gain_from_report(&report, target)?;
    log_gain(requested, applied);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let compact_report = report
        .lines()
        .filter(|line| line.contains("track_gain =") || line.contains("track_peak ="))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = crate::cache::atomic_write(&cache_path, compact_report.as_bytes());
    Ok(NormalizationPlan {
        requested_db: requested,
        applied_db: applied,
    })
}

fn log_gain(requested: f64, applied: f64) {
    eprintln!("[Audio] Поправка громкости: {applied:+.2} дБ (расчёт {requested:+.2} дБ)");
    if applied < requested - 0.01 {
        eprintln!("[Audio] Усиление ограничено пиками; трек останется тише цели.");
    }
}

fn resolved_executable(path: &Path) -> std::path::PathBuf {
    if path.is_file() {
        return std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|root| root.join(path))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_offset_and_peak_headroom() {
        let report = "[replaygain] track_gain = -6.00 dB\n[replaygain] track_peak = 0.8\n";
        assert_eq!(gain_from_report(report, 89).unwrap(), (-6.0, -6.0));
        assert_eq!(gain_from_report(report, 92).unwrap(), (-3.0, -3.0));
        let (requested, applied) = gain_from_report(report, 100).unwrap();
        assert_eq!(requested, 5.0);
        assert!((0.8 * 10.0_f64.powf(applied / 20.0) - 10.0_f64.powf(-1.0 / 20.0)).abs() < 1e-10);
    }

    #[test]
    fn silence_and_invalid_reports() {
        assert_eq!(
            gain_from_report("track_gain = 64 dB\ntrack_peak = 0", 89).unwrap(),
            (0.0, 0.0)
        );
        for report in [
            "",
            "track_gain = NaN\ntrack_peak = 1",
            "track_gain = 2\ntrack_peak = -1",
        ] {
            assert!(gain_from_report(report, 89).is_err());
        }
        assert!(validate_target(74).is_err());
        assert!(validate_target(101).is_err());
    }
}
