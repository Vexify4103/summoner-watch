use crate::settings::Settings;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};

const RATE: u32 = 22050;
/// Imported files are cut to this length, so a long song can't keep playing.
const MAX_FILE_SECONDS: usize = 8;

/// Built-in sounds: (setting value, name in the settings panel).
pub const PRESETS: [(&str, &str); 6] = [
    ("chime", "Chime"),
    ("ding", "Ding"),
    ("rise", "Rise"),
    ("ping", "Ping"),
    ("beep", "Double beep"),
    ("alarm", "Alarm (loudest)"),
];

/// Last prepared sound and what it was made from, so it is built only once.
static CACHE: Mutex<Option<(String, Arc<Vec<u8>>)>> = Mutex::new(None);

/// Plays the "spell is up" sound without blocking the caller.
///
/// The sound plays synchronously on its own thread, which keeps the buffer
/// alive while Windows reads it. A new sound stops the one still playing.
pub fn play(s: &Settings) {
    if s.volume.round() <= 0.0 || s.sound == "off" {
        return;
    }
    let s = s.clone();
    std::thread::spawn(move || {
        let wav = prepared(&s);
        unsafe { PlaySoundW(wav.as_ptr() as *const u16, std::ptr::null_mut(), SND_MEMORY | SND_SYNC | SND_NODEFAULT) };
    });
}

fn prepared(s: &Settings) -> Arc<Vec<u8>> {
    let custom = s.sound == "custom" && !s.sound_path.is_empty();
    // The file's change time is part of the key, so an edited file is read again.
    let modified = if custom {
        std::fs::metadata(&s.sound_path).and_then(|m| m.modified()).ok()
    } else {
        None
    };
    let key = format!("{}|{}|{}|{:?}", s.sound, if custom { &s.sound_path } else { "" }, s.volume.round(), modified);
    let mut cache = CACHE.lock().unwrap();
    if let Some((k, wav)) = cache.as_ref() {
        if *k == key {
            return wav.clone();
        }
    }
    let volume = s.volume.clamp(0.0, 100.0) / 100.0;
    let wav = Arc::new(if custom {
        // A file that can't be read falls back to the default sound.
        load_file(&s.sound_path, volume).unwrap_or_else(|| make_wav(&make_preset("chime", volume), 1, RATE))
    } else {
        make_wav(&make_preset(&s.sound, volume), 1, RATE)
    });
    *cache = Some((key, wav.clone()));
    wav
}

/// One part of a built-in sound.
struct Note {
    freq: f64, // 0 = silence
    dur: f64,
    /// How fast the note fades: 0 = flat, higher = shorter ring.
    decay: f64,
    /// Overtones as (frequency multiple, level).
    partials: &'static [(f64, f64)],
}

const BRIGHT: &[(f64, f64)] = &[(1.0, 1.0), (2.0, 0.45), (3.0, 0.2)];
const BELL: &[(f64, f64)] = &[(1.0, 1.0), (2.0, 0.5), (2.76, 0.35), (5.4, 0.12)];
const SQUARE: &[(f64, f64)] = &[(1.0, 1.0), (3.0, 0.33), (5.0, 0.2), (7.0, 0.14)];

fn note(freq: f64, dur: f64, decay: f64, partials: &'static [(f64, f64)]) -> Note {
    Note { freq, dur, decay, partials }
}

fn rest(dur: f64) -> Note {
    note(0.0, dur, 0.0, &[])
}

/// Built-in sounds. All are tuned to be heard over game audio: high pitch
/// with overtones (the ear is most sensitive around 1-4 kHz).
fn notes(name: &str) -> Vec<Note> {
    match name {
        "ding" => vec![note(1318.5, 0.7, 2.6, BELL)],
        "rise" => vec![note(1046.5, 0.09, 1.2, BRIGHT), note(1318.5, 0.09, 1.2, BRIGHT), note(1568.0, 0.3, 1.6, BRIGHT)],
        "ping" => vec![note(2093.0, 0.22, 2.5, BRIGHT)],
        "beep" => vec![note(1760.0, 0.11, 0.0, SQUARE), rest(0.07), note(1760.0, 0.11, 0.0, SQUARE)],
        "alarm" => {
            let mut v = Vec::new();
            for _ in 0..3 {
                v.extend([note(2093.0, 0.09, 0.0, SQUARE), rest(0.03), note(2637.0, 0.09, 0.0, SQUARE), rest(0.12)]);
            }
            v
        }
        _ => vec![note(1046.5, 0.12, 1.6, BRIGHT), note(1568.0, 0.30, 1.6, BRIGHT)], // chime
    }
}

