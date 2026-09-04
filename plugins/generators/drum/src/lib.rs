use clack_plugin::events::event_types::{MidiEvent, NoteOnEvent};
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
    clap_decay_ms: f32,
    hihat_closed_decay_ms: f32,
    hihat_open_decay_ms: f32,
    tom_decay_ms: f32,
    input_mix: f32,
}

impl Default for DrumConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            kick_decay_ms: 400.0,
            snare_decay_ms: 200.0,
            clap_decay_ms: 250.0,
            hihat_closed_decay_ms: 80.0,
            hihat_open_decay_ms: 450.0,
            tom_decay_ms: 350.0,
            input_mix: 1.0,
        }
    }
}

// --- 2. Minimal Drum Synthesis Voice ---

struct DrumVoice {
    active: bool,
    instrument: u8,
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
            active: false,
            instrument: 0,
            phase: 0.0,
            env: 0.0,
            env_decay: 0.0,
            pitch_env: 0.0,
            pitch_decay: 0.0,
            velocity: 0.0,
            rng_state: 1,
            last_noise: 0.0,
        }
    }

    fn trigger(&mut self, instrument: u8, velocity: f32, decay_ms: f32, sample_rate: f32) {
        self.active = true;
        self.instrument = instrument;
        self.velocity = velocity;
        self.phase = 0.0;
        self.env = 1.0;
        self.pitch_env = 1.0;

        self.env_decay = (-4.6 / ((decay_ms / 1000.0) * sample_rate)).exp();
        let pitch_drop_time = if instrument == 0 { 0.05 } else { 0.1 };
        self.pitch_decay = (-4.6 / (pitch_drop_time * sample_rate)).exp();
    }

    fn next_noise(&mut self) -> f32 {
        self.rng_state = self
            .rng_state
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        if !self.active {
            return 0.0;
        }

        self.env *= self.env_decay;
        if self.env < 0.001 {
            self.active = false;
            return 0.0;
        }

        let mut out = 0.0;

        match self.instrument {
            0 => { // Kick
                self.pitch_env *= self.pitch_decay;
                let freq = 50.0 + 300.0 * self.pitch_env;
                self.phase = (self.phase + freq / sample_rate).fract();
                out = (self.phase * 2.0 * PI).sin() * self.env;
            }
            1 => { // Snare
                let noise = self.next_noise();
                self.phase = (self.phase + 200.0 / sample_rate).fract();
                let tone = (self.phase * 2.0 * PI).sin() * self.env * 0.3;
                out = (noise * self.env * 0.7) + tone;
            }
            2 => { // Clap (Synthesized with noisy bursts)
                let noise = self.next_noise();
                // Simple high-passed noise for clap snap
                let hp = noise - self.last_noise;
                self.last_noise = noise;
                // Add a slight repeating envelope ripple for the "cluster" of hands clapping
                let ripple = 1.0 + (self.env * 40.0 * PI).sin() * 0.5;
                out = hp * self.env * ripple;
            }
            3 | 4 => { // Closed & Open Hi-Hats
                let noise = self.next_noise();
                let hp = noise - self.last_noise;
                self.last_noise = noise;
                out = hp * self.env;
            }
            5 | 6 | 7 => { // Toms (Low, Mid, High)
                self.pitch_env *= self.pitch_decay;
                let base_freq = match self.instrument {
                    5 => 90.0,  // Low Tom
                    6 => 130.0, // Mid Tom
                    _ => 180.0, // High Tom
                };
                let freq = base_freq + (base_freq * 1.5) * self.pitch_env;
                self.phase = (self.phase + freq / sample_rate).fract();
                out = (self.phase * 2.0 * PI).sin() * self.env;
            }
            _ => {}
        }

        out * self.velocity
    }
}

// --- 3. CLAP Plugin Processor ---

const MAX_VOICES: usize = 32;

pub struct MyDrumProcessor {
    voices: Vec<DrumVoice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: DrumConfig,
    expression: f32,
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
        for _ in 0..MAX_VOICES {
            voices.push(DrumVoice::new());
        }

        Ok(Self {
            voices,
            sample_rate: sr,
            block_buffer: vec![0.0; max_frames],
            config: load_plugin_config::<DrumConfig>("drum"),
            expression: 1.0,
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

                            // Map matching your Tidal Cycles drumMap!
                            let drum_info = match key {
                                36 => Some((0, self.config.kick_decay_ms)),         // bd
                                38 => Some((1, self.config.snare_decay_ms)),        // sn
                                39 => Some((2, self.config.clap_decay_ms)),         // cp
                                41 => Some((5, self.config.tom_decay_ms)),          // lt
                                42 => Some((3, self.config.hihat_closed_decay_ms)), // ch
                                45 => Some((6, self.config.tom_decay_ms)),          // mt
                                46 => Some((4, self.config.hihat_open_decay_ms)),   // oh
                                48 => Some((7, self.config.tom_decay_ms)),          // ht
                                _ => None,
                            };

                            if let Some((inst, decay)) = drum_info {
                                // For hi-hats, choke open hat if closed hat is triggered
                                if inst == 3 {
                                    for voice in &mut self.voices {
                                        if voice.instrument == 4 && voice.active {
                                            voice.active = false;
                                        }
                                    }
                                }

                                let voice_idx = self
                                    .voices
                                    .iter()
                                    .position(|v| !v.active)
                                    .unwrap_or_else(|| {
                                        self.voices
                                            .iter()
                                            .enumerate()
                                            .min_by(|a, b| a.1.env.partial_cmp(&b.1.env).unwrap())
                                            .map(|(idx, _)| idx)
                                            .unwrap_or(0)
                                    });
                                self.voices[voice_idx].trigger(inst, vel, decay, self.sample_rate);
                            }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                11 => self.expression = val,
                                16 => self.config.kick_decay_ms = 50.0 + val * 950.0,
                                17 => self.config.snare_decay_ms = 50.0 + val * 950.0,
                                18 => self.config.hihat_closed_decay_ms = 20.0 + val * 280.0,
                                19 => self.config.tom_decay_ms = 50.0 + val * 950.0,
                                20 => self.config.hihat_open_decay_ms = 50.0 + val * 950.0,
                                21 => self.config.clap_decay_ms = 50.0 + val * 450.0,
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
                if voice.active {
                    self.block_buffer[i] += voice.process(self.sample_rate);
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
    MyDrumPlugin,
    MyDrumProcessor,
    "com.example.rust-mixer-drum",
    "Synthesized Drum Machine"
);
