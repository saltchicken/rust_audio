use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::fs;

// --- Configuration Structs ---

#[derive(Deserialize)]
struct RootConfig {
    synth: Option<SynthSection>,
}

#[derive(Deserialize, Default)]
struct SynthSection {
    active_preset: Option<String>,
    presets: Option<HashMap<String, SynthConfig>>,
    #[serde(flatten)]
    base: SynthConfig,
}

impl SynthSection {
    fn resolve(&self) -> SynthConfig {
        if let Some(name) = &self.active_preset {
            if let Some(presets) = &self.presets {
                if let Some(preset) = presets.get(name) {
                    return preset.clone();
                }
            }
            println!("Warning: Preset '{}' not found, falling back to base.", name);
        }
        self.base.clone()
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct SynthConfig {
    detune: f32,
    attack_ms: f32,
    decay_ms: f32,
    sustain_level: f32,
    release_ms: f32,
    hammer_decay_ms: f32,
    hammer_amount: f32,
    base_cutoff: f32,
    key_track: f32,
    vel_amount: f32,
    resonance: f32,
    master_volume: f32,
}

impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            detune: 1.006,
            attack_ms: 0.0,
            decay_ms: 500.0,
            sustain_level: 0.3,
            release_ms: 400.0,
            hammer_decay_ms: 100.0,
            hammer_amount: 4000.0,
            base_cutoff: 300.0,
            key_track: 0.5,
            vel_amount: 2000.0,
            resonance: 1.5,
            master_volume: 0.15,
        }
    }
}

// --- 1. DSP Components ---

struct VAnalogOscillator {
    phase1: f32,
    phase2: f32,
}

impl VAnalogOscillator {
    fn new() -> Self {
        Self { phase1: 0.0, phase2: 0.0 }
    }

    fn process(&mut self, freq: f32, detune: f32, sample_rate: f32) -> f32 {
        let inc1 = freq / sample_rate;
        let inc2 = (freq * detune) / sample_rate; 

        self.phase1 = (self.phase1 + inc1).fract();
        self.phase2 = (self.phase2 + inc2).fract();

        let saw1 = (self.phase1 * 2.0) - 1.0;
        let sq1 = if self.phase1 < 0.5 { 1.0 } else { -1.0 };
        let osc1 = (saw1 + sq1) * 0.5;

        let saw2 = (self.phase2 * 2.0) - 1.0;
        let sq2 = if self.phase2 < 0.5 { 1.0 } else { -1.0 };
        let osc2 = (saw2 + sq2) * 0.5;

        (osc1 + osc2) * 0.5
    }
}

struct AdsrEnvelope {
    level: f32,
    state: u8, // 0=Idle, 1=Attack, 2=Decay, 3=Sustain, 4=Release
    attack_inc: f32,
    decay_inc: f32,
    sustain: f32,
    release_inc: f32,
}

impl AdsrEnvelope {
    fn new(sample_rate: f32, attack_ms: f32, decay_ms: f32, sustain: f32, release_ms: f32) -> Self {
        Self {
            level: 0.0,
            state: 0,
            attack_inc: if attack_ms > 0.0 { 1.0 / ((attack_ms / 1000.0) * sample_rate) } else { 1.0 },
            decay_inc: if decay_ms > 0.0 { 1.0 / ((decay_ms / 1000.0) * sample_rate) } else { 1.0 },
            sustain,
            release_inc: if release_ms > 0.0 { 1.0 / ((release_ms / 1000.0) * sample_rate) } else { 1.0 },
        }
    }

    fn trigger(&mut self) {
        if self.attack_inc >= 1.0 {
            self.level = 1.0;
            self.state = 2; // Jump to decay
        } else {
            self.level = 0.0;
            self.state = 1; // Start attack
        }
    }

    fn release(&mut self) {
        self.state = 4;
    }