/// Builds a built-in sound as 16-bit mono samples. Volume 1.0 uses the full
/// range; a soft limiter makes it loud without clipping.
fn make_preset(name: &str, volume: f64) -> Vec<i16> {
    let mut raw: Vec<f64> = Vec::new();
    for n in notes(name) {
        let len = (RATE as f64 * n.dur) as usize;
        for i in 0..len {
            let t = i as f64 / RATE as f64;
            if n.freq == 0.0 {
                raw.push(0.0);
                continue;
            }
            let attack = (t / 0.004).min(1.0);
            let release = ((n.dur - t) / 0.01).min(1.0); // no click at the note end
            let decay = (-n.decay * t / n.dur).exp();
            let w = 2.0 * PI * n.freq * t;
            let wave: f64 = n.partials.iter().map(|(m, a)| a * (m * w).sin()).sum();
            raw.push(wave * attack * release * decay);
        }
    }
    let peak = raw.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    const DRIVE: f64 = 2.0;
    let gain = volume.clamp(0.0, 1.0) * 0.98 * i16::MAX as f64;
    raw.iter()
        .map(|x| ((DRIVE * x / peak).tanh() / DRIVE.tanh() * gain) as i16)
        .collect()
}

/// Reads a .wav, .mp3, .ogg or .flac file. The sound is raised to full level
/// (so quiet files are loud too), then the volume setting is applied.
fn load_file(path: &str, volume: f64) -> Option<Vec<u8>> {
    let (samples, channels, rate) = decode(path)?;
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak <= 0.0 {
        return None;
    }
    let gain = volume.clamp(0.0, 1.0) as f32 * 0.98 / peak * i16::MAX as f32;
    let pcm: Vec<i16> = samples.iter().map(|x| (x * gain) as i16).collect();
    Some(make_wav(&pcm, channels, rate))
}

/// Decodes an audio file to interleaved f32 samples: (samples, channels, rate).
fn decode(path: &str) -> Option<(Vec<f32>, u16, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).ok()?;

    let mut out: Vec<f32> = Vec::new();
    let (mut channels, mut rate) = (0u16, 0u32);
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) => break, // end of file
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue, // skip a bad frame
            Err(_) => break,
        };
        let spec = *decoded.spec();
        channels = spec.channels.count() as u16;
        rate = spec.rate;
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        out.extend_from_slice(buf.samples());
        if out.len() >= MAX_FILE_SECONDS * rate as usize * channels as usize {
            out.truncate(MAX_FILE_SECONDS * rate as usize * channels as usize);
            break;
        }
    }
    if out.is_empty() || channels == 0 || rate == 0 {
        return None;
    }
    Some((out, channels, rate))
}

/// Wraps 16-bit PCM samples (interleaved) in a WAV file.
fn make_wav(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let block = channels as u32 * 2;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * block).to_le_bytes()); // byte rate
    w.extend_from_slice(&(block as u16).to_le_bytes()); // block align
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
    fn presets_are_loud_but_not_clipped() {
        for (name, _) in PRESETS {
            let (peak, rms) = levels(&make_preset(name, 1.0));
            assert!(peak > -0.5 && peak <= 0.0, "{name}: peak {peak:.1} dBFS");
            assert!(rms > -14.0, "{name}: rms {rms:.1} dBFS");
        }
    }

    #[test]
    fn volume_scales_level() {
        let (_, loud) = levels(&make_preset("chime", 1.0));
        let (_, quiet) = levels(&make_preset("chime", 0.5));
        assert!((loud - quiet - 6.0).abs() < 0.5);
    }

    #[test]
    fn imported_wav_is_normalized_and_scaled() {
        // A quiet stereo file at 44.1 kHz.
        let quiet: Vec<i16> = (0..44100).map(|i| ((i as f64 * 0.05).sin() * 3000.0) as i16).collect();
        let path = std::env::temp_dir().join("summonerwatch_test.wav");
        std::fs::write(&path, make_wav(&quiet, 2, 44100)).unwrap();

        let wav = load_file(path.to_str().unwrap(), 0.5).expect("decodes");
        assert_eq!(&wav[22..24], &2u16.to_le_bytes()); // channels kept
        assert_eq!(&wav[24..28], &44100u32.to_le_bytes()); // rate kept
        let pcm: Vec<i16> = wav[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let (peak, _) = levels(&pcm);
        assert!((peak + 6.2).abs() < 0.3, "peak {peak:.1} dBFS"); // 0.5 * 0.98

        std::fs::remove_file(path).ok();
        assert!(load_file("C:\\does\\not\\exist.mp3", 1.0).is_none());
    }
}
