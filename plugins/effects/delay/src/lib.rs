use clack_plugin::prelude::*;
use serde::Deserialize;
use plugin_core::{export_clap_plugin, load_plugin_config, PluginConfigSection};

// --- Configuration Structs ---

#[derive(Deserialize)]
struct RootConfig {
    delay: Option<PluginConfigSection<DelayConfig>>,
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
        
        let config = load_plugin_config::<RootConfig, _, _>(|root| root.delay);
        
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

// Generates `MyDelayPlugin` trait implementations and bindings magically!
export_clap_plugin!(
    MyDelayPlugin, 
    MyDelayPluginAudioProcessor, 
    "com.example.rust-mixer-delay", 
    "Rust Mixer Configurable Delay"
);
