use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use std::f32::consts::PI;

// --- 1. DSP Components ---

struct VAnalogOscillator {
    phase1: f32,
    phase2: f32,
}

impl VAnalogOscillator {
    fn new() -> Self {
        Self { phase1: 0.0, phase2: 0.0 }
    }

    fn process(&mut self, freq: f32, sample_rate: f32) -> f32 {
        let inc1 = freq / sample_rate;
        let inc2 = (freq * 1.006) / sample_rate; // Detune slightly for Timbre thickness

        self.phase1 = (self.phase1 + inc1).fract();
        self.phase2 = (self.phase2 + inc2).fract();

        // Wave 50: Blend 50% Sawtooth and 50% Square
        let saw1 = (self.phase1 * 2.0) - 1.0;
        let sq1 = if self.phase1 < 0.5 { 1.0 } else { -1.0 };
        let osc1 = (saw1 + sq1) * 0.5;

        let saw2 = (self.phase2 * 2.0) - 1.0;
        let sq2 = if self.phase2 < 0.5 { 1.0 } else { -1.0 };
        let osc2 = (saw2 + sq2) * 0.5;

        // Mix the two oscillators together
        (osc1 + osc2) * 0.5
    }
}

struct AdsrEnvelope {
    level: f32,
    state: u8, // 0=Idle, 1=Attack, 2=Decay, 3=Sustain, 4=Release
    sustain: f32,
    decay_inc: f32,
    release_inc: f32,
}

impl AdsrEnvelope {
    fn new(sample_rate: f32) -> Self {
        Self {
            level: 0.0,
            state: 0,
            sustain: 0.3,                     // 30% Sustain
            decay_inc: 1.0 / (0.5 * sample_rate),   // 500ms Decay
            release_inc: 1.0 / (0.4 * sample_rate), // 400ms Release
        }
    }

    fn trigger(&mut self) {
        self.level = 1.0; // 0ms Attack (Instant)
        self.state = 2;   // Skip straight to Decay
    }

    fn release(&mut self) {
        self.state = 4;
    }

    fn process(&mut self) -> f32 {
        match self.state {
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
                    self.state = 0; // Back to idle
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
    fn new(sample_rate: f32) -> Self {
        Self {
            level: 0.0,
            decay_inc: 1.0 / (0.1 * sample_rate), // 100ms fast decay
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
}

impl Voice {
    fn new(sample_rate: f32) -> Self {
        Self {
            osc: VAnalogOscillator::new(),
            amp_env: AdsrEnvelope::new(sample_rate),
            hammer_env: HammerEnvelope::new(sample_rate),
            filter: BiquadFilter::new(),
            freq: 440.0,
            velocity: 0.0,
            active_note: None,
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
        // If the amp envelope has completely faded out, the voice is officially dead
        if self.amp_env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        // 1. Generate Raw Waveform
        let raw_wave = self.osc.process(self.freq, sample_rate);

        // 2. Mod Matrix: Calculate Filter Cutoff dynamically
        let hammer_mod = self.hammer_env.process() * 4000.0; // CycEnv -> Cutoff (+35)
        let vel_mod = self.velocity * 2000.0;                // Pressure -> Cutoff (+15)
        
        // Base cutoff is ~300Hz, plus we add a little key-tracking (freq * 0.5) so high notes stay bright
        let current_cutoff = 300.0 + (self.freq * 0.5) + hammer_mod + vel_mod;

        // 3. Apply Low Pass Filter (Resonance roughly 1.5 simulates 10% feedback)
        let filtered_wave = self.filter.process(raw_wave, current_cutoff, 1.5, sample_rate);

        // 4. Apply Amp Envelope
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
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MySynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        
        // Initialize 16 voices using the sample rate
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new(sr));
        }

        Ok(Self { voices, sample_rate: sr })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        
        // 1. READ MIDI EVENTS
        for event in events.input {
            if let Some(note_on) = event.as_event::<NoteOnEvent>() {
                if let clack_plugin::events::Match::Specific(k) = note_on.key() {
                    let key = k as i16;
                    let vel = note_on.velocity() as f32;
                    
                    // Find an idle voice (where active_note is None)
                    let voice_idx = self.voices.iter().position(|v| v.active_note.is_none()).unwrap_or(0);
                    self.voices[voice_idx].trigger(key, vel);
                }
            } else if let Some(note_off) = event.as_event::<NoteOffEvent>() {
                match note_off.key() {
                    clack_plugin::events::Match::Specific(k) => {
                        let key = k as i16;
                        for voice in self.voices.iter_mut() {
                            if voice.active_note == Some(key) {
                                // Trigger release stage instead of killing the audio instantly!
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

        // 2. RENDER AUDIO
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
                    
                    *sample = mixed_sample * 0.15; // Master Volume 
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<MySynthPlugin>);
