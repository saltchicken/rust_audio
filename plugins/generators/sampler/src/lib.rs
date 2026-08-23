use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config, PluginConfigSection};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize)]
struct RootConfig {
    sampler: Option<PluginConfigSection<SamplerConfig>>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct SamplerConfig {
    sample_path: String,
    root_note: i16,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            sample_path: "kick.wav".to_string(),
            root_note: 60, // C4
        }
    }
}

// --- 2. Anti-Click Envelope ---

struct MicroEnvelope {
    level: f32,
    state: u8, // 0: Idle, 1: Attack, 2: Sustain, 3: Release
    attack_inc: f32,
    release_inc: f32,
}

impl MicroEnvelope {
    fn new(sample_rate: f32) -> Self {
        Self {
            level: 0.0,
            state: 0,
            attack_inc: 1.0 / (0.005 * sample_rate), // 5ms attack
            release_inc: 1.0 / (0.015 * sample_rate), // 15ms release
        }
    }

    fn trigger(&mut self) { self.state = 1; }
    fn release(&mut self) { self.state = 3; }

    fn process(&mut self) -> f32 {
        match self.state {
            1 => { // Attack
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = 2;
                }
            }
            3 => { // Release
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

// --- 3. Minimal Voice Architecture ---

struct Voice {
    position: f32,
    rate: f32,
    velocity: f32,
    active: bool,
    active_note: Option<i16>,
    env: MicroEnvelope,
}

impl Voice {
    fn new(sample_rate: f32) -> Self {
        Self {
            position: 0.0,
            rate: 1.0,
            velocity: 0.0,
            active: false,
            active_note: None,
            env: MicroEnvelope::new(sample_rate),
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, root_note: i16) {
        self.active_note = Some(note);
        self.velocity = velocity;
        self.position = 0.0;
        
        // Pitch shift based on root note
        self.rate = 2.0_f32.powf((note as f32 - root_note as f32) / 12.0);
        self.active = true;
        self.env.trigger();
    }

    fn release(&mut self) {
        self.env.release();
        self.active_note = None;
    }

    fn process(&mut self, sample_data: &[f32]) -> f32 {
        if !self.active || sample_data.is_empty() {
            return 0.0;
        }

        let env_val = self.env.process();
        if self.env.state == 0 {
            self.active = false;
            return 0.0;
        }

        let idx = self.position.floor() as usize;
        
        if idx >= sample_data.len() {
            self.active = false;
            self.active_note = None;
            self.env.state = 0; 
            self.env.level = 0.0;
            return 0.0;
        }

        let frac = self.position - idx as f32;

        // Hermite Cubic Interpolation (requires 4 adjacent samples)
        // We carefully handle array boundaries to avoid panics at the edges
        let y0 = if idx > 0 { sample_data[idx - 1] } else { sample_data[0] };
        let y1 = sample_data[idx];
        let y2 = if idx + 1 < sample_data.len() { sample_data[idx + 1] } else { 0.0 };
        let y3 = if idx + 2 < sample_data.len() { sample_data[idx + 2] } else { 0.0 };

        // Calculate the Hermite curve coefficients
        let c0 = y1;
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        
        // Compute the smoothed sample
        let sample = ((c3 * frac + c2) * frac + c1) * frac + c0;

        self.position += self.rate;
        
        sample * self.velocity * env_val
    }
}

// --- 4. Audio Loading ---

fn load_sample(path: &str, sample_rate: f32) -> Vec<f32> {
    if let Ok(mut reader) = hound::WavReader::open(path) {
        let spec = reader.spec();
        let channels = spec.channels as usize;
        let mut mono = Vec::new();
        
        match spec.sample_format {
            hound::SampleFormat::Int => {
                let mut i = 0;
                for sample in reader.samples::<i32>() {
                    if let Ok(s) = sample {
                        if i % channels == 0 {
                            // Normalize based on bits per sample
                            let bit_shift = 32 - spec.bits_per_sample;
                            let norm = (s << bit_shift) as f32 / std::i32::MAX as f32;
                            mono.push(norm);
                        }
                    }
                    i += 1;
                }
            }
            hound::SampleFormat::Float => {
                let mut i = 0;
                for sample in reader.samples::<f32>() {
                    if let Ok(s) = sample {
                        if i % channels == 0 {
                            mono.push(s);
                        }
                    }
                    i += 1;
                }
            }
        }
        
        if !mono.is_empty() {
            return mono;
        }
    }
    
    // Fallback: Generate a synthesized kick drum if file isn't found
    let mut buf = Vec::new();
    let duration = 0.8;
    let frames = (duration * sample_rate) as usize;
    for i in 0..frames {
        let t = i as f32 / sample_rate;
        let env = (1.0 - (t / duration)).powi(3).max(0.0);
        let freq = 50.0 + 150.0 * (1.0 - (t / duration)).powi(8);
        let val = (t * freq * 2.0 * PI).sin() * env;
        buf.push(val * 0.8);
    }
    buf
}

// --- 5. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 16;

pub struct MySamplerProcessor {
    voices: Vec<Voice>,
    sample_data: Vec<f32>,
    root_note: i16,
    block_buffer: Vec<f32>,
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
        
        let config = load_plugin_config::<RootConfig, _, _>(|root| root.sampler);
        let sample_data = load_sample(&config.sample_path, sr);
        
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new(sr));
        }

        Ok(Self {
            voices,
            sample_data,
            root_note: config.root_note,
            block_buffer: vec![0.0; max_frames],
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

        for event in events.input {
            if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                    let key = k as i16;
                    let vel = note_on.velocity() as f32;
                    let voice_idx = self.voices.iter().position(|v| !v.active).unwrap_or(0);
                    self.voices[voice_idx].trigger(key, vel, self.root_note);
                }
            } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                match note_off.key() {
                    clack_plugin::events::Match::Specific(k) => {
                        let key = k as i16;
                        for voice in self.voices.iter_mut() {
                            if voice.active_note == Some(key) {
                                voice.release(); 
                            }
                        }
                    }
                    _ => {
                        for voice in self.voices.iter_mut() {
                            voice.release();
                        }
                    }
                }
            }
        }

        for i in 0..frames {
            self.block_buffer[i] = 0.0;
        }

        for voice in &mut self.voices {
            if voice.active {
                for i in 0..frames {
                    self.block_buffer[i] += voice.process(&self.sample_data);
                }
            }
        }

        // Apply gain staging and saturation to prevent clipping
        for i in 0..frames {
            // Drop gain slightly to give headroom for polyphony
            let out = self.block_buffer[i] * 0.5;
            self.block_buffer[i] = out.tanh();
        }

        for mut port_pair in audio.port_pairs() {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue; };
            
            for channel_pair in channel_pairs {
                let buffer = match channel_pair {
                    ChannelPair::OutputOnly(buf) => buf,
                    ChannelPair::InputOutput(_, output) => output,
                    ChannelPair::InPlace(buf) => buf,
                    _ => continue,
                };

                for (i, sample) in buffer.iter_mut().enumerate().take(frames) {
                    *sample = self.block_buffer[i];
                }
            }
        }
        
        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MySamplerPlugin, 
    MySamplerProcessor, 
    "com.example.rust-mixer-sampler", 
    "Configurable Sampler"
);
