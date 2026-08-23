use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use std::f32::consts::PI;

// --- 1. Minimal Voice Architecture ---

struct Voice {
    phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
    }

    fn release(&mut self) {
        self.active_note = None;
    }

    fn process(&mut self, sample_rate: f32) -> f32 {
        // Advance the phase
        let inc = self.freq / sample_rate;
        self.phase = (self.phase + inc).fract();

        // Generate pure sine wave and scale by note velocity
        (self.phase * 2.0 * PI).sin() * self.velocity
    }
}

// --- 2. CLAP Plugin Implementation ---

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
            "Simple Sine Synth"
        )
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> { Ok(()) }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, _shared: &'a Self::Shared<'a>) -> Result<Self::MainThread<'a>, PluginError> { Ok(()) }
}

const MAX_VOICES: usize = 16;

pub struct MySynthProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>, // Our new mono scratch buffer!
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
            voices.push(Voice::new());
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

        // Ensure our scratch buffer is large enough for the host's block size
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

        // Apply master volume to avoid clipping when playing chords
        for i in 0..frames {
            self.block_buffer[i] *= 0.1;
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

                // Copy the exact same frame to both ears
                for (i, sample) in buffer.iter_mut().enumerate().take(frames) {
                    *sample = self.block_buffer[i];
                }
            }
        }
        
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<MySynthPlugin>);
