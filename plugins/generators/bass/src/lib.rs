use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- 1. Configuration ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct BassConfig {
    volume: f32,
    waveform: u32,
    sub_mix: f32,

    amp_attack_ms: f32,
    amp_decay_ms: f32,
    amp_sustain: f32,
    amp_release_ms: f32,

    filter_cutoff_hz: f32,
    filter_env_mod_hz: f32,
    filter_attack_ms: f32,
    filter_decay_ms: f32,
    filter_sustain: f32,
    filter_release_ms: f32,
}

impl Default for BassConfig {
    fn default() -> Self {
        Self {
            volume: 0.3,
            waveform: 0,
            sub_mix: 0.5,

            amp_attack_ms: 2.0,
            amp_decay_ms: 150.0,
            amp_sustain: 0.5,
            amp_release_ms: 50.0,

            filter_cutoff_hz: 100.0,
            filter_env_mod_hz: 1500.0,
            filter_attack_ms: 5.0,
            filter_decay_ms: 100.0,
            filter_sustain: 0.2,
            filter_release_ms: 60.0,
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
        Self {
            state: 0,
            level: 0.0,
            sustain: 0.0,
            atk_inc: 0.0,
            dec_inc: 0.0,
            rel_inc: 0.0,
        }
    }

    fn trigger(
        &mut self,
        sample_rate: f32,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
    ) {
        self.atk_inc = 1.0 / ((attack_ms.max(0.1) / 1000.0) * sample_rate);
        self.dec_inc = 1.0 / ((decay_ms.max(0.1) / 1000.0) * sample_rate);
        self.rel_inc = 1.0 / ((release_ms.max(0.1) / 1000.0) * sample_rate);
        self.sustain = sustain.clamp(0.0, 1.0);
        self.state = 1;
    }

    fn release(&mut self) {
        self.state = 4;
    }

    fn process(&mut self) -> f32 {
        match self.state {
            1 => {
                self.level += self.atk_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = 2;
                }
            }
            2 => {
                self.level -= self.dec_inc;
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.state = 3;
                }
            }
            3 => {}
            4 => {
                self.level -= self.rel_inc;
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

// --- 3. Bass Voice Architecture ---

struct Voice {
    phase: f32,
    sub_phase: f32,
    freq: f32,
    velocity: f32,
    active_note: Option<i16>,
    amp_env: Adsr,
    filter_env: Adsr,
    filter_state: f32,
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            sub_phase: 0.0,
            freq: 110.0,
            velocity: 0.0,
            active_note: None,
            amp_env: Adsr::new(),
            filter_env: Adsr::new(),
            filter_state: 0.0,
        }
    }

    fn trigger(&mut self, note: i16, velocity: f32, config: &BassConfig, sample_rate: f32) {
        self.active_note = Some(note);
        self.freq = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
        self.velocity = velocity;

        self.amp_env.trigger(
            sample_rate,
            config.amp_attack_ms,
            config.amp_decay_ms,
            config.amp_sustain,
            config.amp_release_ms,
        );
        self.filter_env.trigger(
            sample_rate,
            config.filter_attack_ms,
            config.filter_decay_ms,
            config.filter_sustain,
            config.filter_release_ms,
        );
    }

    fn release(&mut self) {
        self.amp_env.release();
        self.filter_env.release();
    }

    fn process(&mut self, sample_rate: f32, config: &BassConfig) -> f32 {
        if self.amp_env.state == 0 {
            self.active_note = None;
            return 0.0;
        }

        let inc = self.freq / sample_rate;
        self.phase += inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        self.sub_phase += inc * 0.5;
        if self.sub_phase >= 1.0 {
            self.sub_phase -= 1.0;
        }

        let main_osc = if config.waveform == 0 {
            self.phase * 2.0 - 1.0
        } else {
            if self.phase < 0.5 { 1.0 } else { -1.0 }
        };

        let sub_osc = (self.sub_phase * 2.0 * PI).sin();
        let mix = (main_osc * (1.0 - config.sub_mix)) + (sub_osc * config.sub_mix);

        let amp_val = self.amp_env.process();
        let env_val = self.filter_env.process();

        let cutoff = config.filter_cutoff_hz + (config.filter_env_mod_hz * env_val);
        let cutoff = cutoff.min(sample_rate / 2.0);

        let wc = 2.0 * PI * cutoff / sample_rate;
        let alpha = wc / (wc + 1.0);
        self.filter_state += alpha * (mix - self.filter_state);

        self.filter_state * amp_val * self.velocity
    }
}

// --- 4. CLAP Plugin Implementation ---

const MAX_VOICES: usize = 8;

pub struct MyBassProcessor {
    voices: Vec<Voice>,
    sample_rate: f32,
    block_buffer: Vec<f32>,
    config: BassConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyBassProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let max_frames = audio_config.max_frames_count as usize;
        let config = load_plugin_config::<BassConfig>("bass");

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
                                if voice.active_note == Some(key) {
                                    voice.release();
                                }
                            }
                        } else {
                            for voice in self.voices.iter_mut() {
                                voice.release();
                            }
                        }
                    } else if let Some(midi) = event.as_event::<MidiEvent>() {
                        let data = midi.data();
                        if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                            let cc = data[1];
                            let val = data[2] as f32 / 127.0;
                            match cc {
                                14 => self.config.sub_mix = val,
                                15 => self.config.amp_decay_ms = 10.0 + val * 990.0,
                                74 => self.config.filter_cutoff_hz = 20.0 + val * 4980.0,
                                71 => self.config.filter_env_mod_hz = val * 5000.0,
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
                if voice.active_note.is_some() || voice.amp_env.state != 0 {
                    self.block_buffer[i] += voice.process(self.sample_rate, &self.config);
                }
            }
        }

        for i in 0..frames {
            let out = self.block_buffer[i] * self.config.volume;
            self.block_buffer[i] = out.clamp(-1.0, 1.0);
        }

        plugin_core::process_f32_channels(&mut audio, |_ch_idx, _input, output| {
            for (i, sample) in output.iter_mut().enumerate().take(frames) {
                *sample = self.block_buffer[i];
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyBassPlugin,
    MyBassProcessor,
    "com.example.rust-mixer-bass",
    "Analog Bass Synthesizer"
);
