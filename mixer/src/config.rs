use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug)]
pub struct MixerConfig {
    pub active_global_preset: Option<String>,
    pub master_volume: Option<f32>,
    pub sample_rate: Option<u32>,
    pub enable_live_input: bool,
    pub latency_ms: f32,
    pub capacity_seconds: f32,
    pub plugin_chain: Vec<String>,
    pub input_device: String,
    pub output_device: String,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            active_global_preset: None,
            master_volume: Some(1.0),
            sample_rate: None,
            enable_live_input: false,
            latency_ms: 2.0,
            capacity_seconds: 0.5,
            plugin_chain: vec![],
            input_device: "default".to_string(),
            output_device: "default".to_string(),
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

pub fn rotate_preset(forward: bool) -> anyhow::Result<()> {
    let config_path = "config.toml";
    let config_str = fs::read_to_string(config_path)?;
    let parsed: toml::Value = toml::from_str(&config_str)?;

    if let Some(presets) = parsed.get("global_presets").and_then(|v| v.as_table()) {
        let mut preset_names: Vec<String> = presets.keys().cloned().collect();
        preset_names.sort();

        if preset_names.is_empty() {
            return Ok(());
        }

        let current = parsed
            .get("active_global_preset")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let current_idx = preset_names.iter().position(|n| n == current).unwrap_or(0);

        let next_idx = if forward {
            (current_idx + 1) % preset_names.len()
        } else {
            (current_idx + preset_names.len() - 1) % preset_names.len()
        };

        let next_preset = &preset_names[next_idx];
        let mut new_config_str = String::new();

        for line in config_str.lines() {
            if line.trim_start().starts_with("active_global_preset") {
                new_config_str.push_str(&format!("active_global_preset = \"{}\"", next_preset));
            } else {
                new_config_str.push_str(line);
            }
            new_config_str.push('\n');
        }

        fs::write(config_path, new_config_str)?;
        println!("\r\n🔄 Switched to preset '{}'\r", next_preset);
    } else {
        println!("\r\n⚠️ No [global_presets] section found in config.toml\r");
    }
    Ok(())
}
