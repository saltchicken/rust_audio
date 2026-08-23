use clack_plugin::prelude::*;

// --- 1. DSP Utilities ---

struct EchoDelay {
    buffer: Vec<f32>,
    index: usize,
    feedback: f32,
    mix: f32,
}

impl EchoDelay {
    fn new(sample_rate: f64, delay_ms: f64) -> Self {
        // Calculate how many samples long our delay needs to be
        let delay_samples = ((delay_ms / 1000.0) * sample_rate) as usize;
        
        Self {
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
            feedback: 0.65, // 65% feedback for a long, obvious trail
            mix: 0.5,       // 50/50 Wet/Dry mix
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        // Read the delayed sample from the buffer
        let delayed = self.buffer[self.index];
        
        // Write the new input mixed with the feedback back into the buffer
        self.buffer[self.index] = input + (delayed * self.feedback);
        
        // Advance the circular buffer index
        self.index = (self.index + 1) % self.buffer.len();

        // Return the mixed signal
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
            "Rust Mixer Obvious Delay"
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
        
        // Create 2 independent delays for stereo
        // 400ms on the Left, 530ms on the Right for a wide, bouncing echo
        let channels = vec![
            EchoDelay::new(sr, 400.0), // Left
            EchoDelay::new(sr, 530.0), // Right
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
                // Route to left or right delay processor
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
