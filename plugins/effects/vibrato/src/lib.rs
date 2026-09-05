use clack_plugin::events::event_types::MidiEvent;
use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;
use std::f32::consts::PI;

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct VibratoConfig {
    rate_hz: f32,  // Speed of the wobble
    depth_ms: f32, // Intensity of the pitch shift
    mix: f32,      // 1.0 = Pure Vibrato, 0.5 = Chorus
}

impl Default for VibratoConfig {
    fn default() -> Self {
        Self {
            rate_hz: 3.5,
            depth_ms: 2.0,
            mix: 1.0,
        }
    }
}

// --- 1. DSP Utilities ---

struct ModulatedDelay {
    buffer: Vec<f32>,
    write_index: usize,
    lfo_phase: f32,
    base_delay_ms: f32,
}

impl ModulatedDelay {
    fn new(sample_rate: f64) -> Self {
        let buffer_samples = ((20.0 / 1000.0) * sample_rate) as usize;
        Self {
            buffer: vec![0.0; buffer_samples.max(1)],
            write_index: 0,
            lfo_phase: 0.0,
            base_delay_ms: 5.0,
        }
    }

    fn process(&mut self, input: f32, sample_rate: f32, rate: f32, depth: f32, mix: f32) -> f32 {
        self.buffer[self.write_index] = input;

        self.lfo_phase += (rate * 2.0 * PI) / sample_rate;
        if self.lfo_phase > 2.0 * PI {
            self.lfo_phase -= 2.0 * PI;
        }

        let lfo_val = self.lfo_phase.sin();
        let current_delay_ms = self.base_delay_ms + (lfo_val * depth);
        let delay_samples = (current_delay_ms / 1000.0) * sample_rate;

        let mut read_index_f = self.write_index as f32 - delay_samples;
        if read_index_f < 0.0 {
            read_index_f += self.buffer.len() as f32;
        }

        let idx1 = read_index_f.trunc() as usize % self.buffer.len();
        let idx2 = (idx1 + 1) % self.buffer.len();
        let frac = read_index_f.fract();

        let delayed_sample = (self.buffer[idx1] * (1.0 - frac)) + (self.buffer[idx2] * frac);

        self.write_index = (self.write_index + 1) % self.buffer.len();

        (input * (1.0 - mix)) + (delayed_sample * mix)
    }
}

// --- 2. CLAP Plugin Implementation ---

pub struct MyVibratoPluginAudioProcessor {
    channels: Vec<ModulatedDelay>,
    sample_rate: f32,
    config: VibratoConfig,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyVibratoPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate;
        let channels = vec![ModulatedDelay::new(sr), ModulatedDelay::new(sr)];

        println!("    🎛️ Vibrato Loaded | CCs: 90 (Rate), 91 (Depth), 92 (Mix)\r");

        Ok(Self {
            channels,
            sample_rate: sr as f32,
            config: load_plugin_config::<VibratoConfig>("vibrato"),
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
                        90 => self.config.rate_hz = val * 10.0,
                        91 => self.config.depth_ms = val * 10.0,
                        92 => self.config.mix = val,
                        _ => {}
                    }
                }
            }
        }

        let config = &self.config;

        plugin_core::process_f32_channels(&mut audio, |ch_idx, input, output| {
            let dsp = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = dsp.process(
                    *i,
                    self.sample_rate,
                    config.rate_hz,
                    config.depth_ms,
                    config.mix,
                );
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyVibratoPlugin,
    MyVibratoPluginAudioProcessor,
    "com.example.rust-mixer-vibrato",
    "Rust Mixer Vibrato"
);
