use clack_plugin::events::event_types::MidiEvent;
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct DelayConfig {
    left_delay_ms: Option<f64>,
    right_delay_ms: Option<f64>,
    bpm: Option<f64>,
    left_delay_beats: Option<f64>,
    right_delay_beats: Option<f64>,
    feedback: f32,
    mix: f32,
}

impl Default for DelayConfig {
    fn default() -> Self {
        Self {
            left_delay_ms: Some(400.0),
            right_delay_ms: Some(530.0),
            bpm: None,
            left_delay_beats: None,
            right_delay_beats: None,
            feedback: 0.65,
            mix: 0.5,
        }
    }
}

// --- 1. DSP Utilities ---

struct EchoDelay {
    buffer: Vec<f32>,
    index: usize,
}

impl EchoDelay {
    fn new(sample_rate: f64, delay_ms: f64) -> Self {
        let delay_samples = ((delay_ms / 1000.0) * sample_rate) as usize;

        Self {
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f32, feedback: f32, mix: f32) -> f32 {
        let delayed = self.buffer[self.index];
        self.buffer[self.index] = input + (delayed * feedback);
        self.index = (self.index + 1) % self.buffer.len();
        (input * (1.0 - mix)) + (delayed * mix)
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyDelayPluginAudioProcessor {
    channels: Vec<EchoDelay>,
    config: DelayConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyDelayPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate;
        let config = load_plugin_config::<DelayConfig>("delay");

        let calc_ms = |beats: Option<f64>, ms: Option<f64>, bpm: Option<f64>| -> f64 {
            if let (Some(b), Some(tempo)) = (beats, bpm) {
                b * (60000.0 / tempo)
            } else {
                ms.unwrap_or(400.0)
            }
        };

        let left_ms = calc_ms(config.left_delay_beats, config.left_delay_ms, config.bpm);
        let right_ms = calc_ms(config.right_delay_beats, config.right_delay_ms, config.bpm);

        let channels = vec![EchoDelay::new(sr, left_ms), EchoDelay::new(sr, right_ms)];

        Ok(Self { channels, config })
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
                    let val = data[2] as f32 / 127.0;
                    match cc {
                        85 => self.config.feedback = val,
                        86 => self.config.mix = val,
                        _ => {}
                    }
                }
            }
        }

        let config = &self.config;

        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let delay = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = delay.process(*i, config.feedback, config.mix);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyDelayPlugin,
    MyDelayPluginAudioProcessor,
    "com.example.rust-mixer-delay",
    "Rust Mixer Configurable Delay"
);
