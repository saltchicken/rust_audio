use clack_plugin::prelude::*;
use serde::Deserialize;
use plugin_core::{export_clap_plugin, load_plugin_config};

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct CompressorConfig {
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_gain_db: f32,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            threshold_db: -12.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_gain_db: 2.0,
        }
    }
}

// --- 1. DSP Utilities: Compressor ---

struct CompressorChannel {
    envelope: f32,
}

impl CompressorChannel {
    fn new() -> Self {
        Self { envelope: 0.0 }
    }

    fn process(&mut self, input: f32, sample_rate: f32, config: &CompressorConfig) -> f32 {
        // 1. Convert input peak to decibels (avoiding log10(0))
        let input_abs = input.abs().max(1e-5);
        let input_db = 20.0 * input_abs.log10();

        // 2. Calculate target gain reduction in dB
        let mut target_gain_reduction_db = 0.0;
        if input_db > config.threshold_db {
            target_gain_reduction_db = config.threshold_db - input_db + ((input_db - config.threshold_db) / config.ratio);
        }

        // 3. Smooth the gain reduction with Attack/Release envelope
        // We use a simple 1-pole filter to track the gain envelope
        let attack_coef = (-1.0 / (config.attack_ms * 0.001 * sample_rate)).exp();
        let release_coef = (-1.0 / (config.release_ms * 0.001 * sample_rate)).exp();

        if target_gain_reduction_db < self.envelope {
            // Signal is getting louder -> compressing more -> use Attack
            self.envelope = target_gain_reduction_db + attack_coef * (self.envelope - target_gain_reduction_db);
        } else {
            // Signal is getting quieter -> compressing less -> use Release
            self.envelope = target_gain_reduction_db + release_coef * (self.envelope - target_gain_reduction_db);
        }

        // 4. Apply makeup gain and convert back to linear multiplier
        let total_gain_db = self.envelope + config.makeup_gain_db;
        let gain_linear = 10.0_f32.powf(total_gain_db / 20.0);

        input * gain_linear
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyCompressorPluginAudioProcessor {
    channels: Vec<CompressorChannel>,
    sample_rate: f32,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyCompressorPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { 
            channels: vec![CompressorChannel::new(), CompressorChannel::new()],
            sample_rate: audio_config.sample_rate as f32
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let config = load_plugin_config::<CompressorConfig>("compressor");

        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let comp = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = comp.process(*i, self.sample_rate, &config);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyCompressorPlugin, 
    MyCompressorPluginAudioProcessor, 
    "com.example.rust-mixer-compressor", 
    "Rust Mixer Compressor"
);
