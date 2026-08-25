use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackConfig {
    pub name: String,
    pub active_preset: Option<String>,
    pub midi_channel: Option<u8>,
    pub enable_live_input: bool,
    pub plugin_chain: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MixerConfig {
    pub master_volume: Option<f32>,
    pub sample_rate: u32,
    pub latency_ms: f32,
    pub capacity_seconds: f32,
    pub input_device: String,
    pub output_device: String,
    pub track: Vec<TrackConfig>,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            master_volume: Some(1.0),
            sample_rate: 48000,
            latency_ms: 2.0,
            capacity_seconds: 0.5,
            input_device: "default".to_string(),
            output_device: "default".to_string(),
            track: vec![TrackConfig {
                name: "Default Track".to_string(),
                active_preset: Some("epic".to_string()),
                midi_channel: None,
                enable_live_input: false,
                plugin_chain: vec![],
            }],
        }
    }
}

pub fn load_or_create_config(path: &str) -> anyhow::Result<MixerConfig> {
    if let Ok(config_str) = fs::read_to_string(path) {
        let config: MixerConfig = toml::from_str(&config_str)?;
        Ok(config)
    } else {
        let default_config = MixerConfig::default();
        let toml_string = toml::to_string_pretty(&default_config)?;
        fs::write(path, toml_string)?;
        Ok(default_config)
    }
}

pub fn rotate_preset(config_path: &str, forward: bool) -> anyhow::Result<()> {
    let config_str = fs::read_to_string(config_path)?;
    let parsed: toml::Value = toml::from_str(&config_str)?;

    if let Some(presets) = parsed.get("global_presets").and_then(|v| v.as_table()) {
        let mut preset_names: Vec<String> = presets.keys().cloned().collect();
        preset_names.sort();

        if preset_names.is_empty() {
            return Ok(());
        }

        let mut new_config_str = String::new();

        for line in config_str.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("active_preset") {
                // Extract the current preset name
                let parts: Vec<&str> = line.split('=').collect();
                if parts.len() == 2 {
                    let current = parts[1].trim().trim_matches('"');
                    let current_idx = preset_names.iter().position(|n| n == current).unwrap_or(0);
                    
                    let next_idx = if forward {
                        (current_idx + 1) % preset_names.len()
                    } else {
                        (current_idx + preset_names.len() - 1) % preset_names.len()
                    };
                    
                    let next_preset = &preset_names[next_idx];
                    
                    // Preserve the exact indentation
                    let indent = line.chars().take_while(|c| c.is_whitespace()).collect::<String>();
                    new_config_str.push_str(&format!("{}active_preset = \"{}\"", indent, next_preset));
                } else {
                    new_config_str.push_str(line);
                }
            } else {
                new_config_str.push_str(line);
            }
            new_config_str.push('\n');
        }

        fs::write(config_path, new_config_str)?;
        println!("\r\n🔄 Rotated presets for all tracks\r");
    } else {
        println!("\r\n⚠️ No [global_presets] section found in config\r");
    }
    Ok(())
}
