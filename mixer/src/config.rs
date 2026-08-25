use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackConfig {
    pub name: String,
    pub midi_channel: Option<u8>,
    pub enable_live_input: bool,
    pub plugin_chain: Vec<String>,
    #[serde(default)]
    pub plugins: toml::Table,
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
                midi_channel: None,
                enable_live_input: false,
                plugin_chain: vec![],
                plugins: toml::Table::new(),
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
