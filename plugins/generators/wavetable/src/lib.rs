use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct WavetableConfig {
    wavetable_path: String,
    frame_size: usize,
    volume: f32,
    table_pos: f32,
    lfo_rate_hz: f32,
    lfo_amount: f32,
    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,
    filter_cutoff_hz: f32,
    input_mix: f32,
}

impl Default for WavetableConfig {
    fn default() -> Self {
        Self {
            wavetable_path: "".to_string(), // Empty string defaults to internal math wavetable
            frame_size: 2048,               // Standard frame size for Serum/Vital wavetables
            volume: 0.3,
            table_pos: 0.1,
            lfo_rate_hz: 0.15,
            lfo_amount: 0.8,
            attack_ms: 400.0,
            decay_ms: 100.0,
            sustain: 0.8,
            release_ms: 1200.0,
            filter_cutoff_hz: 4500.0,
            input_mix: 1.0,
        }
    }
}

// --- 2. Digital Wavetable Engine ---

struct Wavetable {
    frames: Vec<Vec<f32>>,
    num_frames: usize,
    frame_size: usize,
}

impl Wavetable {
    fn from_file(path: &str, frame_size: usize) -> Option<Self> {
        let audio_data = load_wav_mono(path)?;
        let num_frames = audio_data.len() / frame_size;
        
        if num_frames == 0 {
            eprintln!("⚠️ Wavetable: File '{}' is too short to contain even one frame of {} samples.", path, frame_size);
            return None;
        }

        let mut frames = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            let start = i * frame_size;
            frames.push(audio_data[start..start + frame_size].to_vec());
        }

        println!("✅ Wavetable loaded '{}' ({} frames, {} samples/frame)\r", path, num_frames, frame_size);
        Some(Self { frames, num_frames, frame_size })
    }

    fn new_math(frame_size: usize) -> Self {
        let num_frames = 8;
        let mut frames = vec![vec![0.0; frame_size]; num_frames];
        
        for i in 0..frame_size {
            let phase = (i as f32 / frame_size as f32) * 2.0 * PI;
            frames[0][i] = phase.sin();
            frames[1][i] = (phase.sin() + (phase * 2.0).sin() * 0.4) / 1.4;

            let mut tri = 0.0;
            for k in [1, 3, 5, 7, 9] {
                let n = k as f32;
                tri += (phase * n).sin() * (1.0 / (n * n)) * if k % 4 == 1 { 1.0 } else { -1.0 };
            }
            frames[2][i] = tri * 1.2;

            let mut saw = 0.0;
            for k in 1..=10 { saw += (phase * k as f32).sin() / k as f32; }
            frames[3][i] = saw * 1.5;

            let mut sq = 0.0;
            for k in (1..=19).step_by(2) { sq += (phase * k as f32).sin() / k as f32; }
            frames[4][i] = sq * 1.5;

            let mut pulse = 0.0;
            for k in 1..=10 { pulse += (phase * k as f32).sin() / k as f32 * (k as f32 * 0.2).cos(); }
            frames[5][i] = pulse * 2.0;

            let mut buzz = 0.0;
            for k in 1..=15 {
                let n = k as f32;
                buzz += (phase * n).sin() * ((-((n - 5.0).powi(2))) / 4.0).exp();
            }
            frames[6][i] = buzz * 2.5;
            frames[7][i] = (phase.sin() * (phase * 4.0).cos()).tanh();
        }
        
        for f in 0..num_frames {
            let mut max = 0.0_f32;
            for s in 0..frame_size { if frames[f][s].abs() > max { max = frames[f][s].abs(); } }
            if max > 0.0 { for s in 0..frame_size { frames[f][s] /= max; } }
        }
        
        println!("✅ Wavetable fallback: Generated internal mathematical frames.\r");
        Self { frames, num_frames, frame_size }
    }

    fn sample(&self, phase: f32, table_pos: f32) -> f32 {
        let pos_scaled = table_pos.clamp(0.0, 1.0) * (self.num_frames.saturating_sub(1)) as f32;
        let frame_idx = pos_scaled.floor() as usize;
        let frame_frac = pos_scaled.fract();

        let phase_idx_f = phase.fract() * self.frame_size as f32;
        let p1 = phase_idx_f.floor() as usize % self.frame_size;
        let p2 = (p1 + 1) % self.frame_size;
        let p_frac = phase_idx_f.fract();

        let s_a = self.frames[frame_idx][p1] * (1.0 - p_frac) + self.frames[frame_idx][p2] * p_frac;
        
        let s_b = if frame_idx + 1 < self.num_frames {
            self.frames[frame_idx + 1][p1] * (1.0 - p_frac) + self.frames[frame_idx + 1][p2] * p_frac
        } else {
            s_a
        };

        s_a * (1.0 - frame_frac) + s_b * frame_frac
    }
}

