use clack_plugin::prelude::*;
use plugin_core::{export_clap_plugin, load_plugin_config};
use serde::Deserialize;

// --- Configuration Structs ---

#[derive(Deserialize, Clone)]
#[serde(default)]
struct ReverbConfig {
    comb_lengths: [f64; 4],
    allpass_lengths: [f64; 2],
    comb_feedback: f32,
    comb_dampening: f32,
    allpass_feedback: f32,
    left_spread: usize,
    right_spread: usize,
    mix: f32,
    wet_scale: f32,
}

impl Default for ReverbConfig {
    fn default() -> Self {
        Self {
            comb_lengths: [1557.0, 1617.0, 1491.0, 1422.0],
            allpass_lengths: [225.0, 556.0],
            comb_feedback: 0.84,
            comb_dampening: 0.2,
            allpass_feedback: 0.5,
            left_spread: 0,
            right_spread: 23,
            mix: 0.4,
            wet_scale: 0.15,
        }
    }
}

// --- 1. DSP Utilities ---

struct DelayLine {
    buffer: Vec<f32>,
    index: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
        }
    }

    fn read(&self) -> f32 {
        self.buffer[self.index]
    }

    fn write_and_step(&mut self, value: f32) {
        self.buffer[self.index] = value;
        self.index = (self.index + 1) % self.buffer.len();
    }
}

struct CombFilter {
    delay: DelayLine,
    feedback: f32,
    dampening: f32,
    filter_store: f32,
}

impl CombFilter {
    fn new(len: usize, feedback: f32, dampening: f32) -> Self {
        Self {
            delay: DelayLine::new(len),
            feedback,
            dampening,
            filter_store: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.delay.read();
        self.filter_store =
            (output * (1.0 - self.dampening)) + (self.filter_store * self.dampening);
        self.delay
            .write_and_step(input + self.filter_store * self.feedback);
        output
    }
}

struct AllPassFilter {
    delay: DelayLine,
    feedback: f32,
}

impl AllPassFilter {
    fn new(len: usize, feedback: f32) -> Self {
        Self {
            delay: DelayLine::new(len),
            feedback,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.read();
        let output = -input * self.feedback + delayed;
        self.delay.write_and_step(input + delayed * self.feedback);
        output
    }
}

// --- 2. Reverb Structure ---

struct ReverbChannel {
    combs: [CombFilter; 4],
    allpasses: [AllPassFilter; 2],
    mix: f32,
    wet_scale: f32,
}

impl ReverbChannel {
    fn new(sample_rate: f64, stereo_spread: usize, config: &ReverbConfig) -> Self {
        let sr_scale = sample_rate / 44100.0;

        let c1 = (config.comb_lengths[0] * sr_scale) as usize + stereo_spread;
        let c2 = (config.comb_lengths[1] * sr_scale) as usize + stereo_spread;
        let c3 = (config.comb_lengths[2] * sr_scale) as usize + stereo_spread;
        let c4 = (config.comb_lengths[3] * sr_scale) as usize + stereo_spread;

        let a1 = (config.allpass_lengths[0] * sr_scale) as usize + stereo_spread;
        let a2 = (config.allpass_lengths[1] * sr_scale) as usize + stereo_spread;

        Self {
            combs: [
                CombFilter::new(c1, config.comb_feedback, config.comb_dampening),
                CombFilter::new(c2, config.comb_feedback, config.comb_dampening),
                CombFilter::new(c3, config.comb_feedback, config.comb_dampening),
                CombFilter::new(c4, config.comb_feedback, config.comb_dampening),
            ],
            allpasses: [
                AllPassFilter::new(a1, config.allpass_feedback),
                AllPassFilter::new(a2, config.allpass_feedback),
            ],
            mix: config.mix,
            wet_scale: config.wet_scale,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut out = 0.0;

        for comb in &mut self.combs {
            out += comb.process(input);
        }

        for allpass in &mut self.allpasses {
            out = allpass.process(out);
        }

        (input * (1.0 - self.mix)) + (out * self.mix * self.wet_scale)
    }
}

// --- 3. CLAP Plugin Implementation ---

pub struct MyReverbPluginAudioProcessor {
    channels: Vec<ReverbChannel>,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for MyReverbPluginAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate;
        let config = load_plugin_config::<ReverbConfig>("reverb");

        let channels = vec![
            ReverbChannel::new(sr, config.left_spread, &config),
            ReverbChannel::new(sr, config.right_spread, &config),
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
            let reverb = if ch_idx < self.channels.len() {
                &mut self.channels[ch_idx]
            } else {
                &mut self.channels[0]
            };

            for (i, o) in input.iter().zip(output.iter_mut()) {
                *o = reverb.process(*i);
            }
        });

        Ok(ProcessStatus::Continue)
    }
}

export_clap_plugin!(
    MyReverbPlugin,
    MyReverbPluginAudioProcessor,
    "com.example.rust-mixer-reverb",
    "Rust Mixer Configurable Reverb"
);
