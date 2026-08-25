use clack_plugin::events::event_types::{NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct DrumConfig {
    volume: f32,
    kick_decay_ms: f32,
    snare_decay_ms: f32,
    hihat_decay_ms: f32,
}

impl Default for DrumConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            kick_decay_ms: 400.0,
            snare_decay_ms: 200.0,
            hihat_decay_ms: 80.0,
        }
    }
}

// --- 2. Minimal Drum Synthesis Voice ---

struct DrumVoice {
    active: bool,
    instrument: u8, // 0: Kick, 1: Snare, 2: Hi-hat
    phase: f32,
    env: f32,
    env_decay: f32,
    pitch_env: f32,
    pitch_decay: f32,
    velocity: f32,
    rng_state: u32,
    last_noise: f32,
}

impl DrumVoice {
    fn new() -> Self {
        Self {
            active: false, instrument: 0, phase: 0.0, env: 0.0,
            env_decay: 0.0, pitch_env: 0.0, pitch_decay: 0.0,
            velocity: 0.0, rng_state: 1, last_noise: 0.0,
        }
    }

    fn trigger(&mut self, instrument: u8, velocity: f32, decay_ms: f32, sample_rate: f32) {
        self.active = true;
        self.instrument = instrument;
        self.velocity = velocity;
        self.phase = 0.0;
        self.env = 1.0;
        self.pitch_env = 1.0;
        
        // Calculate exponential decay multipliers (time to reach ~1%)
        self.env_decay = (-4.6 / ((decay_ms / 1000.0) * sample_rate)).exp();
        self.pitch_decay = (-4.6 / (0.05 * sample_rate)).exp(); // 50ms pitch drop for kick
    }

    fn next_noise(&mut self) -> f32 {
        self.rng_state = self.rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        if !self.active { return 0.0; }

        self.env *= self.env_decay;
        if self.env < 0.001 {
            self.active = false;
            return 0.0;
        }

        let mut out = 0.0;

        match self.instrument {
            0 => { // Kick: Fast pitch dropping Sine wave
                self.pitch_env *= self.pitch_decay;
                let freq = 50.0 + 300.0 * self.pitch_env;
                self.phase = (self.phase + freq / sample_rate).fract();
                out = (self.phase * 2.0 * PI).sin() * self.env;
            }
            1 => { // Snare: Noise + Fundamental Tone
                let noise = self.next_noise();
                self.phase = (self.phase + 200.0 / sample_rate).fract();
                let tone = (self.phase * 2.0 * PI).sin() * self.env * 0.3;
                out = (noise * self.env * 0.7) + tone;
            }
            2 => { // Hi-hat: Pseudo High-Pass Noise
                let noise = self.next_noise();
                let hp = noise - self.last_noise; 
                self.last_noise = noise;
                out = hp * self.env;
            }
            _ => {}
        }

        out * self.velocity
    }
}

// --- 3. CLAP Plugin Processor ---

const MAX_VOICES: usize = 8;

pub struct MyDrumProcessor {
    voices: Vec<DrumVoice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: DrumConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyDrumProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES { voices.push(DrumVoice::new()); }

        Ok(Self {
            voices,
            sample_rate: sr,
            block_buffer: vec![0.0; max_frames],
            config: load_plugin_config::<DrumConfig>("drum"),
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
        for i in 0..frames { self.block_buffer[i] = 0.0; }

        for event in events.input {
            if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                    let key = k as i16;
                    let vel = note_on.velocity() as f32;
                    
                    // Map standard MIDI notes to our drums
                    let (inst, decay) = match key {
                        36 => (0, self.config.kick_decay_ms),   // C1
                        38 => (1, self.config.snare_decay_ms),  // D1
                        42 => (2, self.config.hihat_decay_ms),  // F#1
                        _ => continue,
                    };

                    if let Some(voice) = self.voices.iter_mut().find(|v| !v.active) {
                        voice.trigger(inst, vel, decay, self.sample_rate);
                    }
                }
            }
        }

        for voice in &mut self.voices {
            if voice.active {
                for i in 0..frames {
                    self.block_buffer[i] += voice.process(self.sample_rate);
                }
            }
        }

        for i in 0..frames {
            self.block_buffer[i] = (self.block_buffer[i] * self.config.volume).clamp(-1.0, 1.0);
        }

        plugin_core::process_f32_channels(&mut audio, |_ch_idx, _input, output| {
            for (i, sample) in output.iter_mut().enumerate().take(frames) {
                *sample = self.block_buffer[i];
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyDrumPlugin,
    MyDrumProcessor,
    "com.example.rust-mixer-drum",
    "Synthesized Drum Machine"
);
