use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct DroneConfig {
    volume: f32,
    detune_cents: f32, // Detune the second sawtooth for thick beating
    noise_mix: f32,    // Amount of white noise texture

    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,

    filter_cutoff_hz: f32,
    filter_resonance: f32,
    lfo_rate_hz: f32,  // Speed of the filter sweep
    lfo_amount: f32,   // How much the LFO modulates the cutoff (0.0 to 1.0)

    input_mix: f32,
}

impl Default for DroneConfig {
    fn default() -> Self {
        Self {
            volume: 0.25,
            detune_cents: 15.0,
            noise_mix: 0.1,

            attack_ms: 3000.0,  // 3 second attack!
            decay_ms: 1000.0,
            sustain: 1.0,       // Full sustain for continuous drone
            release_ms: 4000.0, // 4 second release

            filter_cutoff_hz: 300.0,
            filter_resonance: 0.3,
            lfo_rate_hz: 0.1,   // 1 cycle every 10 seconds
            lfo_amount: 0.6,

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

// --- 3. Transistor Ladder Filter ---

struct LadderFilter {
    y1: f32, y2: f32, y3: f32, y4: f32,
}

impl LadderFilter {
    fn new() -> Self {
        Self { y1: 0.0, y2: 0.0, y3: 0.0, y4: 0.0 }
    }

    fn process(&mut self, input: f32, cutoff: f32, resonance: f32, sample_rate: f32) -> f32 {
        let cutoff = cutoff.clamp(20.0, sample_rate * 0.45);
        let w0 = 2.0 * PI * cutoff / sample_rate;
        let res_scaled = resonance.clamp(0.0, 1.0) * 3.9; 

        let comp = 1.0 + 0.5 * resonance;
        let fb = res_scaled * self.y4;
        let input_driven = (input * comp - fb).tanh();

        self.y1 += w0 * (input_driven - self.y1);
        self.y2 += w0 * (self.y1 - self.y2);
        self.y3 += w0 * (self.y2 - self.y3);
        self.y4 += w0 * (self.y3 - self.y4);

        self.y4
    }
}

// --- 4. Drone Voice Architecture ---

struct Voice {
    phase1: f32,
    phase2: f32,
    phase_sub: f32,
    lfo_phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    amp_env: Adsr,
    ladder: LadderFilter,
    pitch_gain: f32,
    rng_state: u32,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase1: 0.0,
            phase2: 0.0,
            phase_sub: 0.0,
            lfo_phase: 0.0, // Start LFO at 0 for each voice
            freq: 110.0,
            velocity: 0.0,
            active_note: None,
            amp_env: Adsr::new(),
            ladder: LadderFilter::new(),
            pitch_gain: 1.0,
            rng_state: 1,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &DroneConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;
        self.pitch_gain = (440.0 / self.freq).sqrt().clamp(0.4, 3.0);

        self.amp_env.trigger(sample_rate, config.attack_ms, config.decay_ms, config.sustain, config.release_ms);
    }

    fn release(&mut self) {
        self.amp_env.release();
    }

    fn process(&mut self, sample_rate: f32, config: &DroneConfig) -> f32 {
        if self.amp_env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        let freq2 = self.freq * 2.0_f32.powf(config.detune_cents / 1200.0);
        let freq_sub = self.freq * 0.5; // One octave down

        self.phase1 = (self.phase1 + (self.freq / sample_rate)).fract();
        self.phase2 = (self.phase2 + (freq2 / sample_rate)).fract();
        self.phase_sub = (self.phase_sub + (freq_sub / sample_rate)).fract();
        self.lfo_phase = (self.lfo_phase + (config.lfo_rate_hz / sample_rate)).fract();

        // Dual Sawtooths
        let saw1 = self.phase1 * 2.0 - 1.0;
        let saw2 = self.phase2 * 2.0 - 1.0;

        // Sub Sine
        let sub = (self.phase_sub * 2.0 * PI).sin();

        // White Noise
        self.rng_state = self.rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (self.rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;

        // Thick Mix
        let raw_mix = (saw1 * 0.3) + (saw2 * 0.3) + (sub * 0.4) + (noise * config.noise_mix);
        
        let amp_val = self.amp_env.process();

        // Breathing LFO on the Filter (unipolar modulation upwards from cutoff)
        let lfo_val = (self.lfo_phase * 2.0 * PI).sin() * 0.5 + 0.5;
        let current_cutoff = config.filter_cutoff_hz + (config.lfo_amount * lfo_val * config.filter_cutoff_hz * 10.0);
        
        let filtered = self.ladder.process(raw_mix, current_cutoff, config.filter_resonance, sample_rate);

        filtered * amp_val * self.velocity * self.pitch_gain
    }
}

// --- 5. CLAP Plugin Processor ---

const MAX_VOICES: usize = 6; // Drones usually require polyphony for thick ambient chords

pub struct MyDroneProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: DroneConfig,
    expression: f32,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyDroneProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        let config = load_plugin_config::<DroneConfig>("drone");

        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES { voices.push(Voice::new()); }

        Ok(Self { voices, sample_rate: sr, block_buffer: vec![0.0; max_frames], config, expression: 1.0 })
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
                                .min_by(|a, b| a.1.amp_env.level.partial_cmp(&b.1.amp_env.level).unwrap())
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
                                11 => self.expression = val,
                                74 => self.config.filter_cutoff_hz = 20.0 + val * 2000.0,
                                71 => self.config.filter_resonance = val,
                                73 => self.config.attack_ms = 10.0 + val * 10000.0, // Up to 10s attack
                                72 => self.config.release_ms = 10.0 + val * 15000.0, // Up to 15s release
                                76 => self.config.lfo_rate_hz = 0.01 + val * 2.0,
                                _ => {}
                            }
                        }
                    }
                    next_event.next();
                } else { break; }
            }

            self.block_buffer[i] = 0.0;
            for voice in &mut self.voices {
                if voice.active_note.is_some() || voice.amp_env.state != 0 {
                    self.block_buffer[i] += voice.process(self.sample_rate, &self.config);
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
    MyDronePlugin,
    MyDroneProcessor,
    "com.example.rust-mixer-drone",
    "Ambient Drone Synthesizer"
);