    fn process(&mut self) -> f32 {
        match self.state {
            1 => { // Attack
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = 2;
                }
            }
            2 => { // Decay
                self.level -= self.decay_inc;
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.state = 3;
                }
            }
            4 => { // Release
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

struct HammerEnvelope {
    level: f32,
    decay_inc: f32,
}

impl HammerEnvelope {
    fn new(sample_rate: f32, decay_ms: f32) -> Self {
        Self {
            level: 0.0,
            decay_inc: if decay_ms > 0.0 { 1.0 / ((decay_ms / 1000.0) * sample_rate) } else { 1.0 },
        }
    }

    fn trigger(&mut self) {
        self.level = 1.0;
    }

    fn process(&mut self) -> f32 {
        self.level -= self.decay_inc;
        if self.level < 0.0 { self.level = 0.0; }
        self.level
    }
}

struct BiquadFilter {
    x1: f32, x2: f32, y1: f32, y2: f32,
}

impl BiquadFilter {
    fn new() -> Self {
        Self { x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    fn process(&mut self, input: f32, cutoff: f32, res: f32, sample_rate: f32) -> f32 {
        let cutoff = cutoff.clamp(20.0, 20000.0);
        let w0 = 2.0 * PI * cutoff / sample_rate;
        let alpha = w0.sin() / (2.0 * res);

        let a0 = 1.0 + alpha;
        let b0 = ((1.0 - w0.cos()) / 2.0) / a0;
        let b1 = (1.0 - w0.cos()) / a0;
        let b2 = ((1.0 - w0.cos()) / 2.0) / a0;
        let a1 = (-2.0 * w0.cos()) / a0;
        let a2 = (1.0 - alpha) / a0;

        let output = b0 * input + b1 * self.x1 + b2 * self.x2 - a1 * self.y1 - a2 * self.y2;
        self.x2 = self.x1; self.x1 = input;
        self.y2 = self.y1; self.y1 = output;
        
        output
    }
}

// --- 2. Voice Architecture ---

struct Voice {
    osc: VAnalogOscillator,
    amp_env: AdsrEnvelope,
    hammer_env: HammerEnvelope,
    filter: BiquadFilter,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    config: SynthConfig,
}

impl Voice {
    fn new(sample_rate: f32, config: SynthConfig) -> Self {
        Self {
            osc: VAnalogOscillator::new(),
            amp_env: AdsrEnvelope::new(
                sample_rate, 
                config.attack_ms, 
                config.decay_ms, 
                config.sustain_level, 
                config.release_ms
            ),
            hammer_env: HammerEnvelope::new(sample_rate, config.hammer_decay_ms),
            filter: BiquadFilter::new(),
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
            config,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.amp_env.trigger();
        self.hammer_env.trigger();
    }

    fn release(&mut self) {
        self.amp_env.release();
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        if self.amp_env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        let raw_wave = self.osc.process(self.freq, self.config.detune, sample_rate);

        let hammer_mod = self.hammer_env.process() * self.config.hammer_amount;
        let vel_mod = self.velocity * self.config.vel_amount;
        
        let current_cutoff = self.config.base_cutoff + (self.freq * self.config.key_track) + hammer_mod + vel_mod;
        let filtered_wave = self.filter.process(raw_wave, current_cutoff, self.config.resonance, sample_rate);

        filtered_wave * self.amp_env.process() * self.velocity
    }
}

// --- 3. CLAP Plugin Implementation ---

pub struct MySynthPlugin;

impl Plugin for MySynthPlugin {
    type AudioProcessor<'a> = MySynthProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = ();
}

impl DefaultPluginFactory for MySynthPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "com.example.rust-mixer-synth", 
            "Rust MicroFreak Clone"
        )
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> { Ok(()) }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, _shared: &'a Self::Shared<'a>) -> Result<Self::MainThread<'a>, PluginError> { Ok(()) }
}

const MAX_VOICES: usize = 16;

pub struct MySynthProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    master_volume: f32,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MySynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        
        let config = fs::read_to_string("config.toml")
            .ok()
            .and_then(|c| toml::from_str::<RootConfig>(&c).ok())
            .and_then(|root| root.synth)
            .map(|sec| sec.resolve())
            .unwrap_or_default();

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new(sr, config.clone()));
        }

        Ok(Self { voices, sample_rate: sr, master_volume: config.master_volume })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        
        for event in events.input {
            if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                    let key = k as i16;
                    let vel = note_on.velocity() as f32;
                    let voice_idx = self.voices.iter().position(|v| v.active_note.is_none()).unwrap_or(0);
                    self.voices[voice_idx].trigger(key, vel);
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

        for mut port_pair in audio.port_pairs() {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue; };
            
            for channel_pair in channel_pairs {
                let buffer = match channel_pair {
                    ChannelPair::OutputOnly(buf) => buf,
                    ChannelPair::InputOutput(_, output) => output,
                    ChannelPair::InPlace(buf) => buf,
                    _ => continue,
                };

                for sample in buffer.iter_mut() {
                    let mut mixed_sample = 0.0;
                    for voice in &mut self.voices {
                        mixed_sample += voice.process(self.sample_rate);
                    }
                    *sample = mixed_sample * self.master_volume; 
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<MySynthPlugin>);
