//! Disk-backed PCM previews: decode once, seek directly, keep only peak bins in memory.
use crate::{cache, process_runner};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Asset {
    pub path: PathBuf,
    pub identity: String,
    pub duration_ms: i64,
    pub peaks: Vec<f32>,
}

pub fn prepare(input: &Path, ffmpeg: &Path, cancelled: Arc<AtomicBool>) -> Result<Asset, String> {
    let identity = cache::file_sha256_cancellable(input, &cancelled)?;
    let root = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("karaoke-generator/preview-v1");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let path = root.join(format!("{identity}.wav"));
    let metadata = path.with_extension("json");
    if let Ok(data) = fs::read(&metadata)
        && let Ok(mut asset) = serde_json::from_slice::<Asset>(&data)
        && hound::WavReader::open(&path).is_ok()
    {
        asset.path = path;
        return Ok(asset);
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err("Предпросмотр отменён".into());
    }
    let temporary = root.join(format!(
        "{identity}.{}.{}.part.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let mut command = Command::new(ffmpeg);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(input)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-ac",
                "2",
                "-ar",
                "44100",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&temporary);
        let mut errors = String::new();
        if !process_runner::run(command, cancelled.clone(), |_, line| {
            errors.push_str(line);
            errors.push('\n');
        })? {
            return Err(if cancelled.load(Ordering::Relaxed) {
                "Предпросмотр отменён".into()
            } else {
                format!("Не удалось подготовить звук: {errors}")
            });
        }
        let mut reader = hound::WavReader::open(&temporary).map_err(|e| e.to_string())?;
        let spec = reader.spec();
        let frames = reader.duration() as usize;
        let mut peaks = vec![0.0f32; 2048];
        for (i, sample) in reader.samples::<i16>().enumerate() {
            if i % 65536 == 0 && cancelled.load(Ordering::Relaxed) {
                return Err("Предпросмотр отменён".into());
            }
            let index =
                ((i / spec.channels as usize) * peaks.len() / frames.max(1)).min(peaks.len() - 1);
            peaks[index] = peaks[index]
                .max(sample.map_err(|e| e.to_string())?.unsigned_abs() as f32 / 32768.0);
        }
        let asset = Asset {
            path: path.clone(),
            identity,
            duration_ms: frames as i64 * 1000 / spec.sample_rate as i64,
            peaks,
        };
        cache::replace_file(&temporary, &path)?;
        cache::atomic_write(
            &metadata,
            &serde_json::to_vec(&asset).map_err(|e| e.to_string())?,
        )?;
        prune(&root, &path);
        Ok(asset)
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn prune(root: &Path, keep: &Path) {
    let mut entries = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("wav") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((metadata.modified().ok()?, metadata.len(), path))
        })
        .collect::<Vec<_>>();
    let mut bytes: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    entries.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in entries {
        if bytes <= 512 * 1024 * 1024 {
            break;
        }
        if path != keep && fs::remove_file(&path).is_ok() {
            bytes = bytes.saturating_sub(size);
            let _ = fs::remove_file(path.with_extension("json"));
        }
    }
}

pub struct WavSource {
    reader: hound::WavReader<BufReader<fs::File>>,
    spec: hound::WavSpec,
    position: u64,
    start: u64,
    end: u64,
    fade_in: u64,
    fade_out: u64,
    gain: f32,
    duration: Duration,
}
impl WavSource {
    pub fn new(
        path: &Path,
        selection: (i64, i64),
        play_ms: i64,
        fades: (i32, i32),
        gain_db: f64,
    ) -> Result<Self, String> {
        let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
        let spec = reader.spec();
        if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
            return Err("Ожидается PCM 16 бит".into());
        }
        let frame = |ms: i64| {
            (ms.max(0) as u64 * spec.sample_rate as u64 / 1000).min(reader.duration() as u64)
        };
        let start = frame(selection.0);
        let end = frame(selection.1);
        let position = frame(play_ms).clamp(start, end);
        reader.seek(position as u32).map_err(|e| e.to_string())?;
        let channels = spec.channels as u64;
        Ok(Self {
            reader,
            spec,
            position: position * channels,
            start: start * channels,
            end: end * channels,
            fade_in: fades.0.max(0) as u64 * spec.sample_rate as u64 * channels / 1000,
            fade_out: fades.1.max(0) as u64 * spec.sample_rate as u64 * channels / 1000,
            gain: 10.0f64.powf(gain_db / 20.0) as f32,
            duration: Duration::from_secs_f64((end - position) as f64 / spec.sample_rate as f64),
        })
    }
}
impl Iterator for WavSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.position >= self.end {
            return None;
        }
        let sample = self.reader.samples::<i16>().next()?.ok()? as f32 / 32768.0;
        let frame_position = self.position / self.spec.channels as u64 * self.spec.channels as u64;
        let fade_in = if self.fade_in > 0 {
            ((frame_position - self.start) as f32 / self.fade_in as f32).min(1.0)
        } else {
            1.0
        };
        let fade_out = if self.fade_out > 0 {
            ((self.end - frame_position) as f32 / self.fade_out as f32).min(1.0)
        } else {
            1.0
        };
        self.position += 1;
        Some(sample * self.gain * fade_in * fade_out)
    }
}
impl rodio::Source for WavSource {
    fn current_frame_len(&self) -> Option<usize> {
        Some((self.end - self.position) as usize)
    }
    fn channels(&self) -> u16 {
        self.spec.channels
    }
    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_seek_preserves_selection_fades_and_stereo() {
        let path = std::env::temp_dir().join(format!("preview-test-{}.wav", std::process::id()));
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 1000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..4000 {
            writer.write_sample(16384i16).unwrap();
        }
        writer.finalize().unwrap();
        let mut source = WavSource::new(&path, (500, 1500), 750, (500, 500), 0.0).unwrap();
        assert_eq!(source.next(), Some(0.25));
        assert_eq!(source.next(), Some(0.25));
        assert_eq!(source.count(), 1498);
        assert!(
            WavSource::new(&path, (500, 1500), 1500, (0, 0), 0.0)
                .unwrap()
                .next()
                .is_none()
        );
        fs::remove_file(path).unwrap();
    }
    #[test]
    #[ignore = "requires FFmpeg; run with cargo test --lib -- --ignored"]
    fn decoded_waveform_tracks_silence_and_signal_and_cached_wav_is_reused() {
        let root = std::env::temp_dir().join(format!("waveform-smoke-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let audio = root.join("input.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&audio, spec).unwrap();
        for i in 0..88200 {
            let sample = if i < 44100 {
                0
            } else {
                (16000.0 * (i as f64 * std::f64::consts::TAU * 440.0 / 44100.0).sin()) as i16
            };
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let asset = prepare(&audio, Path::new("ffmpeg"), cancel.clone()).unwrap();
        assert_eq!(asset.duration_ms, 2000);
        assert!(asset.peaks[..1000].iter().all(|peak| *peak == 0.0));
        assert!(asset.peaks[1100..].iter().all(|peak| *peak > 0.3));
        let modified = fs::metadata(&asset.path).unwrap().modified().unwrap();
        let cached = prepare(&audio, Path::new("nonexistent-ffmpeg"), cancel).unwrap();
        assert_eq!(
            fs::metadata(&cached.path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(asset.peaks, cached.peaks);
        let source = WavSource::new(&asset.path, (0, 2000), 1500, (0, 0), 0.0).unwrap();
        assert_eq!(source.count(), 44100);
        fs::remove_dir_all(root).unwrap();
    }
}
