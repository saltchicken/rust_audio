use clack_plugin::events::event_types::MidiEvent;
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;

// --- Configuration Structs ---

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
        let pre_gain = input * drive.max(0.1);
        let saturated = pre_gain.tanh();

        let alpha = 0.05 + (tone * 0.95);
        self.tone_state = self.tone_state + alpha * (saturated - self.tone_state);

        self.tone_state * level
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyAmpPluginAudioProcessor {
    channels: Vec<Amplifier>,
    config: AmpConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyAmpPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            channels: vec![Amplifier::new(), Amplifier::new()],
            config: load_plugin_config::<AmpConfig>("amp"),
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
                    let val = data[2] as f32 / 127.0;
                    match cc {
                        70 => self.config.drive = 0.1 + val * 9.9,
                        76 => self.config.tone = val,
                        77 => self.config.level = val * 2.0,
                        _ => {}
                    }
                }
            }
        }

        let config = &self.config;

        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let amp = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = amp.process(*i, config.drive, config.tone, config.level);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyAmpPlugin,
    MyAmpPluginAudioProcessor,
    "com.example.rust-mixer-amp",
    "Rust Mixer Amp Sim"
);
