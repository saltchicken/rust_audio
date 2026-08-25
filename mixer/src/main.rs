mod config;
mod engine;
mod host;
mod midi;

use crate::config::load_or_create_config;
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
    let mut cli_midi_channel: Option<u8> = None;
    let mut config_path = "config.toml".to_string();

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--midi-channel" || args[i] == "-c" {
            if i + 1 < args.len() {
                if let Ok(ch) = args[i + 1].parse::<u8>() {
                    if ch <= 15 {
                        cli_midi_channel = Some(ch);
                    } else {
                        eprintln!("⚠️ MIDI channel must be between 0 and 15");
                    }
                }
                i += 1;
            }
        } else if args[i] == "--config" {
            if i + 1 < args.len() {
                config_path = args[i + 1].clone();
                i += 1;
            }
        }
        i += 1;
    }

    enable_raw_mode()?;
    let _guard = RawModeGuard;

    loop {
        let mut app_config = load_or_create_config(&config_path)?;
        
        if cli_midi_channel.is_some() {
            app_config.midi_channel = cli_midi_channel;
        }

        let mut engine = AudioEngine::new(app_config, config_path.clone());

        match engine.run() {
            Ok(true) => continue,
            Ok(false) => break,
            Err(e) => {
                eprintln!("\r\n❌ Engine error: {:?}\r", e);
                break;
            }
        }
    }

    Ok(())
}
