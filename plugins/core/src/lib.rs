use serde::Deserialize;
use std::collections::HashMap;
use std::fs;

// --- 1. Global Configuration Extractor ---

#[derive(Deserialize, Default)]
struct GlobalConfig<R> {
    active_global_preset: Option<String>,
    global_presets: Option<HashMap<String, R>>,
}

pub fn load_plugin_config<R, F, T>(extract_section: F) -> T
where
    R: for<'a> Deserialize<'a> + Default,
    F: Fn(&R) -> Option<&T>,
    T: Clone + Default,
{
    let config_str = fs::read_to_string("config.toml").unwrap_or_default();
    let global_cfg: GlobalConfig<R> = toml::from_str(&config_str).unwrap_or_default();

    if let Some(global_name) = &global_cfg.active_global_preset {
        if !global_name.is_empty() {
            if let Some(global_presets) = &global_cfg.global_presets {
                if let Some(preset_root) = global_presets.get(global_name) {
                    if let Some(config) = extract_section(preset_root) {
                        return config.clone();
                    }
                }
            }
            println!("Warning: Global preset '{}' not found or missing section, using default.", global_name);
        }
    }

    T::default()
}

// --- 2. CLAP Boilerplate Macro ---

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
