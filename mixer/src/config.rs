use anyhow::Context;
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
pub struct EngineConfig {
    pub master_volume: Option<f32>,
    pub sample_rate: u32,
    pub latency_ms: f32,
    pub capacity_seconds: f32,
    pub input_device: String,
    pub output_device: String,
    pub default_preset: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Preset {
    pub track: Vec<TrackConfig>,
}

#[derive(Debug, Clone)]
pub struct MixerConfig {
    pub master_volume: Option<f32>,
    pub sample_rate: u32,
    pub latency_ms: f32,
    pub capacity_seconds: f32,
    pub input_device: String,
    pub output_device: String,
    pub track: Vec<TrackConfig>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            master_volume: Some(1.0),
            sample_rate: 48000,
            latency_ms: 2.0,
            capacity_seconds: 0.5,
            input_device: "default".to_string(),
            output_device: "default".to_string(),
            default_preset: Some("presets/default.toml".to_string()),
        }
    }
}

pub fn load_config_and_preset(config_path: &str, preset_path: Option<&str>) -> anyhow::Result<(MixerConfig, String)> {
    let engine_config: EngineConfig = if let Ok(config_str) = fs::read_to_string(config_path) {
        toml::from_str(&config_str).context("Failed to parse engine config.toml")?
    } else {
        let default_config = EngineConfig::default();
        let toml_string = toml::to_string_pretty(&default_config)?;
        fs::write(config_path, toml_string)?;
        default_config
    };

    let resolved_preset_path = preset_path
        .map(|s| s.to_string())
        .or_else(|| engine_config.default_preset.clone())
        .unwrap_or_else(|| "presets/default.toml".to_string());

    let preset_str = fs::read_to_string(&resolved_preset_path)
        .with_context(|| format!("Failed to find preset file: {}", resolved_preset_path))?;
    let preset: Preset = toml::from_str(&preset_str)
        .with_context(|| format!("Failed to parse preset file: {}", resolved_preset_path))?;

    Ok((
        MixerConfig {
            master_volume: engine_config.master_volume,
            sample_rate: engine_config.sample_rate,
            latency_ms: engine_config.latency_ms,
            capacity_seconds: engine_config.capacity_seconds,
            input_device: engine_config.input_device,
            output_device: engine_config.output_device,
            track: preset.track,
        },
        resolved_preset_path,
    ))
}
