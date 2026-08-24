use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;

use clack_host::events::event_types::NoteOffEvent;
use clack_host::events::event_types::NoteOnEvent;
use clack_host::prelude::*;

use crossterm::event::{poll, read, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use midir::{Ignore, MidiInput};

#[derive(Serialize, Deserialize, Debug)]
struct MixerConfig {
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

struct MixerHost;

impl HostHandlers for MixerHost {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

struct RawModeGuard;
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[derive(Debug, Clone, Copy)]
enum MidiMsg {
    NoteOn(u8, f32),
    NoteOff(u8),
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

fn rotate_preset(forward: bool) -> anyhow::Result<()> {
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

struct AudioEngine {
    config: MixerConfig,
    host: cpal::Host,
}

impl AudioEngine {
    fn new(config: MixerConfig) -> Self {
        Self {
            config,
            host: cpal::default_host(),
        }
    }

    fn setup_output_device(&self) -> anyhow::Result<(cpal::Device, cpal::StreamConfig)> {
        let output_device = if self.config.output_device.to_lowercase() == "default" {
            self.host
                .default_output_device()
                .expect("No default output device found")
        } else {
            self.host
                .output_devices()?
                .find(|d| {
                    d.name()
                        .map(|n| n == self.config.output_device)
                        .unwrap_or(false)
                })
                .unwrap_or_else(|| {
                    panic!("Output device '{}' not found.", self.config.output_device)
                })
        };

        println!("✅ Bound to Output: {}\r", output_device.name()?);

        let supported_output_config = output_device.default_output_config()?;
        let mut output_stream_config: cpal::StreamConfig = supported_output_config.clone().into();

        if let Some(sr) = self.config.sample_rate {
            output_stream_config.sample_rate = cpal::SampleRate(sr);
        }

        if let cpal::SupportedBufferSize::Range { min, max: _ } =
            supported_output_config.buffer_size()
        {
            output_stream_config.buffer_size = cpal::BufferSize::Fixed((*min).max(64));
        }

        Ok((output_device, output_stream_config))
    }

    fn run(&mut self) -> anyhow::Result<bool> {
        let (output_device, output_stream_config) = self.setup_output_device()?;
        let channels = output_stream_config.channels as usize;
        let sample_rate = output_stream_config.sample_rate.0;

        // --- INLINED MIDI SETUP ---
        let midi_rb = HeapRb::<MidiMsg>::new(256);
        let (mut midi_tx, mut midi_rx) = midi_rb.split();

        let mut midi_in =
            MidiInput::new("Rust Synth Host").expect("Failed to initialize MIDI input");
        midi_in.ignore(Ignore::None);

        let midi_ports = midi_in.ports();
        let _midi_connection = if let Some(port) = midi_ports.first() {
            let port_name = midi_in
                .port_name(port)
                .unwrap_or_else(|_| "Unknown USB Device".into());
            println!("✅ Bound to MIDI: {}\r", port_name);

            let conn = midi_in
                .connect(
                    port,
                    "midir-read-input",
                    move |_, message, _| {
                        if message.len() >= 3 {
                            let status_nibble = message[0] & 0xF0;
                            let note = message[1];
                            let velocity = message[2];
                            let normalized_vel = velocity as f32 / 127.0;

                            if status_nibble == 0x90 && velocity > 0 {
                                let _ = midi_tx.push(MidiMsg::NoteOn(note, normalized_vel));
                            } else if status_nibble == 0x80
                                || (status_nibble == 0x90 && velocity == 0)
                            {
                                let _ = midi_tx.push(MidiMsg::NoteOff(note));
                            }
                        }
                    },
                    (),
                )
                .expect("Failed to connect to MIDI port");
            Some(conn)
        } else {
            println!("⚠️ No MIDI input ports found. Plug in a USB keyboard and restart.\r");
            None
        };

        // --- INLINED LIVE INPUT SETUP ---
        let mut opt_consumer = None;
        let mut _input_stream_guard = None;

        if self.config.enable_live_input {
            let input_device = if self.config.input_device.to_lowercase() == "default" {
                self.host
                    .default_input_device()
                    .expect("No default input device found")
            } else {
                self.host
                    .input_devices()?
                    .find(|d| {
                        d.name()
                            .map(|n| n == self.config.input_device)
                            .unwrap_or(false)
                    })
                    .unwrap_or_else(|| panic!("Input device not found"))
            };

            println!("✅ Bound to Input:  {}\r", input_device.name()?);

            let supported_input_config = input_device.default_input_config()?;
            let mut input_stream_config: cpal::StreamConfig = supported_input_config.clone().into();
            input_stream_config.sample_rate = cpal::SampleRate(sample_rate);

            if let cpal::SupportedBufferSize::Range { min, max: _ } =
                supported_input_config.buffer_size()
            {
                input_stream_config.buffer_size = cpal::BufferSize::Fixed((*min).max(64));
            }

            let latency_frames =
                (self.config.latency_ms / 1_000.0) * input_stream_config.sample_rate.0 as f32;
            let ring_capacity =
                (self.config.capacity_seconds * input_stream_config.sample_rate.0 as f32) as usize
                    * channels;

            let ring = HeapRb::new(ring_capacity);
            let (mut producer, consumer) = ring.split();
            opt_consumer = Some(consumer);

            let padding = vec![0.0f32; latency_frames as usize * channels];
            producer.push_slice(&padding);

            let input_stream = input_device.build_input_stream(
                &input_stream_config,
                move |data: &[f32], _: &_| {
                    let _ = producer.push_slice(data);
                },
                |err| eprintln!("Input stream error: {}\r", err),
                None,
            )?;

            input_stream.play()?;
            _input_stream_guard = Some(input_stream);
        } else {
            println!("✅ Live input disabled (Generator Mode)\r");
        }

        let host_info = HostInfo::new(
            "Rust Synth Host",
            "My Company",
            "https://example.com",
            "0.1.0",
        )?;
        let max_frames = 65536;
        let audio_config = PluginAudioConfiguration {
            sample_rate: sample_rate as f64,
            min_frames_count: 1,
            max_frames_count: max_frames as u32,
        };

        // We must hold these instances so they don't drop during audio processing
        let mut plugin_entries = Vec::new();
        let mut plugin_instances = Vec::new();
        let mut audio_processors = Vec::new();

        for plugin_path in &self.config.plugin_chain {
            println!("✅ Loading CLAP from: {}\r", plugin_path);
            let entry = unsafe { PluginEntry::load(plugin_path) }?;
            let factory = entry.get_plugin_factory().expect("No plugin factory found");
            let descriptor = factory
                .plugin_descriptors()
                .next()
                .expect("No plugins found in CLAP");

            let mut plugin_instance = PluginInstance::<MixerHost>::new(
                |_| (),
                |_| (),
                &entry,
                descriptor.id().unwrap(),
                &host_info,
            )?;

            let stopped_processor = plugin_instance.activate(|_, _| (), audio_config)?;
            let audio_processor = stopped_processor
                .start_processing()
                .map_err(|e| anyhow::anyhow!("Failed to start CLAP processor: {:?}", e))?;

            plugin_instances.push(plugin_instance);
            plugin_entries.push(entry);
            audio_processors.push(audio_processor);
        }

        let mut intermediate_buffers = [
            vec![vec![0.0f32; max_frames as usize]; channels],
            vec![vec![0.0f32; max_frames as usize]; channels],
        ];
        let mut interleaved_in = vec![0.0f32; max_frames as usize * channels];

        let mut input_ports = AudioPorts::with_capacity(channels, 1);
        let mut output_ports = AudioPorts::with_capacity(channels, 1);
        let mut input_events_buffer = EventBuffer::new();
        let mut output_events_buffer = EventBuffer::new();
        let master_volume = self.config.master_volume.unwrap_or(1.0);

        let output_stream = output_device.build_output_stream(
            &output_stream_config,
            move |data: &mut [f32], _: &_| {
                let samples_to_read = data.len();
                let frames = samples_to_read / channels;

                if let Some(consumer) = &mut opt_consumer {
                    let read = consumer.pop_slice(&mut interleaved_in[..samples_to_read]);
                    interleaved_in[read..samples_to_read].fill(0.0);
                } else {
                    interleaved_in[..samples_to_read].fill(0.0);
                }

                for frame in 0..frames {
                    for ch in 0..channels {
                        intermediate_buffers[0][ch][frame] = interleaved_in[frame * channels + ch];
                    }
                }

                input_events_buffer.clear();
                while let Some(msg) = midi_rx.pop() {
                    match msg {
                        MidiMsg::NoteOn(note, velocity) => {
                            let event = NoteOnEvent::new(
                                0,
                                Pckn::new(0u16, 0u16, note, 0u32),
                                velocity as f64,
                            );
                            input_events_buffer.push(&event);
                        }
                        MidiMsg::NoteOff(note) => {
                            let event =
                                NoteOffEvent::new(0, Pckn::new(0u16, 0u16, note, 0u32), 0.0);
                            input_events_buffer.push(&event);
                        }
                    }
                }

                if audio_processors.is_empty() {
                    for frame in 0..frames {
                        for ch in 0..channels {
                            let sample = intermediate_buffers[0][ch][frame] * master_volume;
                            data[frame * channels + ch] = sample.clamp(-1.0, 1.0);
                        }
                    }
                    return;
                }

                let mut current_in_buf = 0;

                for audio_processor in audio_processors.iter_mut() {
                    let [ref mut buf_0, ref mut buf_1] = intermediate_buffers;
                    let (in_buf, out_buf) = if current_in_buf == 0 {
                        (buf_0, buf_1)
                    } else {
                        (buf_1, buf_0)
                    };

                    let clap_input = input_ports.with_input_buffers([AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            in_buf
                                .iter_mut()
                                .map(|c| InputChannel::constant(&mut c[..frames])),
                        ),
                    }]);

                    let mut clap_output = output_ports.with_output_buffers([AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_output_only(
                            out_buf.iter_mut().map(|c| &mut c[..frames]),
                        ),
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

                    current_in_buf = 1 - current_in_buf;
                }

                let final_buf = current_in_buf;
                for frame in 0..frames {
                    for ch in 0..channels {
                        let sample = intermediate_buffers[final_buf][ch][frame] * master_volume;
                        data[frame * channels + ch] = sample.clamp(-1.0, 1.0);
                    }
                }
            },
            |err| eprintln!("Stream error: {}\r", err),
            None,
        )?;

        println!("\r\n🚀 Engine running at {}Hz!\r", sample_rate);
        println!("👉 Press 'p'/'o' to rotate presets, 'r' to reload config, 'q' or Esc to quit.\r");

        output_stream.play()?;

        loop {
            if poll(Duration::from_millis(50))? {
                if let Event::Key(event) = read()? {
                    match event.code {
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            let _ = rotate_preset(true);
                            return Ok(true);
                        }
                        KeyCode::Char('o') | KeyCode::Char('O') => {
                            let _ = rotate_preset(false);
                            return Ok(true);
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            println!("\r\n🔄 Reloading audio engine and config...\r");
                            return Ok(true);
                        }
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                            println!("\r\n🛑 Shutting down...\r");
                            return Ok(false);
                        }
                        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                            println!("\r\n🛑 Shutting down...\r");
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
            }
        }
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
