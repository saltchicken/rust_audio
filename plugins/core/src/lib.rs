use clack_plugin::prelude::*;
use serde::de::DeserializeOwned;
use std::env;
use std::fs;

// --- 1. Global Configuration Extractor ---

// Helper to read the host's CLI args from within the plugin
fn get_cli_config_path() -> String {
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            return args[i + 1].clone();
        }
        i += 1;
    }
    "config.toml".to_string() // Fallback
}

pub fn load_plugin_config<T>(plugin_name: &str) -> T
where
    T: DeserializeOwned + Default + Clone,
{
    let config_path = get_cli_config_path();
    let config_str = fs::read_to_string(&config_path).unwrap_or_default();

    // Parse as an untyped toml Value to navigate dynamically
    if let Ok(global_cfg) = config_str.parse::<toml::Value>() {
        if let Some(global_name) = global_cfg
            .get("active_global_preset")
            .and_then(|v| v.as_str())
        {
            if !global_name.is_empty() {
                if let Some(preset_data) = global_cfg
                    .get("global_presets")
                    .and_then(|p| p.get(global_name))
                    .and_then(|g| g.get(plugin_name))
                {
                    // Attempt to deserialize the specific section into the requested struct
                    if let Ok(config) = preset_data.clone().try_into::<T>() {
                        return config;
                    }
                }
            }
        }
    }

    println!(
        "Warning: Global preset for '{}' not found or missing section, using default.",
        plugin_name
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
                    // Eliminate heap allocation (buf.to_vec()) in the real-time audio thread.
                    // Process in chunks utilizing a stack-allocated buffer.
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
