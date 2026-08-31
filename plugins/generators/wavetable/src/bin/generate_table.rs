// plugins/generators/wavetable/src/bin/generate_table.rs
use hound;
use std::f32::consts::PI;
use std::fs;

fn main() {
    let frame_size = 2048;
    let num_frames = 16;
    
    // 32-bit float is ideal for synth wavetables to prevent clipping/quantization noise
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44100, // The samplerate here doesn't matter for wavetables, just the frame size
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    
    // Ensure the samples directory exists relative to the project root
    let _ = fs::create_dir_all("samples");
    let path = "samples/morphing_sync.wav";
    
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    
    for frame in 0..num_frames {
        let morph = frame as f32 / (num_frames - 1) as f32; // Progresses from 0.0 to 1.0
        
        for i in 0..frame_size {
            let phase = (i as f32 / frame_size as f32) * 2.0 * PI;
            
            // Texture 1: Pure Sine
            let sine = phase.sin();
            
            // Texture 2: Hollow Square (Odd harmonics)
            let mut square = 0.0;
            for k in (1..=11).step_by(2) {
                square += (phase * k as f32).sin() / k as f32;
            }
            // Normalize square roughly
            square *= 1.2;
            
            // Texture 3: Aggressive Sweeping Sync
            // The "sync_freq" sweeps higher as we move through the later frames
            let sync_freq = 1.0 + (morph * 12.0); 
            let sync = (phase.sin() * (phase * sync_freq).cos()).tanh() * 1.5;
            
            // Crossfade logic based on our current frame position
            let sample = if morph < 0.5 {
                // First half of the wavetable: Fade Sine into Square
                let mix = morph * 2.0;
                sine * (1.0 - mix) + square * mix
            } else {
                // Second half of the wavetable: Fade Square into Sync
                let mix = (morph - 0.5) * 2.0;
                square * (1.0 - mix) + sync * mix
            };
            
            // Soft clip to prevent any mathematical overs
            let safe_sample = sample.tanh();
            
            writer.write_sample(safe_sample).unwrap();
        }
    }
    
    writer.finalize().unwrap();
    println!("✅ Generated {} frames of {} samples.", num_frames, frame_size);
    println!("✅ Wavetable saved to '{}'", path);
}
