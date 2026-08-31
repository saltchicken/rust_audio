use hound;
use std::f32::consts::PI;
use std::fs;
use std::path::Path;

fn main() {
    let frame_size = 2048;
    let num_frames = 16;
    
    // Automatically find the workspace root using CARGO_MANIFEST_DIR
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .ancestors()
        .nth(3)
        .expect("Failed to locate workspace root");
    
    let samples_dir = workspace_root.join("samples");
    fs::create_dir_all(&samples_dir).expect("Failed to create samples directory");
    
    let path = samples_dir.join("morphing_sync.wav");
    
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44100,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    
    for frame in 0..num_frames {
        let morph = frame as f32 / (num_frames - 1) as f32;
        
        for i in 0..frame_size {
            let phase = (i as f32 / frame_size as f32) * 2.0 * PI;
            
            let sine = phase.sin();
            
            let mut square = 0.0;
            for k in (1..=11).step_by(2) {
                square += (phase * k as f32).sin() / k as f32;
            }
            square *= 1.2;
            
            let sync_freq = 1.0 + (morph * 12.0); 
            let sync = (phase.sin() * (phase * sync_freq).cos()).tanh() * 1.5;
            
            let sample = if morph < 0.5 {
                let mix = morph * 2.0;
                sine * (1.0 - mix) + square * mix
            } else {
                let mix = (morph - 0.5) * 2.0;
                square * (1.0 - mix) + sync * mix
            };
            
            writer.write_sample(sample.tanh()).unwrap();
        }
    }
    
    writer.finalize().unwrap();
    println!("✅ Generated {} frames of {} samples.", num_frames, frame_size);
    println!("✅ Wavetable saved to '{}'", path.display());
}
