use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct PluckConfig {
    volume: f32,
    decay: f32,           // Overall feedback gain (0.0 to 1.0). 0.99 = long ring.
    damping: f32,         // Low-pass filter weight. Higher = duller string.
    input_mix: f32,
    transpose_octaves: i32, // Shifts the base pitch of the instrument
    exciter_type: u32,    // 0 = White Noise (String), 1 = Sine Wave (Bell/Mallet)
}

impl Default for PluckConfig {
    fn default() -> Self {
        Self {
            volume: 0.8,
            decay: 0.99,
            damping: 0.5,
            input_mix: 1.0,
            transpose_octaves: 0,
            exciter_type: 0, 
        }
    }
}

// --- 2. Karplus-Strong Voice Architecture ---

struct Voice {
    active: bool,
    note: i16,
    freq: f32,
    velocity: f32,
    
    // Delay Line
    delay_buffer: Vec<f32>,
    write_idx: usize,
    delay_samples: f32,
    
    // Exciter State (Holds variables for both noise and sine)
    noise_burst_left: usize,
    rng_state: u32,
    exciter_phase: f32,
    exciter_inc: f32,
    
    // Filter State
    last_out: f32,
    
    // Note Off Mute Envelope
    env: f32,
    env_decay_multiplier: f32,
}

impl Voice {
    fn new() -> Self {
        Self {
            active: false,
            note: 0,
            freq: 440.0,
            velocity: 0.0,
            delay_buffer: vec![0.0; 4096], // 4096 samples supports down to ~11.7Hz at 48kHz
            write_idx: 0,
            delay_samples: 0.0,
            noise_burst_left: 0,
            rng_state: 1,
            exciter_phase: 0.0,
            exciter_inc: 0.0,
            last_out: 0.0,
            env: 0.0,
            env_decay_multiplier: 1.0,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, sample_rate: f32, config: &PluckConfig) {
        self.active = true;
        self.note = note;
        
        // Shift the incoming MIDI note by the configured octaves
        let shifted_note = note as f32 + (config.transpose_octaves as f32 * 12.0);
        
        // Calculate frequency based on the shifted note
        self.freq = 440.0 * 2.0_f32.powf((shifted_note - 69.0) / 12.0);
        
        // Calculate delay length. We subtract 0.5 samples to compensate for the 
        // phase delay introduced by our 1-pole low-pass filter in the feedback loop.
        self.delay_samples = (sample_rate / self.freq) - 0.5;
        
        // The noise burst (the "pluck") lasts exactly one cycle of the delay line
        self.noise_burst_left = self.delay_samples.ceil() as usize;
        
        // Set up the smooth sine wave exciter variables just in case
        self.exciter_phase = 0.0;
        self.exciter_inc = self.freq / sample_rate;
        
        self.velocity = velocity;
        self.write_idx = 0;
        self.last_out = 0.0;
        self.env = 1.0;
        self.env_decay_multiplier = 1.0; // 1.0 means no decay from the mute envelope
        
        self.delay_buffer.fill(0.0);
    }

    fn release(&mut self, sample_rate: f32) {
        // When the key is lifted, simulate the player putting their hand on the string.
        // We drop the envelope to zero over ~50ms to prevent an abrupt clicking cut-off.
        self.env_decay_multiplier = (-4.6 / (0.05 * sample_rate)).exp();
    }

    fn process(&mut self, config: &PluckConfig) -> f32 {
        if !self.active {
            return 0.0;
        }

        // 1. Run the Note Off mute envelope
        if self.env_decay_multiplier < 1.0 {
            self.env *= self.env_decay_multiplier;
            if self.env < 0.001 {
                self.active = false;
                return 0.0;
            }
        }

        // 2. Generate the Exciter Burst
        let mut input = 0.0;
        if self.noise_burst_left > 0 {
            if config.exciter_type == 0 {
                // Plucked String (White Noise Burst)
                self.rng_state = self.rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
                input = (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            } else {
                // Marimba/Bell (Smooth Sine Burst)
                input = (self.exciter_phase * 2.0 * PI).sin();
                self.exciter_phase += self.exciter_inc;
            }
            self.noise_burst_left -= 1;
        }

        // 3. Read from Fractional Delay Line (Linear Interpolation)
        let mut read_idx_f = self.write_idx as f32 - self.delay_samples;
        if read_idx_f < 0.0 {
            read_idx_f += self.delay_buffer.len() as f32;
        }
        
        let idx1 = read_idx_f.trunc() as usize % self.delay_buffer.len();
        let idx2 = (idx1 + 1) % self.delay_buffer.len();
        let frac = read_idx_f.fract();
        
        let delayed = (self.delay_buffer[idx1] * (1.0 - frac)) + (self.delay_buffer[idx2] * frac);

        // 4. Low-Pass Filter in the feedback loop (Simulates high frequencies decaying faster)
        let filtered = (delayed * (1.0 - config.damping)) + (self.last_out * config.damping);
        self.last_out = filtered;

        // 5. Mix input burst with feedback loop and write back to delay line
        let output = input + (filtered * config.decay);
        self.delay_buffer[self.write_idx] = output;
        
        self.write_idx = (self.write_idx + 1) % self.delay_buffer.len();

        // 6. Scale final output
        output * self.velocity * self.env
    }
}

// --- 3. CLAP Plugin Processor ---

const MAX_VOICES: usize = 16; 

pub struct MyPluckProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: PluckConfig,
    expression: f32, // NEW
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyPluckProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        let config = load_plugin_config::<PluckConfig>("pluck");

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new());
        }

        Ok(Self {
            voices,
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
                            
                            // Voice stealing: find inactive voice, or the quietest ringing one
                            let voice_idx = self.voices.iter().enumerate()
                                .min_by(|a, b| {
                                    let env_a = if a.1.active { a.1.env } else { -1.0 };
                                    let env_b = if b.1.active { b.1.env } else { -1.0 };
                                    env_a.partial_cmp(&env_b).unwrap()
                                })
                                .map(|(idx, _)| idx)
                                .unwrap_or(0);
                                
                            self.voices[voice_idx].trigger(key, vel, self.sample_rate, &self.config);
                        }
                    } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                        if let clack_plugin::events::Match::Specific(k) = note_off.key() {
                            let key = k as i16;
                            for voice in self.voices.iter_mut() {
                                if voice.active && voice.note == key {
                                    voice.release(self.sample_rate);
                                }
                            }
                        } else {
                            for voice in self.voices.iter_mut() {
                                voice.release(self.sample_rate);
                            }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                11 => self.expression = val, // NEW
                                74 => self.config.damping = val, 
                                71 => self.config.decay = 0.5 + (val * 0.499), 
                                76 => self.config.exciter_type = if val < 0.5 { 0 } else { 1 }, // Toggle Exciter Type
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
                if voice.active {
                    self.block_buffer[i] += voice.process(&self.config);
                }
            }
        }

        // Apply volume and soft clip
        for i in 0..frames {
            let out = self.block_buffer[i] * self.config.volume * self.expression;
            self.block_buffer[i] = out.tanh();
        }

        // Mix with incoming audio
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
    MyPluckPlugin,
    MyPluckProcessor,
    "com.example.rust-mixer-pluck",
    "Karplus-Strong String Modeler"
);
