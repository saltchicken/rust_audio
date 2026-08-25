use clack_plugin::prelude::*;
use serde::de::DeserializeOwned;
use std::env;
use std::fs;

// --- 1. Global Configuration Extractor ---

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
    
    // The host injects the track's index into the environment right before activation
    let track_idx_str = env::var("CURRENT_TRACK_INDEX").unwrap_or_default();
    let track_idx: usize = track_idx_str.parse().unwrap_or(0);

    if let Ok(global_cfg) = config_str.parse::<toml::Value>() {
        if let Some(tracks) = global_cfg.get("track").and_then(|t| t.as_array()) {
            if let Some(track) = tracks.get(track_idx) {
                if let Some(plugin_data) = track
                    .get("plugins")
                    .and_then(|p| p.get(plugin_name))
                {
                    if let Ok(config) = plugin_data.clone().try_into::<T>() {
                        return config;
                    }
                }
            }
        }
    }

    println!(
        "Warning: Config for plugin '{}' on track index '{}' not found, using default.",
        plugin_name, track_idx
    );
    T::default()
}

// --- 2. CLAP Audio Processing Abstraction ---

pub fn process_f32_channels(
    audio: &mut Audio,
    mut process_channel: impl FnMut(usize, &[f32], &mut [f32]),
) {
    // Thread-local vector gracefully expands dynamically avoiding the chunking loop
    // bug while obeying strict Rust aliasing boundaries. No re-allocation happens
    // on the real-time audio thread after the first capacity expansion.
    thread_local! {
        static TMP_BUF: std::cell::RefCell<Vec<f32>> = std::cell::RefCell::new(Vec::new());
    }

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
                    TMP_BUF.with(|tmp| {
                        let mut tmp_ref = tmp.borrow_mut();
                        let len = buf.len();
                        
                        // Extend our lock-free buffer if the block request gets larger
                        if tmp_ref.len() < len {
                            tmp_ref.resize(len, 0.0);
                        }
                        
                        // Copy entire buffer to satisfy aliasing constraint safely 
                        // completely bypassing the flawed 4096 framing chunk system.
                        tmp_ref[..len].copy_from_slice(buf);
                        process_channel(ch_idx, &tmp_ref[..len], buf);
                    });
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
