use clack_plugin::prelude::*;

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

// Comb filter: Delay with feedback and a low-pass filter for dampening
struct CombFilter {
    delay: DelayLine,
    feedback: f32,
    dampening: f32,
    filter_store: f32,
}

impl CombFilter {
    fn new(len: usize) -> Self {
        Self {
            delay: DelayLine::new(len),
            feedback: 0.84, // High feedback for long tails
            dampening: 0.2, // Dampens high frequencies over time
            filter_store: 0.0,
        }
    }
    
    fn process(&mut self, input: f32) -> f32 {
        let output = self.delay.read();
        self.filter_store = (output * (1.0 - self.dampening)) + (self.filter_store * self.dampening);
        self.delay.write_and_step(input + self.filter_store * self.feedback);
        output
    }
}

// All-pass filter: Modifies phase to "smear" the echoes and increase density
struct AllPassFilter {
    delay: DelayLine,
    feedback: f32,
}

impl AllPassFilter {
    fn new(len: usize) -> Self {
        Self {
            delay: DelayLine::new(len),
            feedback: 0.5,
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
}

impl ReverbChannel {
    fn new(sample_rate: f64, stereo_spread: usize) -> Self {
        let sr_scale = sample_rate / 44100.0;
        
        // Prime-ish delay lengths (scaled to sample rate) + stereo spread to widen the image
        let c1 = (1557.0 * sr_scale) as usize + stereo_spread;
        let c2 = (1617.0 * sr_scale) as usize + stereo_spread;
        let c3 = (1491.0 * sr_scale) as usize + stereo_spread;
        let c4 = (1422.0 * sr_scale) as usize + stereo_spread;

        let a1 = (225.0 * sr_scale) as usize + stereo_spread;
        let a2 = (556.0 * sr_scale) as usize + stereo_spread;

        Self {
            combs: [
                CombFilter::new(c1), CombFilter::new(c2),
                CombFilter::new(c3), CombFilter::new(c4),
            ],
            allpasses: [
                AllPassFilter::new(a1), AllPassFilter::new(a2),
            ],
            mix: 0.4, // Wet/Dry mix
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut out = 0.0;
        
        // 1. Run combs in parallel
        for comb in &mut self.combs {
            out += comb.process(input);
        }
        
        // 2. Run all-passes in series
        for allpass in &mut self.allpasses {
            out = allpass.process(out);
        }
        
        // 3. Output Wet/Dry mix (Scale wet signal down slightly to prevent clipping)
        (input * (1.0 - self.mix)) + (out * self.mix * 0.15)
    }
}

// --- 3. CLAP Plugin Implementation ---

pub struct MyReverbPlugin;

impl Plugin for MyReverbPlugin {
    type AudioProcessor<'a> = MyReverbPluginAudioProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = ();
}

impl DefaultPluginFactory for MyReverbPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "com.example.rust-mixer-reverb", 
            "Rust Mixer Reverb Effect"
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
        
        // Create 2 independent reverb networks for stereo. 
        // We add a "spread" value to the right channel so the echoes decorrelate, sounding much wider.
        let channels = vec![
            ReverbChannel::new(sr, 0),  // Left
            ReverbChannel::new(sr, 23), // Right (offsets all delay lines by 23 samples)
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
                // Pick the correct reverb state for Left or Right channel
                let reverb = if ch_idx < self.channels.len() {
                    &mut self.channels[ch_idx]
                } else {
                    &mut self.channels[0]
                };

                match channel_pair {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        for (i, o) in input.iter().zip(output.iter_mut()) {
                            *o = reverb.process(*i);
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        for sample in buf.iter_mut() {
                            *sample = reverb.process(*sample);
                        }
                    }
                }
            }
        }
        
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<MyReverbPlugin>);
