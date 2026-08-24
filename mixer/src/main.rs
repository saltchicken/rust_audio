mod config;
mod engine;
mod host;
mod midi;

use crate::config::load_or_create_config;
use crate::engine::AudioEngine;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn main() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let _guard = RawModeGuard;

    loop {
        let config_path = "config.toml";
        let app_config = load_or_create_config(config_path)?;
        let mut engine = AudioEngine::new(app_config);

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
