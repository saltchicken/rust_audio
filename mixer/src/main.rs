use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;

use clack_host::prelude::*;

#[derive(Serialize, Deserialize, Debug)]
struct MixerConfig {
    latency_ms: f32,
    capacity_seconds: f32,
    plugin_chain: Vec<String>,
    input_device: String,
    output_device: String,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            latency_ms: 2.0,
            capacity_seconds: 0.5,
            plugin_chain: vec![
                "../plugins/reverb/target/release/libexample_clap_plugin.so".to_string(),
            ],
            input_device: "default".to_string(),
            output_device: "default".to_string(),
        }
    }
}

fn load_or_create_config(path: &str) -> anyhow::Result<MixerConfig> {
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

struct MixerHost;

impl HostHandlers for MixerHost {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

fn main() -> anyhow::Result<()> {
    let config_path = "config.toml";
    let app_config = load_or_create_config(config_path)?;
    let host = cpal::default_host();

    // 1. Bind to the configured Input Device
    let input_device = if app_config.input_device.to_lowercase() == "default" {
        host.default_input_device().expect("No default input device found")
    } else {
        host.input_devices()?
            .find(|d| d.name().map(|n| n == app_config.input_device).unwrap_or(false))
            .unwrap_or_else(|| panic!("Input device '{}' not found.", app_config.input_device))
    };

    // 2. Bind to the configured Output Device
    let output_device = if app_config.output_device.to_lowercase() == "default" {
        host.default_output_device().expect("No default output device found")
    } else {
        host.output_devices()?
            .find(|d| d.name().map(|n| n == app_config.output_device).unwrap_or(false))
            .unwrap_or_else(|| panic!("Output device '{}' not found.", app_config.output_device))
    };

    println!("✅ Bound to Input:  {}", input_device.name()?);
    println!("✅ Bound to Output: {}", output_device.name()?);

    let supported_input_config = input_device.default_input_config()?;
    let mut input_stream_config: cpal::StreamConfig = supported_input_config.clone().into();
    
    let supported_output_config = output_device.default_output_config()?;
    let mut output_stream_config: cpal::StreamConfig = supported_output_config.clone().into();

    if let cpal::SupportedBufferSize::Range { min, max: _ } = supported_input_config.buffer_size() {
        input_stream_config.buffer_size = cpal::BufferSize::Fixed((*min).max(64));
    }
    if let cpal::SupportedBufferSize::Range { min, max: _ } = supported_output_config.buffer_size() {
        output_stream_config.buffer_size = cpal::BufferSize::Fixed((*min).max(64));
    }

    let channels = input_stream_config.channels as usize;
    let latency_frames = (app_config.latency_ms / 1_000.0) * input_stream_config.sample_rate.0 as f32;
    let latency_samples = latency_frames as usize * channels;

    let capacity_frames = app_config.capacity_seconds * input_stream_config.sample_rate.0 as f32;
    let ring_capacity = capacity_frames as usize * channels;

    let ring = HeapRb::new(ring_capacity);
    let (mut producer, mut consumer) = ring.split();

    let padding = vec![0.0f32; latency_samples];
    producer.push_slice(&padding);

    let host_info = HostInfo::new("Rust Mixer", "My Company", "https://example.com", "0.1.0")?;
    let max_frames = 65536;
    let audio_config = PluginAudioConfiguration {
        sample_rate: input_stream_config.sample_rate.0 as f64,
        min_frames_count: 1,
        max_frames_count: max_frames as u32,
    };

    // Load all plugins in the chain
    let mut plugin_entries = Vec::new();
    let mut audio_processors = Vec::new();
    let mut _plugin_instances = Vec::new();

    for plugin_path in &app_config.plugin_chain {
        println!("✅ Loading CLAP from: {}", plugin_path);
        let entry = unsafe { PluginEntry::load(plugin_path) }?;
        let factory = entry.get_plugin_factory().expect("No plugin factory found");
        let descriptor = factory.plugin_descriptors().next().expect("No plugins found in CLAP");
        
        let mut plugin_instance = PluginInstance::<MixerHost>::new(
            |_| (), |_| (), &entry, descriptor.id().unwrap(), &host_info
        )?;

        let stopped_processor = plugin_instance.activate(|_, _| (), audio_config)?;
        let audio_processor = stopped_processor.start_processing()
            .map_err(|e| anyhow::anyhow!("Failed to start CLAP processor: {:?}", e))?;

        _plugin_instances.push(plugin_instance);
        plugin_entries.push(entry);
        audio_processors.push(audio_processor);
    }

    let err_fn = |err| eprintln!("Stream error: {}", err);

    let input_stream = input_device.build_input_stream(
        &input_stream_config,
        move |data: &[f32], _: &_| {
            let _ = producer.push_slice(data);
        },
        err_fn,
        None,
    )?;

    let mut intermediate_buffers = [
        vec![vec![0.0f32; max_frames as usize]; channels],
        vec![vec![0.0f32; max_frames as usize]; channels],
    ];
    let mut interleaved_in = vec![0.0f32; max_frames as usize * channels];

    let mut input_ports = AudioPorts::with_capacity(channels, 1);
    let mut output_ports = AudioPorts::with_capacity(channels, 1);
    let input_events_buffer = EventBuffer::new();
    let mut output_events_buffer = EventBuffer::new();

    let output_stream = output_device.build_output_stream(
        &output_stream_config,
        move |data: &mut [f32], _: &_| {
            let samples_to_read = data.len();
            let frames = samples_to_read / channels;

            let read = consumer.pop_slice(&mut interleaved_in[..samples_to_read]);
            interleaved_in[read..samples_to_read].fill(0.0);

            // De-interleave raw hardware input into Buffer 0
            for frame in 0..frames {
                for ch in 0..channels {
                    intermediate_buffers[0][ch][frame] = interleaved_in[frame * channels + ch];
                }
            }

            // If no plugins are loaded, just pass through Buffer 0
            if audio_processors.is_empty() {
                for frame in 0..frames {
                    for ch in 0..channels {
                        data[frame * channels + ch] = intermediate_buffers[0][ch][frame];
                    }
                }
                return;
            }

            let mut current_in_buf = 0;

            for audio_processor in audio_processors.iter_mut() {
                // Split the array borrow to yield distinct, mutable references for input and output
                let [ref mut buf_0, ref mut buf_1] = intermediate_buffers;
                let (in_buf, out_buf) = if current_in_buf == 0 {
                    (buf_0, buf_1)
                } else {
                    (buf_1, buf_0)
                };

                // Use iter_mut() and &mut for both due to the clack-host API enforcing AsMut
                let clap_input = input_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_buf.iter_mut().map(|c| InputChannel::constant(&mut c[..frames]))
                    )
                }]);

                let mut clap_output = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_buf.iter_mut().map(|c| &mut c[..frames])
                    )
                }]);

                let input_events = InputEvents::from_buffer(&input_events_buffer);
                let mut output_events = OutputEvents::from_buffer(&mut output_events_buffer);

                let _ = audio_processor.process(
                    &clap_input,
                    &mut clap_output,
                    &input_events,
                    &mut output_events,
                    Some(0),
                    None,
                );

                // Swap buffers for the next iteration (0 becomes 1, 1 becomes 0)
                current_in_buf = 1 - current_in_buf;
            }

            // Interleave final processed buffer back to the hardware output stream
            let final_buf = current_in_buf;
            for frame in 0..frames {
                for ch in 0..channels {
                    data[frame * channels + ch] = intermediate_buffers[final_buf][ch][frame];
                }
            }
        },
        err_fn,
        None,
    )?;

    println!("\nStreaming live at {}Hz with multiple CLAP plugins! (Press Ctrl+C to stop)", input_stream_config.sample_rate.0);
    input_stream.play()?;
    output_stream.play()?;

    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
