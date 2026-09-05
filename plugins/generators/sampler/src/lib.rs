use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct SamplerConfig {
    sample_path: String,
    mode: String, // "varispeed" or "granular"
    grain_size_ms: f32, // Size of grains for granular mode
    root_note: i16,
    attack_ms: f32,
    release_ms: f32,
    volume: f32,
    input_mix: f32,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            sample_path: "samples/default.wav".to_string(),
            mode: "varispeed".to_string(),
            grain_size_ms: 40.0,
            root_note: 60, // Middle C
            attack_ms: 2.0,
            release_ms: 15.0,
            volume: 0.8,
            input_mix: 1.0,
        }
    }
}

// --- 2. Audio Loader ---

fn load_wav_mono(path: &str) -> Vec<f32> {
    let Ok(mut reader) = hound::WavReader::open(path) else {
        eprintln!("⚠️ Sampler: Could not open sample '{}'", path);
        return vec![0.0];
    };

    let spec = reader.spec();
    let mut audio_data = Vec::new();

    match spec.sample_format {
        hound::SampleFormat::Float => {
            let samples: Vec<f32> = reader.samples::<f32>().filter_map(Result::ok).collect();
            if spec.channels == 2 {
                for chunk in samples.chunks_exact(2) {
                    audio_data.push((chunk[0] + chunk[1]) * 0.5);
                }
            } else {
                audio_data = samples;
            }
        }
        hound::SampleFormat::Int => {
            let max_val = match spec.bits_per_sample {
                16 => 32768.0,
                24 => 8388608.0,
                32 => 2147483648.0,
                _ => 1.0,
            };

            if spec.bits_per_sample <= 16 {
                let samples: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
                if spec.channels == 2 {
                    for chunk in samples.chunks_exact(2) {
                        audio_data.push((chunk[0] as f32 + chunk[1] as f32) * 0.5 / max_val);
                    }
                } else {
                    audio_data = samples.into_iter().map(|s| s as f32 / max_val).collect();
                }
            } else {
                let samples: Vec<i32> = reader.samples::<i32>().filter_map(Result::ok).collect();
                if spec.channels == 2 {
                    for chunk in samples.chunks_exact(2) {
                        audio_data.push((chunk[0] as f32 + chunk[1] as f32) * 0.5 / max_val);
                    }
                } else {
                    audio_data = samples.into_iter().map(|s| s as f32 / max_val).collect();
                }
            }
        }
    }

    if audio_data.is_empty() {
        audio_data.push(0.0);
    }

    println!("✅ Sampler loaded '{}' ({} samples)", path, audio_data.len());
    audio_data
}

// --- 3. Anti-Click Envelope ---

struct MicroEnvelope {
    level: f32,
    state: u8,
    attack_inc: f32,
    release_inc: f32,
}

impl MicroEnvelope {
    fn new() -> Self {
        Self { level: 0.0, state: 0, attack_inc: 0.0, release_inc: 0.0 }
    }

    fn trigger(&mut self, sample_rate: f32, attack_ms: f32, release_ms: f32) {
        self.attack_inc = 1.0 / ((attack_ms.max(0.1) / 1000.0) * sample_rate);
        self.release_inc = 1.0 / ((release_ms.max(0.1) / 1000.0) * sample_rate);
        self.state = 1;
    }

    fn release(&mut self) {
        self.state = 3;
    }

    fn process(&mut self) -> f32 {
        match self.state {
            1 => {
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = 2;
                }
            }
            3 => {
                self.level -= self.release_inc;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.state = 0;
                }
            }
            _ => {}
        }
        self.level
    }
}

// Safe interpolation helper
fn get_sample_interpolated(buffer: &[f32], pos: f32) -> f32 {
    let idx = pos as usize;
    if idx >= buffer.len() || pos < 0.0 {
        return 0.0;
    }
    let idx_next = (idx + 1).min(buffer.len() - 1);
    let frac = pos.fract();
    (buffer[idx] * (1.0 - frac)) + (buffer[idx_next] * frac)
}

// --- 4. Sampler Voice Architecture ---

struct Voice {
    active_note: Option<i16>,
    is_granular: bool,
    
    // Global playhead (advances at 1.0 speed for granular, scaled speed for varispeed)
    global_pos: f32, 
    playback_rate: f32,
    velocity: f32,
    env: MicroEnvelope,

    // Granular specifics
    grain_size_samples: f32,
    grain_phase: f32,
    grain_anchor1: f32,
    grain_anchor2: f32,
}

