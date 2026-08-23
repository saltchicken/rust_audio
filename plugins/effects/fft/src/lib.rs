use clack_plugin::prelude::*;
use plugin_core::export_clap_plugin;
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::f32::consts::PI;
use std::sync::Arc;

// 4096 samples provides a good balance between frequency resolution (~11Hz at 48kHz) and real-time responsiveness.
const FFT_SIZE: usize = 4096;

pub struct FftAnalyzerProcessor {
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<f32>,
    complex_buffer: Vec<Complex<f32>>,
    window: Vec<f32>,
    cursor: usize,
    sample_rate: f32,
}

impl<'a> PluginAudioProcessor<'a, (), ()> for FftAnalyzerProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut (),
        _shared: &'a (),
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Pre-calculate a Hanning window to prevent spectral leakage
        let mut window = vec![0.0; FFT_SIZE];
        for i in 0..FFT_SIZE {
            window[i] = 0.5 * (1.0 - (2.0 * PI * i as f32 / (FFT_SIZE as f32 - 1.0)).cos());
        }

        Ok(Self {
            fft,
            buffer: vec![0.0; FFT_SIZE],
            complex_buffer: vec![Complex { re: 0.0, im: 0.0 }; FFT_SIZE],
            window,
            cursor: 0,
            sample_rate: audio_config.sample_rate as f32,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for mut port_pair in audio.port_pairs() {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };

            // Process the first channel for analysis, but pass through all channels safely
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                match channel_pair {
                    ChannelPair::InputOutput(input, output) => {
                        for (inp, out) in input.iter().zip(output.iter_mut()) {
                            *out = *inp; // Pass audio through unaltered
                            if ch_idx == 0 {
                                self.push_sample(*inp);
                            }
                        }
                    }
                    ChannelPair::InPlace(buffer) => {
                        if ch_idx == 0 {
                            for &sample in buffer.iter() {
                                self.push_sample(sample);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

impl FftAnalyzerProcessor {
    fn push_sample(&mut self, sample: f32) {
        self.buffer[self.cursor] = sample;
        self.cursor += 1;

        if self.cursor >= FFT_SIZE {
            self.analyze();
            self.cursor = 0;
        }
    }

    fn analyze(&mut self) {
        // Apply the window function and populate the complex buffer
        for i in 0..FFT_SIZE {
            self.complex_buffer[i] = Complex {
                re: self.buffer[i] * self.window[i],
                im: 0.0,
            };
        }

        // Perform the FFT in-place
        self.fft.process(&mut self.complex_buffer);

        let mut max_mag = 0.0;
        let mut peak_bin = 0;

        // Search only the positive frequencies, skipping DC offset (bin 0)
        for i in 1..FFT_SIZE / 2 {
            let mag = self.complex_buffer[i].norm();
            if mag > max_mag {
                max_mag = mag;
                peak_bin = i;
            }
        }

        if max_mag > 10.0 {
            // Apply Parabolic Interpolation to find the exact sub-bin peak
            let exact_bin = if peak_bin > 0 && peak_bin < (FFT_SIZE / 2 - 1) {
                let y1 = self.complex_buffer[peak_bin - 1].norm();
                let y2 = max_mag;
                let y3 = self.complex_buffer[peak_bin + 1].norm();

                let denominator = y1 - 2.0 * y2 + y3;
                let offset = if denominator != 0.0 {
                    0.5 * (y1 - y3) / denominator
                } else {
                    0.0
                };
                
                peak_bin as f32 + offset
            } else {
                peak_bin as f32
            };

            // Calculate the exact frequency from our interpolated bin
            let freq = (exact_bin * self.sample_rate) / (FFT_SIZE as f32);
            
            // Convert frequency to MIDI note number
            let midi_float = 69.0 + 12.0 * (freq / 440.0).log2();
            let midi_note = midi_float.round() as i32;
            
            // Map the MIDI note to a pitch class (C, C#, etc.) and octave
            let notes = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
            
            // rem_euclid correctly wraps negative numbers if the note is below C0
            let pitch_class = notes[midi_note.rem_euclid(12) as usize];
            let octave = (midi_note / 12) - 1;
            
            // The \r resets the cursor in raw terminal mode
            println!("Main Frequency: {:.2} Hz (Note: {}{})         \r", freq, pitch_class, octave);
        }
    }
}

export_clap_plugin!(
    FftPlugin,
    FftAnalyzerProcessor,
    "com.example.rust-mixer-fft",
    "FFT Analyzer"
);