// Helper to load standard WAV files as float mono
fn load_wav_mono(path: &str) -> Option<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let mut audio_data = Vec::new();

    match spec.sample_format {
        hound::SampleFormat::Float => {
            let samples: Vec<f32> = reader.samples::<f32>().filter_map(Result::ok).collect();
            if spec.channels == 2 {
                for chunk in samples.chunks_exact(2) { audio_data.push((chunk[0] + chunk[1]) * 0.5); }
            } else { audio_data = samples; }
        }
        hound::SampleFormat::Int => {
            let max_val = match spec.bits_per_sample {
                16 => 32768.0, 24 => 8388608.0, 32 => 2147483648.0, _ => 1.0,
            };
            if spec.bits_per_sample <= 16 {
                let samples: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
                if spec.channels == 2 {
                    for chunk in samples.chunks_exact(2) { audio_data.push((chunk[0] as f32 + chunk[1] as f32) * 0.5 / max_val); }
                } else { audio_data = samples.into_iter().map(|s| s as f32 / max_val).collect(); }
            } else {
                let samples: Vec<i32> = reader.samples::<i32>().filter_map(Result::ok).collect();
                if spec.channels == 2 {
                    for chunk in samples.chunks_exact(2) { audio_data.push((chunk[0] as f32 + chunk[1] as f32) * 0.5 / max_val); }
                } else { audio_data = samples.into_iter().map(|s| s as f32 / max_val).collect(); }
            }
        }
    }
    Some(audio_data)
}

// --- 3. ADSR Envelope ---
struct Adsr {
    state: u8, level: f32, sustain: f32,
    atk_inc: f32, dec_inc: f32, rel_inc: f32,
}

impl Adsr {
    fn new() -> Self { Self { state: 0, level: 0.0, sustain: 0.0, atk_inc: 0.0, dec_inc: 0.0, rel_inc: 0.0 } }
    fn trigger(&mut self, sample_rate: f32, atk_ms: f32, dec_ms: f32, sus: f32, rel_ms: f32) {
        self.atk_inc = 1.0 / ((atk_ms.max(0.1) / 1000.0) * sample_rate);
        self.dec_inc = 1.0 / ((dec_ms.max(0.1) / 1000.0) * sample_rate);
        self.rel_inc = 1.0 / ((rel_ms.max(0.1) / 1000.0) * sample_rate);
        self.sustain = sus.clamp(0.0, 1.0);
        self.state = 1;
    }
    fn release(&mut self) { self.state = 4; }
    fn process(&mut self) -> f32 {
        match self.state {
            1 => { self.level += self.atk_inc; if self.level >= 1.0 { self.level = 1.0; self.state = 2; } }
            2 => { self.level -= self.dec_inc; if self.level <= self.sustain { self.level = self.sustain; self.state = 3; } }
            3 => {}
            4 => { self.level -= self.rel_inc; if self.level <= 0.0 { self.level = 0.0; self.state = 0; } }
            _ => {}
        }
        self.level
    }
}

// --- 4. Voice Architecture ---
struct Voice {
    phase: f32,
    lfo_phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    env: Adsr,
    pitch_gain: f32,
    filter_state: f32,
}