impl Voice {
    fn new() -> Self {
        Self {
            active_note: None,
            is_granular: false,
            global_pos: 0.0,
            playback_rate: 1.0,
            velocity: 0.0,
            env: MicroEnvelope::new(),
            grain_size_samples: 48000.0 * 0.05,
            grain_phase: 0.0,
            grain_anchor1: 0.0,
            grain_anchor2: 0.0,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &SamplerConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.velocity = velocity;
        self.is_granular = config.mode.to_lowercase() == "granular";
        
        self.playback_rate = 2.0_f32.powf((note as f32 - config.root_note as f32) / 12.0);
        
        self.global_pos = 0.0;
        
        if self.is_granular {
            self.grain_size_samples = (config.grain_size_ms / 1000.0) * sample_rate;
            self.grain_phase = 0.0;
            self.grain_anchor1 = 0.0;
            // Anchor 2 simulates a grain that started exactly half a grain-length ago
            self.grain_anchor2 = -0.5 * self.grain_size_samples;
        }

        self.env.trigger(sample_rate, config.attack_ms, config.release_ms);
    }

    fn release(&mut self) {
        self.env.release();
    }

    fn process(&mut self, _sample_rate: f32, sample_buffer: &[f32]) -> f32 {
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        let sample_out = if self.is_granular {
            // End note if global playhead exceeds the original sample duration
            if self.global_pos >= sample_buffer.len() as f32 {
                self.active_note = None;
                self.env.state = 0;
                return 0.0;
            }

            let phase_inc = 1.0 / self.grain_size_samples.max(1.0);
            
            // Advance main grain phase
            let old_phase1 = self.grain_phase;
            self.grain_phase += phase_inc;
            if self.grain_phase >= 1.0 {
                self.grain_phase -= 1.0;
                self.grain_anchor1 = self.global_pos;
            }

            // Detect phase wrapping for the second grain (offset by 180 degrees)
            let old_phase2 = (old_phase1 + 0.5) % 1.0;
            let phase2 = (self.grain_phase + 0.5) % 1.0;
            if phase2 < old_phase2 {
                self.grain_anchor2 = self.global_pos;
            }

            // --- Grain 1 ---
            let read_pos1 = self.grain_anchor1 + (self.grain_phase * self.grain_size_samples * self.playback_rate);
            let s1 = get_sample_interpolated(sample_buffer, read_pos1);
            let w1 = (self.grain_phase * PI).sin();
            let win1 = w1 * w1; // Hann Window

            // --- Grain 2 ---
            let read_pos2 = self.grain_anchor2 + (phase2 * self.grain_size_samples * self.playback_rate);
            let s2 = get_sample_interpolated(sample_buffer, read_pos2);
            let w2 = (phase2 * PI).sin();
            let win2 = w2 * w2;

            // Advance the global playhead at 1.0 speed (preserving time length)
            self.global_pos += 1.0;

            (s1 * win1) + (s2 * win2)

        } else {
            // Classic Varispeed Mode
            if self.global_pos >= sample_buffer.len() as f32 {
                self.active_note = None;
                self.env.state = 0;
                return 0.0;
            }

            let s = get_sample_interpolated(sample_buffer, self.global_pos);
            self.global_pos += self.playback_rate;
            s
        };

        sample_out * self.velocity * self.env.process()
    }
}

// --- 5. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 16;

pub struct MySamplerProcessor {
    voices: Vec<Voice>,
    sample_buffer: Vec<f32>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: SamplerConfig,
    expression: f32, // NEW
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MySamplerProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        let config = load_plugin_config::<SamplerConfig>("sampler");

        println!("    🎹 Sampler Loaded | CCs: 7 (Volume), 11 (Expression), 12 (Input Mix), 72 (Release), 73 (Attack)\r");

        let sample_buffer = load_wav_mono(&config.sample_path);

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new());
        }

        Ok(Self {
            voices,
            sample_buffer,
            sample_rate: sr,
            block_buffer: vec![0.0; max_frames],
            config,
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

        if self.block_buffer.len() < frames {
            self.block_buffer.resize(frames, 0.0);
        }

        let mut next_event = events.input.into_iter().peekable();

        for i in 0..frames {
            while let Some(event) = next_event.peek() {
                if event.header().time() as usize <= i {
                    if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                            let key = k as i16;
                            let vel = note_on.velocity() as f32;
                            let voice_idx = self
                                .voices
                                .iter()
                                .enumerate()
                                .min_by(|a, b| a.1.env.level.partial_cmp(&b.1.env.level).unwrap())
                                .map(|(idx, _)| idx)
                                .unwrap_or(0);
                            self.voices[voice_idx].trigger(
                                key,
                                vel,
                                &self.config,
                                self.sample_rate,
                            );
                        }
                    } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_off.key() {
                            let key = k as i16;
                            for voice in self.voices.iter_mut() {
                                if voice.active_note == Some(key) {
                                    voice.release();
                                }
                            }
                        } else {
                            for voice in self.voices.iter_mut() {
                                voice.release();
                            }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                11 => self.expression = val, // NEW
                                73 => self.config.attack_ms = 1.0 + val * 500.0,
                                72 => self.config.release_ms = 1.0 + val * 2000.0,
                                7 => self.config.volume = val,
                                12 => self.config.input_mix = val,
                                _ => {}
                            }
                        }
                    }
                    next_event.next();
                } else {
                    break;
                }
            }

            self.block_buffer[i] = 0.0;
            for voice in &mut self.voices {
                if voice.active_note.is_some() || voice.env.state != 0 {
                    self.block_buffer[i] += voice.process(self.sample_rate, &self.sample_buffer);
                }
            }
        }

        for i in 0..frames {
            let out = self.block_buffer[i] * self.config.volume * self.expression;
            self.block_buffer[i] = out.clamp(-1.0, 1.0);
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
    MySamplerPlugin,
    MySamplerProcessor,
    "com.example.rust-mixer-sampler",
    "Granular / Varispeed Sampler"
);
