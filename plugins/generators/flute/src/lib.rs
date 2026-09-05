use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct FluteConfig {
    volume: f32,
    breath_noise: f32,
    overtone_mix: f32,
    
    vibrato_rate_hz: f32,
    vibrato_depth: f32,

    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,

    filter_cutoff_hz: f32,
    input_mix: f32,
}

impl Default for FluteConfig {
    fn default() -> Self {
        Self {
            volume: 0.6,
            breath_noise: 0.15, // Chiff sound
            overtone_mix: 0.2,  // Slight harmonic content
            
            vibrato_rate_hz: 5.5,
            vibrato_depth: 0.008, // Subtle pitch wobble

            attack_ms: 45.0,  // Soft onset (blown)
            decay_ms: 200.0,
            sustain: 0.8,
            release_ms: 150.0, // Natural fade

            filter_cutoff_hz: 2500.0, // Tames the noise/overtones
            input_mix: 1.0,
        }
    }
}

// --- 2. ADSR Envelope ---

struct Adsr {
    state: u8,
    level: f32,
    sustain: f32,
    atk_inc: f32,
    dec_inc: f32,
    rel_inc: f32,
}

impl Adsr {
    fn new() -> Self {
        Self { state: 0, level: 0.0, sustain: 0.0, atk_inc: 0.0, dec_inc: 0.0, rel_inc: 0.0 }
    }

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
            1 => {
                self.level += self.atk_inc;
                if self.level >= 1.0 { self.level = 1.0; self.state = 2; }
            }
            2 => {
                self.level -= self.dec_inc;
                if self.level <= self.sustain { self.level = self.sustain; self.state = 3; }
            }
            3 => {}
            4 => {
                self.level -= self.rel_inc;
                if self.level <= 0.0 { self.level = 0.0; self.state = 0; }
            }
            _ => {}
        }
        self.level
    }
}

// --- 3. Flute Voice Architecture ---

struct Voice {
    phase: f32,
    lfo_phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    env: Adsr,
    filter_state: f32,
    pitch_gain: f32,
    rng_state: u32,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            lfo_phase: 0.0,
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
            env: Adsr::new(),
            filter_state: 0.0,
            pitch_gain: 1.0,
            rng_state: 1,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &FluteConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.pitch_gain = (440.0 / self.freq).sqrt().clamp(0.5, 2.0);
        self.env.trigger(sample_rate, config.attack_ms, config.decay_ms, config.sustain, config.release_ms);
    }

    fn release(&mut self) {
        self.env.release();
    }

    fn process(&mut self, sample_rate: f32, config: &FluteConfig) -> f32 {
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        // 1. Vibrato LFO
        self.lfo_phase = (self.lfo_phase + (config.vibrato_rate_hz / sample_rate)).fract();
        let vibrato = (self.lfo_phase * 2.0 * PI).sin() * config.vibrato_depth;
        
        // Pitch with vibrato applied
        let modulated_freq = self.freq * (1.0 + vibrato);
        self.phase = (self.phase + (modulated_freq / sample_rate)).fract();

        // 2. Fundamental (Sine)
        let fundamental = (self.phase * 2.0 * PI).sin();

        // 3. Overtone (Octave up + slightly driven)
        let overtone_phase = (self.phase * 2.0).fract();
        let overtone = (overtone_phase * 2.0 * PI).sin().tanh();

        // 4. Breath Noise
        self.rng_state = self.rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;

        let env_val = self.env.process();
        
        // Breath is most prominent during the attack phase
        let breath_envelope = env_val * (1.0 - env_val).max(0.2); 
        let breath = noise * config.breath_noise * breath_envelope;

        let raw_mix = fundamental + (overtone * config.overtone_mix) + breath;

        // 5. Tone Filter (Simple Lowpass)
        let wc = 2.0 * PI * config.filter_cutoff_hz / sample_rate;
        let alpha = wc / (wc + 1.0);
        self.filter_state += alpha * (raw_mix - self.filter_state);

        self.filter_state * env_val * self.velocity * self.pitch_gain
    }
}

// --- 4. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 8;

pub struct MyFluteProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: FluteConfig,
    expression: f32, // NEW
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyFluteProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES { voices.push(Voice::new()); }
        
        println!("    🎹 Flute Loaded | CCs: 1 (Vibrato Depth), 2 (Breath Noise), 7 (Volume), 11 (Expression)\r");

        Ok(Self {
            voices,
            sample_rate: audio_config.sample_rate as f32,
            block_buffer: vec![0.0; audio_config.max_frames_count as usize],
            config: load_plugin_config::<FluteConfig>("flute"),
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
                        } else {
                            for voice in self.voices.iter_mut() { voice.release(); }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                11 => self.expression = val, // NEW
                                1 => self.config.vibrato_depth = val * 0.02, // Mod Wheel -> Vibrato
                                2 => self.config.breath_noise = val * 0.5,   // Breath Controller -> Noise
                                7 => self.config.volume = val,
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
                    self.block_buffer[i] += voice.process(self.sample_rate, &self.config);
                }
            }
        }

        let headroom_scaler = 1.0 / (MAX_VOICES as f32).sqrt(); 

        for i in 0..frames {
            let summed_signal = self.block_buffer[i] * self.config.volume * self.expression * headroom_scaler;
            
            // Use tanh() for soft-clipping instead of clamp() to round off peaks gracefully
            self.block_buffer[i] = summed_signal.tanh();
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
    MyFlutePlugin,
    MyFluteProcessor,
    "com.example.rust-mixer-flute",
    "Melodic Flute Synthesizer"
);
