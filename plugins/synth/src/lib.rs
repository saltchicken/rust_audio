use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use std::f32::consts::PI;

// --- 1. Anti-Click Envelope ---

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
            // 5ms attack, 15ms release - just enough to stop pops!
            attack_inc: 1.0 / (0.005 * sample_rate),
            release_inc: 1.0 / (0.015 * sample_rate),
        }
    }

    fn trigger(&mut self) {
        self.state = 1;
    }

    fn release(&mut self) {
        self.state = 3;
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

// --- 2. Minimal Voice Architecture ---

struct Voice {
    phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    env: MicroEnvelope,
    pitch_gain: f32,
}

impl Voice {
    fn new(sample_rate: f32) -> Self {
        Self {
            phase: 0.0,
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
            env: MicroEnvelope::new(sample_rate),
            pitch_gain: 1.0,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.env.trigger();
        
        // A4 (440Hz) is our 1.0x volume baseline.
        // Lower notes will calculate > 1.0, higher notes will calculate < 1.0.
        // We clamp it between 0.4 and 3.0 so it doesn't get completely out of control.
        self.pitch_gain = (440.0 / self.freq).sqrt().clamp(0.4, 3.0);
    }

    fn release(&mut self) {
        self.env.release();
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        // If the envelope is completely finished, kill the voice
        if self.env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        // Advance the phase
        let inc = self.freq / sample_rate;
        self.phase = (self.phase + inc).fract();

        // Generate pure sine wave, scale by velocity, envelope, and pitch compensation gain
        (self.phase * 2.0 * PI).sin() * self.velocity * self.env.process() * self.pitch_gain
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
            "Smooth Sine Synth"
        )
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> { Ok(()) }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, _shared: &'a Self::Shared<'a>) -> Result<Self::MainThread<'a>, PluginError> { Ok(()) }
}

const MAX_VOICES: usize = 16;

pub struct MySynthProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
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
        
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new(sr));
        }

        Ok(Self { 
            voices, 
            sample_rate: sr,
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

        // 1. Handle MIDI Events
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

        // 2. Render Audio into our MONO scratch buffer ONCE
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

        // Apply master volume and SOFT CLIPPING (tanh) to prevent harsh digital distortion
        for i in 0..frames {
            let out = self.block_buffer[i] * 0.15;
            self.block_buffer[i] = out.tanh();
        }

        // 3. Copy the rendered mono buffer to ALL output channels (Left and Right)
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

clack_export_entry!(SinglePluginEntry<MySynthPlugin>);
