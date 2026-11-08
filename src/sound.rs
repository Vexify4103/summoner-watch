use crate::settings::Settings;
use std::f64::consts::PI;
use std::sync::Mutex;
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_MEMORY, SND_NODEFAULT};

// Built-in sound, cached per volume level. Buffers are leaked on purpose:
// PlaySoundW reads the memory asynchronously, so it must never be freed.
static BUILT_IN: Mutex<Option<(u32, &'static [u8])>> = Mutex::new(None);

/// Plays the "spell is up" sound without blocking.
pub fn play(s: &Settings) {
    if !s.sound_path.is_empty() {
        let wide: Vec<u16> = s.sound_path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT) };
        return;
    }
    let vol = s.volume.round() as u32;
    if vol == 0 {
        return;
    }
    let mut cache = BUILT_IN.lock().unwrap();
    if cache.map(|(v, _)| v) != Some(vol) {
        let wav: &'static [u8] = Box::leak(make_wav(vol as f64 / 100.0).into_boxed_slice());
        *cache = Some((vol, wav));
    }
    let data = cache.unwrap().1;
    unsafe { PlaySoundW(data.as_ptr() as *const u16, std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
}

const RATE: u32 = 22050;

/// Two rising notes (C6 then G6), about 0.4 seconds long.
///
/// Built to be heard over game audio: a high pitch with overtones (the ear is
/// most sensitive around 1-4 kHz), a slow decay, and a soft limiter so the
/// sound is loud without clipping. Volume 100 uses the full sample range.
fn make_samples(volume: f64) -> Vec<i16> {
    let notes = [(1046.5, 0.12), (1568.0, 0.30)];
    let mut raw: Vec<f64> = Vec::new();
    for (freq, dur) in notes {
        let n = (RATE as f64 * dur) as usize;
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            let attack = (t / 0.004).min(1.0);
            let release = ((dur - t) / 0.01).min(1.0); // no click at the note end
            let decay = (-1.6 * t / dur).exp();
            let w = 2.0 * PI * freq * t;
            let wave = w.sin() + 0.45 * (2.0 * w).sin() + 0.2 * (3.0 * w).sin();
            raw.push(wave * attack * release * decay);
        }
    }
    // Normalize to peak 1.0, then soft-limit to raise the average level.
    let peak = raw.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    const DRIVE: f64 = 2.0;
    let gain = volume.clamp(0.0, 1.0) * 0.98 * i16::MAX as f64;
    raw.iter()
        .map(|x| ((DRIVE * x / peak).tanh() / DRIVE.tanh() * gain) as i16)
        .collect()
}

fn make_wav(volume: f64) -> Vec<u8> {
    let samples = make_samples(volume);
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(s: &[i16]) -> (f64, f64) {
        let full = i16::MAX as f64;
        let peak = s.iter().map(|&x| (x as f64).abs()).fold(0.0, f64::max) / full;
        let rms = (s.iter().map(|&x| (x as f64 / full).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        (20.0 * peak.log10(), 20.0 * rms.log10())
    }

    #[test]
    fn full_volume_is_loud_but_not_clipped() {
        let (peak, rms) = levels(&make_samples(1.0));
        assert!(peak > -0.5 && peak <= 0.0, "peak {peak:.1} dBFS");
        assert!(rms > -12.0, "rms {rms:.1} dBFS");
    }

    #[test]
    fn volume_scales_level() {
        let (_, loud) = levels(&make_samples(1.0));
        let (_, quiet) = levels(&make_samples(0.5));
        assert!((loud - quiet - 6.0).abs() < 0.5);
    }
}
