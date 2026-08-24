use clack_plugin::prelude::*;
use serde::Deserialize;
use plugin_core::{export_clap_plugin, load_plugin_config};

// --- Configuration Structs ---

#[derive(Deserialize, Default)]
struct RootConfig {
    amp: Option<AmpConfig>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct AmpConfig {
    drive: f32,
    tone: f32, // 0.0 (dark) to 1.0 (bright)
    level: f32,
}

impl Default for AmpConfig {
    fn default() -> Self {
        Self {
            drive: 1.0,
            tone: 0.5,
            level: 1.0,
        }
    }
}

// --- 1. DSP Utilities ---

struct Amplifier {
    tone_state: f32,
}

impl Amplifier {
    fn new() -> Self {
        Self { tone_state: 0.0 }
    }

    fn process(&mut self, input: f32, drive: f32, tone: f32, level: f32) -> f32 {
        // 1. Input Gain (Drive)
        let pre_gain = input * drive.max(0.1);
        
        // 2. Non-linear saturation (Waveshaping)
        // tanh provides classic symmetrical soft-clipping (analog tube/tape style)
        let saturated = pre_gain.tanh();

        // 3. Simple 1-pole Low-Pass Filter for the Tone knob
        // Map tone (0.0 - 1.0) to a smoothing factor (alpha)
        let alpha = 0.05 + (tone * 0.95); 
        self.tone_state = self.tone_state + alpha * (saturated - self.tone_state);

        // 4. Output Volume
        self.tone_state * level
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyAmpPluginAudioProcessor {
    channels: Vec<Amplifier>,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyAmpPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { 
            channels: vec![Amplifier::new(), Amplifier::new()] 
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let config = load_plugin_config::<RootConfig, _, _>(|root| root.amp.as_ref());

        for mut port_pair in audio.port_pairs() {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue; };
            
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                let amp = if ch_idx < self.channels.len() {
                    &mut self.channels[ch_idx]
                } else {
                    &mut self.channels[0]
                };

                match channel_pair {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        for (i, o) in input.iter().zip(output.iter_mut()) {
                            *o = amp.process(*i, config.drive, config.tone, config.level);
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        for sample in buf.iter_mut() {
                            *sample = amp.process(*sample, config.drive, config.tone, config.level);
                        }
                    }
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyAmpPlugin, 
    MyAmpPluginAudioProcessor, 
    "com.example.rust-mixer-amp", 
    "Rust Mixer Amp Sim"
);
