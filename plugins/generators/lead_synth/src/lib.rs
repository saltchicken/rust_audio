// plugins/generators/lead_synth/src/lib.rs
use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config, PluginConfigSection};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize)]
struct RootConfig {
    lead_synth: Option<PluginConfigSection<LeadSynthConfig>>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct LeadSynthConfig {
    osc_mix: f32,           // 0.0 = Saw, 1.0 = Square
    filter_cutoff: f32,     // Base cutoff in Hz
    filter_res: f32,        // 0.0 to 0.99 (Resonance)
    filter_env_amount: f32, // How much the ADSR sweeps the filter (Hz)
    attack_ms: f32,
    decay_ms: f32,
    sustain_level: f32,
    release_ms: f32,
}

impl Default for LeadSynthConfig {
    fn default() -> Self {
        Self {
            osc_mix: 0.3, // Mostly saw, bit of square body
            filter_cutoff: 400.0,
            filter_res: 0.7,
            filter_env_amount: 4000.0, // Big sweep for lead plucks
            attack_ms: 10.0,
            decay_ms: 150.0,
            sustain_level: 0.5,
            release_ms: 200.0,
        }
    }
}

// --- 2. ADSR Envelope ---

struct Adsr {
    level: f32,
    state: u8, // 0: Idle, 1: Attack, 2: Decay, 3: Sustain, 4: Release
    attack_inc: f32,
    decay_inc: f32,
    sustain_level: f32,
    release_inc: f32,
}

impl Adsr {
    fn new() -> Self {
        Self {
            level: 0.0,
            state: 0,
            attack_inc: 0.0,
            decay_inc: 0.0,
            sustain_level: 0.0,
            release_inc: 0.0,
        }
    }

    fn update_rates(&mut self, config: &LeadSynthConfig, sample_rate: f32) {
        self.attack_inc = 1.0 / (config.attack_ms.max(1.0) / 1000.0 * sample_rate);
        self.decay_inc = 1.0 / (config.decay_ms.max(1.0) / 1000.0 * sample_rate);
        self.sustain_level = config.sustain_level.clamp(0.0, 1.0);
        self.release_inc = 1.0 / (config.release_ms.max(1.0) / 1000.0 * sample_rate);
    }

    fn trigger(&mut self) {
        self.state = 1;
    }

    fn release(&mut self) {
        if self.state != 0 {
            self.state = 4;
        }
    }

    fn process(&mut self) -> f32 {
        match self.state {
            1 => {
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = 2; // Move to Decay
                }
            }
            2 => {
                self.level -= self.decay_inc;
                if self.level <= self.sustain_level {
                    self.level = self.sustain_level;
                    self.state = 3; // Move to Sustain
                }
            }
            4 => {
                self.level -= self.release_inc;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.state = 0; // Idle
                }
            }
            _ => {}
        }
        self.level
    }
}

// --- 3. Resonant Low-Pass Filter (SVF) ---

struct Filter {
    lp: f32,
    hp: f32,
    bp: f32,
}

impl Filter {
    fn new() -> Self {
        Self { lp: 0.0, hp: 0.0, bp: 0.0 }
    }

    fn process(&mut self, input: f32, cutoff: f32, res: f32, sample_rate: f32) -> f32 {
        // Safe linear SVF implementation. Cap cutoff well below Nyquist to prevent blowups.
        let fc = cutoff.clamp(20.0, sample_rate / 2.5); 
        let f = 2.0 * (PI * fc / sample_rate).sin();
        let q = 1.0 - res.clamp(0.0, 0.99);

        self.lp += f * self.bp;
        self.hp = input - self.lp - q * self.bp;
        self.bp += f * self.hp;
        
        self.lp
    }
}

// --- 4. Voice Architecture ---

struct Voice {
    phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    env: Adsr,
    filter: Filter,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
            env: Adsr::new(),
            filter: Filter::new(),
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &LeadSynthConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.env.update_rates(config, sample_rate);
        self.env.trigger();
    }

    fn release(&mut self) {
        self.env.release();
    }

    fn process(&mut self, config: &LeadSynthConfig, sample_rate: f32) -> f32 {
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        // 1. Oscillator Generation (Mix Saw and Square)
        let inc = self.freq / sample_rate;
        self.phase = (self.phase + inc).fract();
        
        let saw = 2.0 * self.phase - 1.0;
        let square = if self.phase < 0.5 { 1.0 } else { -1.0 };
        let osc_out = saw * (1.0 - config.osc_mix) + square * config.osc_mix;

        // 2. ADSR Envelope
        let env_val = self.env.process();

        // 3. Filter Processing (Cutoff swept by ADSR)
        let current_cutoff = config.filter_cutoff + (env_val * config.filter_env_amount);
        let filtered_out = self.filter.process(osc_out, current_cutoff, config.filter_res, sample_rate);

        // Gain compensation for resonance loss
        let res_gain = 1.0 + (config.filter_res * 0.5);

        filtered_out * self.velocity * env_val * res_gain
    }
}

// --- 5. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 16;

pub struct LeadSynthProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    config: LeadSynthConfig,
    block_buffer: Vec<f32>,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for LeadSynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        let config = load_plugin_config::<RootConfig, _, _>(|root| root.lead_synth);

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new());
        }

        Ok(Self { 
            voices, 
            sample_rate: sr,
            config,
            block_buffer: vec![0.0; max_frames]
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
                    self.block_buffer[i] += voice.process(&self.config, self.sample_rate);
                }
            }
        }

        for i in 0..frames {
            // Apply gentle saturation at the output to gel the voices and prevent digital clipping
            let out = self.block_buffer[i] * 0.25;
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
    LeadSynthPlugin, 
    LeadSynthProcessor, 
    "com.example.rust-mixer-lead-synth", 
    "Lead Keys Synth"
);
