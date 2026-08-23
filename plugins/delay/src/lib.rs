use clack_plugin::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;

// --- Configuration Structs ---

#[derive(Deserialize)]
struct RootConfig {
    delay: Option<DelaySection>,
}

#[derive(Deserialize, Default)]
struct DelaySection {
    active_preset: Option<String>,
    presets: Option<HashMap<String, DelayConfig>>,
    #[serde(flatten)]
    base: DelayConfig,
}

impl DelaySection {
    fn resolve(&self) -> DelayConfig {
        if let Some(name) = &self.active_preset {
            if let Some(presets) = &self.presets {
                if let Some(preset) = presets.get(name) {
                    return preset.clone();
                }
            }
            println!("Warning: Preset '{}' not found, falling back to base.", name);
        }
        self.base.clone()
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct DelayConfig {
    left_delay_ms: f64,
    right_delay_ms: f64,
    feedback: f32,
    mix: f32,
}

impl Default for DelayConfig {
    fn default() -> Self {
        Self {
            left_delay_ms: 400.0,
            right_delay_ms: 530.0,
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

pub struct MyDelayPlugin;

impl Plugin for MyDelayPlugin {
    type AudioProcessor<'a> = MyDelayPluginAudioProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = ();
}

impl DefaultPluginFactory for MyDelayPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "com.example.rust-mixer-delay", 
            "Rust Mixer Configurable Delay"
        )
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        Ok(())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(())
    }
}

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
        
        // Read root config and extract the resolved [delay] section
        let config = fs::read_to_string("config.toml")
            .ok()
            .and_then(|c| toml::from_str::<RootConfig>(&c).ok())
            .and_then(|root| root.delay)
            .map(|sec| sec.resolve())
            .unwrap_or_default();
        
        let channels = vec![
            EchoDelay::new(sr, config.left_delay_ms, config.feedback, config.mix),
            EchoDelay::new(sr, config.right_delay_ms, config.feedback, config.mix),
        ];
        
        Ok(Self { channels })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for mut port_pair in audio.port_pairs() {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue; };
            
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                let delay = if ch_idx < self.channels.len() {
                    &mut self.channels[ch_idx]
                } else {
                    &mut self.channels[0]
                };

                match channel_pair {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        for (i, o) in input.iter().zip(output.iter_mut()) {
                            *o = delay.process(*i);
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        for sample in buf.iter_mut() {
                            *sample = delay.process(*sample);
                        }
                    }
                }
            }
        }
        
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<MyDelayPlugin>);
