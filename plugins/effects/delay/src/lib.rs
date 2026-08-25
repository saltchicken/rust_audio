use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct DelayConfig {
    // Legacy fixed times
    left_delay_ms: Option<f64>,
    right_delay_ms: Option<f64>,
    
    // Tempo sync settings
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
    feedback: f32,
    mix: f32,
}

impl EchoDelay {
    fn new(sample_rate: f64, delay_ms: f64, feedback: f32, mix: f32) -> Self {
        let delay_samples = ((delay_ms / 1000.0) * sample_rate) as usize;

        Self {
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
            feedback,
            mix,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        self.buffer[self.index] = input + (delayed * self.feedback);
        self.index = (self.index + 1) % self.buffer.len();
        (input * (1.0 - self.mix)) + (delayed * self.mix)
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyDelayPluginAudioProcessor {
    channels: Vec<EchoDelay>,
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

        // Helper closure to calculate final MS
        let calc_ms = |beats: Option<f64>, ms: Option<f64>, bpm: Option<f64>| -> f64 {
            if let (Some(b), Some(tempo)) = (beats, bpm) {
                b * (60000.0 / tempo)
            } else {
                ms.unwrap_or(400.0) // Fallback
            }
        };

        let left_ms = calc_ms(config.left_delay_beats, config.left_delay_ms, config.bpm);
        let right_ms = calc_ms(config.right_delay_beats, config.right_delay_ms, config.bpm);

        let channels = vec![
            EchoDelay::new(sr, left_ms, config.feedback, config.mix),
            EchoDelay::new(sr, right_ms, config.feedback, config.mix),
        ];

        Ok(Self { channels })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let delay = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = delay.process(*i);
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
