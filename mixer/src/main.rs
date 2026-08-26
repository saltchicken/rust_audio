mod config;
mod engine;
mod host;
mod midi;

use crate::config::load_config_and_preset;
use crate::engine::AudioEngine;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::env;

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let mut config_path = "config.toml".to_string();
    let mut preset_path = "presets/default.toml".to_string();

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            config_path = args[i + 1].clone();
            i += 1;
        } else if args[i] == "--preset" && i + 1 < args.len() {
            preset_path = args[i + 1].clone();
            i += 1;
        }
        i += 1;
    }

    enable_raw_mode()?;
    let _guard = RawModeGuard;

    loop {
        let app_config = load_config_and_preset(&config_path, &preset_path)?;
        let mut engine = AudioEngine::new(app_config);

        match engine.run(&preset_path) {
            Ok(Some(new_preset)) => {
                preset_path = new_preset;
                continue; // Hot-swap triggered, engine reboots with new preset!
            }
            Ok(None) => break, // Quit signal
            Err(e) => {
                eprintln!("\r\n❌ Engine error: {:?}\r", e);
                break;
            }
        }
    }

    Ok(())
}
