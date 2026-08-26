use clack_plugin::prelude::*;
use serde::de::DeserializeOwned;
use std::env;
use std::fs;

// --- 1. Global Configuration Extractor ---

fn get_preset_path() -> String {
    // 1. Prioritize hot-swapped preset path injected by the audio engine
    if let Ok(path) = env::var("CURRENT_PRESET_PATH") {
        return path;
    }

    // 2. Fallback to CLI args (useful if you ever debug plugins standalone)
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--preset" && i + 1 < args.len() {
            return args[i + 1].clone();
        }
        i += 1;
    }
    "presets/default.toml".to_string() // Fallback
}

pub fn load_plugin_config<T>(plugin_name: &str) -> T
where
    T: DeserializeOwned + Default + Clone,
{
    let config_path = get_preset_path();
    let config_str = fs::read_to_string(&config_path).unwrap_or_default();

    // The host injects the track's index into the environment right before activation
    let track_idx_str = env::var("CURRENT_TRACK_INDEX").unwrap_or_default();
    let track_idx: usize = track_idx_str.parse().unwrap_or(0);

    if let Ok(global_cfg) = config_str.parse::<toml::Value>() {
        if let Some(tracks) = global_cfg.get("track").and_then(|t| t.as_array()) {
            if let Some(track) = tracks.get(track_idx) {
                if let Some(plugin_data) = track.get("plugins").and_then(|p| p.get(plugin_name)) {
                    if let Ok(config) = plugin_data.clone().try_into::<T>() {
                        return config;
                    }
                }
            }
        }
    }

    println!(
        "Warning: Config for plugin '{}' on track index '{}' not found in '{}', using default.",
        plugin_name, track_idx, config_path
    );
    T::default()
}

// --- 2. CLAP Audio Processing Abstraction ---

pub fn process_f32_channels(
    audio: &mut Audio,
    mut process_channel: impl FnMut(usize, &[f32], &mut [f32]),
) {
    for mut port_pair in audio.port_pairs() {
        let Some(channel_pairs) = port_pair.channels().ok().and_then(|c| c.into_f32()) else {
            continue;
        };

        for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
            match channel_pair {
                ChannelPair::InputOnly(_) => {}
                ChannelPair::OutputOnly(buf) => {
                    buf.fill(0.0);
                    process_channel(ch_idx, &[], buf);
                }
                ChannelPair::InputOutput(input, output) => {
                    process_channel(ch_idx, input, output);
                }
                ChannelPair::InPlace(buf) => {
                    const CHUNK_SIZE: usize = 4096;
                    let mut tmp = [0.0f32; CHUNK_SIZE];
                    for chunk in buf.chunks_mut(CHUNK_SIZE) {
                        let len = chunk.len();
                        tmp[..len].copy_from_slice(chunk);
                        process_channel(ch_idx, &tmp[..len], chunk);
                    }
                }
            }
        }
    }
}

// --- 3. CLAP Boilerplate Macro ---

#[macro_export]
macro_rules! export_clap_plugin {
    ($plugin_type:ident, $processor_type:ty, $id:expr, $name:expr) => {
        pub struct $plugin_type;

        impl Plugin for $plugin_type {
            type AudioProcessor<'a> = $processor_type;
            type Shared<'a> = ();
            type MainThread<'a> = ();
        }

        impl DefaultPluginFactory for $plugin_type {
            fn get_descriptor() -> PluginDescriptor {
                PluginDescriptor::new($id, $name)
            }

            fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
                Ok(())
            }

            fn new_main_thread<'a>(
                _host: HostMainThreadHandle<'a>,
                _shared: &'a Self::Shared<'a>,
            ) -> Result<Self::MainThread<'a>, PluginError> {
                Ok(())
            }
        }

        clack_export_entry!(SinglePluginEntry<$plugin_type>);
    };
}
