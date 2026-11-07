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

/// Two soft rising notes (C5 then G5), about a quarter second long.
fn make_wav(volume: f64) -> Vec<u8> {
    const RATE: u32 = 22050;
    let notes = [(523.25, 0.09), (783.99, 0.17)];
    let mut samples: Vec<i16> = Vec::new();
    for (freq, dur) in notes {
        let n = (RATE as f64 * dur) as usize;
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            let attack = (t / 0.006).min(1.0);
            let decay = (-3.5 * t / dur).exp();
            let wave = (2.0 * PI * freq * t).sin() * 0.8 + (4.0 * PI * freq * t).sin() * 0.2;
            samples.push((wave * attack * decay * volume * 0.7 * i16::MAX as f64) as i16);
        }
    }
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
