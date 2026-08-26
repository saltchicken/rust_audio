use clack_plugin::events::event_types::MidiEvent;
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f64::consts::PI;

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct CabConfig {
    low_cut_hz: f64,
    high_cut_hz: f64,
    resonance: f64,
}

impl Default for CabConfig {
    fn default() -> Self {
        Self {
            low_cut_hz: 100.0,
            high_cut_hz: 5000.0,
            resonance: 0.707,
        }
    }
}

// --- 1. DSP Utilities: Biquad Filter ---

struct Biquad {
    b0: f64, b1: f64, b2: f64,
    a1: f64, a2: f64,
    z1: f64, z2: f64,
}

impl Biquad {
    fn new() -> Self {
        Self {
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            z1: 0.0, z2: 0.0,
        }
    }

    fn calculate_lpf(&mut self, sample_rate: f64, freq: f64, q: f64) {
        let w0 = 2.0 * PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();

        let a0 = 1.0 + alpha;
        self.b0 = ((1.0 - cos_w0) / 2.0) / a0;
        self.b1 = (1.0 - cos_w0) / a0;
        self.b2 = ((1.0 - cos_w0) / 2.0) / a0;
        self.a1 = (-2.0 * cos_w0) / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn calculate_hpf(&mut self, sample_rate: f64, freq: f64, q: f64) {
        let w0 = 2.0 * PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();

        let a0 = 1.0 + alpha;
        self.b0 = ((1.0 + cos_w0) / 2.0) / a0;
        self.b1 = (-(1.0 + cos_w0)) / a0;
        self.b2 = ((1.0 + cos_w0) / 2.0) / a0;
        self.a1 = (-2.0 * cos_w0) / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn process(&mut self, input: f32) -> f32 {
        let input_f64 = input as f64;
        let output = (self.b0 * input_f64) + self.z1;

        self.z1 = (self.b1 * input_f64) - (self.a1 * output) + self.z2;
        self.z2 = (self.b2 * input_f64) - (self.a2 * output);

        output as f32
    }
}

// --- 2. Cabinet Simulation Chain ---

struct CabChannel {
    hpf: Biquad,
    lpf: Biquad,
}

impl CabChannel {
    fn new() -> Self {
        Self {
            hpf: Biquad::new(),
            lpf: Biquad::new(),
        }
    }

    fn process(&mut self, input: f32, sample_rate: f64, config: &CabConfig) -> f32 {
        self.hpf
            .calculate_hpf(sample_rate, config.low_cut_hz, config.resonance);
        self.lpf
            .calculate_lpf(sample_rate, config.high_cut_hz, config.resonance);

        let out = self.hpf.process(input);
        self.lpf.process(out)
    }
}

// --- 3. CLAP Plugin Implementation ---

pub struct MyCabPluginAudioProcessor {
    channels: Vec<CabChannel>,
    sample_rate: f64,
    config: CabConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyCabPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            channels: vec![CabChannel::new(), CabChannel::new()],
            sample_rate: audio_config.sample_rate,
            config: load_plugin_config::<CabConfig>("cab"),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for event in events.input {
            if let Some(midi) = event.as_event::<MidiEvent>() {
                let data = midi.data();
                if data.len() == 3 && (data[0] & 0xF0) == 0xB0 {
                    let cc = data[1];
                    let val = data[2] as f64 / 127.0;
                    match cc {
                        78 => self.config.low_cut_hz = 20.0 + val * 480.0,
                        79 => self.config.high_cut_hz = 1000.0 + val * 9000.0,
                        80 => self.config.resonance = 0.1 + val * 1.9,
                        _ => {}
                    }
                }
            }
        }

        let config = &self.config;

        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let cab = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = cab.process(*i, self.sample_rate, config);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyCabPlugin,
    MyCabPluginAudioProcessor,
    "com.example.rust-mixer-cab",
    "Rust Analog Cab Sim"
);
