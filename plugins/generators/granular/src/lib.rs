use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;

#[derive(Deserialize, Clone)]
#[serde(default)]
struct GranularConfig {
    sample_path: String,
    volume: f32,
    grain_size_ms: f32,
    grain_spacing_ms: f32,
    position: f32,       // 0.0 to 1.0 (where to scan in the sample)
    position_jitter: f32,
    attack_ms: f32,
    release_ms: f32,
    input_mix: f32,
}

impl Default for GranularConfig {
    fn default() -> Self {
        Self {
            sample_path: "samples/vocal.wav".to_string(),
            volume: 0.8,
            grain_size_ms: 60.0,
            grain_spacing_ms: 20.0,
            position: 0.5,
            position_jitter: 0.05,
            attack_ms: 100.0,
            release_ms: 300.0,
            input_mix: 1.0,
        }
    }
}

// --- Grain & Voice Architecture ---

#[derive(Clone, Default)]
struct Grain {
    active: bool,
    start_idx: f64,
    current_idx: f64,
    duration_samples: f64,
    speed: f64,
}

struct GranularVoice {
    active_note: Option<i16>,
    velocity: f32,
    base_freq: f32,
    grains: [Grain; 8],
    grain_idx: usize,
    samples_since_last_grain: f32,
    env_level: f32,
    env_state: u8, // 0: Off, 1: Atk, 2: Rel
    atk_inc: f32,
    rel_inc: f32,
    rng_state: u32,
}

impl GranularVoice {
    fn new() -> Self {
        Self {
            active_note: None,
            velocity: 0.0,
            base_freq: 440.0,
            grains: core::array::from_fn(|_| Grain::default()),
            grain_idx: 0,
            samples_since_last_grain: 999999.0,
            env_level: 0.0,
            env_state: 0,
            atk_inc: 0.0,
            rel_inc: 0.0,
            rng_state: 1,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &GranularConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.velocity = velocity;
        self.base_freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        
        self.atk_inc = 1.0 / ((config.attack_ms.max(0.1) / 1000.0) * sample_rate);
        self.rel_inc = 1.0 / ((config.release_ms.max(0.1) / 1000.0) * sample_rate);
        self.env_state = 1;
    }

    fn release(&mut self) {
        self.env_state = 2;
    }

    fn process(&mut self, sample_buffer: &[f32], config: &GranularConfig, sample_rate: f32) -> f32 {
        if self.env_state == 0 || sample_buffer.is_empty() {
            self.active_note = None;
            return 0.0;
        }

        // Process Envelope
        match self.env_state {
            1 => {
                self.env_level += self.atk_inc;
                if self.env_level >= 1.0 { self.env_level = 1.0; }
            }
            2 => {
                self.env_level -= self.rel_inc;
                if self.env_level <= 0.0 { self.env_level = 0.0; self.env_state = 0; }
            }
            _ => {}
        }

        // Use f64 for stable math on long buffers
        let pitch_ratio = self.base_freq as f64 / 261.63; // Assume sample is Middle C
        let spacing_samples = (config.grain_spacing_ms / 1000.0) * sample_rate;

        // Spawn new grains
        self.samples_since_last_grain += 1.0;
        if self.samples_since_last_grain >= spacing_samples {
            self.samples_since_last_grain = 0.0;
            
            self.rng_state = self.rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
            let jitter = (self.rng_state as f32 / u32::MAX as f32) * config.position_jitter;
            let start_pos = (config.position + jitter).clamp(0.0, 1.0) as f64;
            
            let grain_duration = (config.grain_size_ms as f64 / 1000.0) * sample_rate as f64;
            
            let g = &mut self.grains[self.grain_idx];
            g.active = true;
            g.start_idx = start_pos * sample_buffer.len() as f64;
            g.current_idx = g.start_idx;
            g.duration_samples = grain_duration;
            g.speed = pitch_ratio;

            self.grain_idx = (self.grain_idx + 1) % self.grains.len();
        }

        // Accumulate active grains
        let mut out = 0.0;
        for g in &mut self.grains {
            if !g.active { continue; }
            
            let progress = (g.current_idx - g.start_idx) / (g.duration_samples * g.speed);
            if progress >= 1.0 || g.current_idx >= sample_buffer.len() as f64 {
                g.active = false;
                continue;
            }

            // Hanning Window using high precision f64
            let window = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * progress).cos());
            
            // Linear Interpolation
            let idx = g.current_idx as usize;
            let frac = g.current_idx.fract() as f32; // Cast down for sample math
            
            let s1 = sample_buffer[idx];
            let s2 = if idx + 1 < sample_buffer.len() { sample_buffer[idx + 1] } else { 0.0 };
            let sample = s1 + frac * (s2 - s1);

            out += sample * window as f32;
            g.current_idx += g.speed;
        }

        out * self.env_level * self.velocity
    }
}

// --- CLAP Plugin Implementation ---

const MAX_VOICES: usize = 16;

pub struct GranularProcessor {
    voices: Vec<GranularVoice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: GranularConfig,
    audio_data: Vec<f32>,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for GranularProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let config = load_plugin_config::<GranularConfig>("granular");
        
        // Robust WAV loading via Hound with Stereo downmixing
        let audio_data = if let Ok(mut reader) = hound::WavReader::open(&config.sample_path) {
            let spec = reader.spec();
            let channels = spec.channels as usize;
            
            let raw_floats: Vec<f32> = if spec.sample_format == hound::SampleFormat::Float {
                let samples: Result<Vec<f32>, _> = reader.samples().collect();
                samples.unwrap_or_default()
            } else {
                let max_amp = (1u64 << (spec.bits_per_sample - 1)) as f32;
                let samples: Result<Vec<i32>, _> = reader.samples().collect();
                
                samples.unwrap_or_default()
                    .into_iter()
                    .map(|s| s as f32 / max_amp)
                    .collect()
            };

            // Downmix stereo to mono to prevent channel skipping/ring-modulation
            if channels > 1 {
                raw_floats.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect()
            } else {
                raw_floats
            }
        } else {
            // Fallback sine wave if no vocal sample is found
            (0..44100).map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 44100.0).sin()).collect()
        };

        Ok(Self {
            voices: (0..MAX_VOICES).map(|_| GranularVoice::new()).collect(),
            sample_rate: sr,
            block_buffer: vec![0.0; audio_config.max_frames_count as usize],
            config,
            audio_data,
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
                                .min_by(|a, b| a.1.env_level.partial_cmp(&b.1.env_level).unwrap())
                                .map(|(idx, _)| idx).unwrap_or(0);
                            self.voices[voice_idx].trigger(key, vel, &self.config, self.sample_rate);
                        }
                    } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_off.key() {
                            for v in &mut self.voices {
                                if v.active_note == Some(k as i16) { v.release(); }
                            }
                        } else {
                            for v in &mut self.voices { v.release(); }
                        }
                    }
                    next_event.next();
                } else { break; }
            }

            self.block_buffer[i] = 0.0;
            for voice in &mut self.voices {
                if voice.active_note.is_some() || voice.env_state != 0 {
                    self.block_buffer[i] += voice.process(&self.audio_data, &self.config, self.sample_rate);
                }
            }
        }

        for i in 0..frames {
            self.block_buffer[i] = (self.block_buffer[i] * self.config.volume).clamp(-1.0, 1.0);
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
    MyGranularPlugin,
    GranularProcessor,
    "com.example.rust-mixer-granular",
    "Vocalese Granular Engine"
);