impl Voice {
    fn new() -> Self {
        Self { phase: 0.0, lfo_phase: 0.0, freq: 110.0, velocity: 0.0, active_note: None, env: Adsr::new(), pitch_gain: 1.0, filter_state: 0.0 }
    }
    fn trigger(&mut self, note: i16, velocity: f32, config: &WavetableConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.pitch_gain = (440.0 / self.freq).sqrt().clamp(0.4, 3.0);
        self.lfo_phase = (note as f32 * 0.1).fract(); 
        self.env.trigger(sample_rate, config.attack_ms, config.decay_ms, config.sustain, config.release_ms);
    }
    fn release(&mut self) { self.env.release(); }
    fn process(&mut self, sample_rate: f32, config: &WavetableConfig, wavetable: &Wavetable) -> f32 {
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        self.phase = (self.phase + (self.freq / sample_rate)).fract();
        self.lfo_phase = (self.lfo_phase + (config.lfo_rate_hz / sample_rate)).fract();

        let lfo_val = (self.lfo_phase * 2.0 * PI).sin() * 0.5 + 0.5; 
        let mut current_pos = config.table_pos + (lfo_val * config.lfo_amount);
        if current_pos > 1.0 { current_pos = 2.0 - current_pos; }
        if current_pos < 0.0 { current_pos = -current_pos; }

        let raw_mix = wavetable.sample(self.phase, current_pos);
        let amp_val = self.env.process();

        let wc = 2.0 * PI * config.filter_cutoff_hz / sample_rate;
        let alpha = wc / (wc + 1.0);
        self.filter_state += alpha * (raw_mix - self.filter_state);

        self.filter_state * amp_val * self.velocity * self.pitch_gain
    }
}

// --- 5. CLAP Plugin Processor ---
const MAX_VOICES: usize = 8;

pub struct MyWavetableProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: WavetableConfig,
    wavetable: Wavetable, 
    expression: f32, // NEW
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyWavetableProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let config = load_plugin_config::<WavetableConfig>("wavetable");
        
        let wavetable = if !config.wavetable_path.is_empty() {
            Wavetable::from_file(&config.wavetable_path, config.frame_size)
                .unwrap_or_else(|| Wavetable::new_math(config.frame_size))
        } else {
            Wavetable::new_math(config.frame_size)
        };

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES { voices.push(Voice::new()); }

        Ok(Self {
            voices,
            sample_rate: audio_config.sample_rate as f32,
            block_buffer: vec![0.0; audio_config.max_frames_count as usize],
            config,
            wavetable, 
            expression: 1.0, // NEW
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count() as usize;
        if self.block_buffer.len() < frames { self.block_buffer.resize(frames, 0.0); }
        let mut next_event = events.input.into_iter().peekable();

        for i in 0..frames {
            while let Some(event) = next_event.peek() {
                if event.header().time() as usize <= i {
                    if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                            let key = k as i16;
                            let vel = note_on.velocity() as f32;
                            let voice_idx = self.voices.iter().enumerate()
                                .min_by(|a, b| a.1.env.level.partial_cmp(&b.1.env.level).unwrap())
                                .map(|(idx, _)| idx).unwrap_or(0);
                            self.voices[voice_idx].trigger(key, vel, &self.config, self.sample_rate);
                        }
                    } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_off.key() {
                            let key = k as i16;
                            for voice in self.voices.iter_mut() {
                                if voice.active_note == Some(key) { voice.release(); }
                            }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                11 => self.expression = val, // NEW
                                71 => self.config.table_pos = val,
                                74 => self.config.filter_cutoff_hz = 100.0 + val * 10000.0,
                                76 => self.config.lfo_rate_hz = 0.01 + val * 4.0,
                                77 => self.config.lfo_amount = val,
                                _ => {}
                            }
                        }
                    }
                    next_event.next();
                } else { break; }
            }

            self.block_buffer[i] = 0.0;
            for voice in &mut self.voices {
                if voice.active_note.is_some() || voice.env.state != 0 {
                    self.block_buffer[i] += voice.process(self.sample_rate, &self.config, &self.wavetable);
                }
            }
        }

        for i in 0..frames {
            self.block_buffer[i] = (self.block_buffer[i] * self.config.volume * self.expression).clamp(-1.0, 1.0);
        }

        plugin_core::process_f32_channels(&mut audio, |_ch_idx, input, output| {
            for (i, sample) in output.iter_mut().enumerate().take(frames) {
                let in_val = input.get(i).copied().unwrap_or(0.0);
                *sample = self.block_buffer[i] + (in_val * self.config.input_mix);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyWavetablePlugin,
    MyWavetableProcessor,
    "com.example.rust-mixer-wavetable",
    "Digital Wavetable Morphing Synth"
);
