use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Default)]
struct RootConfig {
    synth: Option<SynthConfig>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct SynthConfig {
    attack_ms: f32,
    release_ms: f32,
    volume: f32,
}

impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            attack_ms: 5.0,
            release_ms: 15.0,
            volume: 0.15,
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
    fn new() -> Self {
        Self {
            level: 0.0,
            state: 0,
            attack_inc: 0.0,
            release_inc: 0.0,
        }
    }

    fn trigger(&mut self, sample_rate: f32, attack_ms: f32, release_ms: f32) {
        self.attack_inc = 1.0 / ((attack_ms.max(0.1) / 1000.0) * sample_rate);
        self.release_inc = 1.0 / ((release_ms.max(0.1) / 1000.0) * sample_rate);
        self.state = 1; 
    }
    
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
    phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    env: MicroEnvelope,
    pitch_gain: f32,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
            env: MicroEnvelope::new(),
            pitch_gain: 1.0,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &SynthConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.env.trigger(sample_rate, config.attack_ms, config.release_ms);
        self.pitch_gain = (440.0 / self.freq).sqrt().clamp(0.4, 3.0);
    }

    fn release(&mut self) {
        self.env.release();
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        let inc = self.freq / sample_rate;
        self.phase = (self.phase + inc).fract();

        (self.phase * 2.0 * PI).sin() * self.velocity * self.env.process() * self.pitch_gain
    }
}

// --- 4. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 16;

pub struct MySynthProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: SynthConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MySynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        
        let config = load_plugin_config::<RootConfig, _, _>(|root| root.synth.as_ref());

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new());
        }

        Ok(Self { 
            voices, 
            sample_rate: sr,
            block_buffer: vec![0.0; max_frames],
            config,
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
                    let voice_idx = self.voices.iter().position(|v| v.active_note.is_none()).unwrap_or(0);
                    self.voices[voice_idx].trigger(key, vel, &self.config, self.sample_rate);
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
            if voice.active_note.is_some() {
                for i in 0..frames {
                    self.block_buffer[i] += voice.process(self.sample_rate);
                }
            }
        }

        // Apply config volume instead of hardcoded 0.15
        for i in 0..frames {
            let out = self.block_buffer[i] * self.config.volume;
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
    MySynthPlugin, 
    MySynthProcessor, 
    "com.example.rust-mixer-synth", 
    "Smooth Sine Synth"
);
