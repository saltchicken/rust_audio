use crate::config::MixerConfig;
use crate::host::MixerHost;
use crate::midi::{connect_midi, MidiMsg};

use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_host::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::event::{poll, read, Event, KeyCode, KeyModifiers};
use ringbuf::HeapRb;
use std::time::Duration;

pub struct AudioEngine {
    config: MixerConfig,
    host: cpal::Host,
}

// Helper to scan the presets directory for hot-swapping
fn get_available_presets() -> Vec<String> {
    let mut presets = Vec::new();
    if let Ok(entries) = std::fs::read_dir("presets") {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_file() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.ends_with(".toml") {
                            presets.push(format!("presets/{}", name));
                        }
                    }
                }
            }
        }
    }
    presets.sort();
    presets
}

impl AudioEngine {
    pub fn new(config: MixerConfig) -> Self {
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

        let target_sr = cpal::SampleRate(self.config.sample_rate);

        let supported_config = output_device
            .supported_output_configs()?
            .filter(|c| {
                c.channels() == 2
                    && c.min_sample_rate() <= target_sr
                    && c.max_sample_rate() >= target_sr
            })
            .min_by_key(|c| match c.buffer_size() {
                cpal::SupportedBufferSize::Range { min, .. } => *min,
                cpal::SupportedBufferSize::Unknown => u32::MAX,
            })
            .map(|c| c.with_sample_rate(target_sr))
            .unwrap_or_else(|| {
                output_device
                    .default_output_config()
                    .expect("No default output config available")
            });

        let mut output_stream_config: cpal::StreamConfig = supported_config.clone().into();

        if let cpal::SupportedBufferSize::Range { min, max } = supported_config.buffer_size() {
            let desired = (*min).max(512).min(*max);
            output_stream_config.buffer_size = cpal::BufferSize::Fixed(desired);
            println!("⚡ Requested Output Buffer Size: {} frames\r", desired);
        }

        Ok((output_device, output_stream_config))
    }

    pub fn run(&mut self, current_preset: &str) -> anyhow::Result<Option<String>> {
        // Broadcast the active preset path so the plugins know which file to load!
        std::env::set_var("CURRENT_PRESET_PATH", current_preset);

        let (output_device, output_stream_config) = self.setup_output_device()?;
        let channels = output_stream_config.channels as usize;
        let sample_rate = output_stream_config.sample_rate.0;

        // --- MIDI SETUP ---
        let midi_rb = HeapRb::<MidiMsg>::new(8192);
        let (mut midi_tx, mut midi_rx) = midi_rb.split();

        let _midi_connection = connect_midi(move |msg| {
            let _ = midi_tx.push(msg);
        });

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

        let mut all_plugin_entries = Vec::new();
        let mut all_plugin_instances = Vec::new();
        let mut tracks_processors = Vec::new();
        let mut tracks_events = Vec::new();
        let mut tracks_intermediate_buffers = Vec::new();
        let mut input_ports_vec = Vec::new();
        let mut output_ports_vec = Vec::new();

        println!("\r\n--- Initializing Tracks ---\r");
        for (idx, track_cfg) in self.config.track.iter().enumerate() {
            println!("👉 Track {}: '{}'\r", idx, track_cfg.name);

            std::env::set_var("CURRENT_TRACK_INDEX", idx.to_string());

            let mut processors = Vec::new();

            for plugin_path in &track_cfg.plugin_chain {
                println!("   ✅ Loading: {}\r", plugin_path);
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

                processors.push(audio_processor);
                all_plugin_instances.push(plugin_instance);
                all_plugin_entries.push(entry);
            }

            tracks_processors.push(processors);
            tracks_events.push((EventBuffer::new(), EventBuffer::new()));
            tracks_intermediate_buffers.push([
                vec![vec![0.0f32; max_frames as usize]; channels],
                vec![vec![0.0f32; max_frames as usize]; channels],
            ]);
            input_ports_vec.push(AudioPorts::with_capacity(channels, 1));
            output_ports_vec.push(AudioPorts::with_capacity(channels, 1));
        }

        let master_volume = self.config.master_volume.unwrap_or(1.0);
        let config_tracks = self.config.track.clone();

        // PRE-ALLOCATE the MIDI message buffer OUTSIDE the audio callback
        let mut msgs = Vec::with_capacity(256);

        let output_stream = output_device.build_output_stream(
            &output_stream_config,
            move |data: &mut [f32], _: &_| {
                let samples_to_read = data.len();
                let frames = samples_to_read / channels;

                data.fill(0.0);

                for (in_ev, _) in &mut tracks_events {
                    in_ev.clear();
                }

                // CLEAR and REUSE the vector instead of allocating a new one
                msgs.clear();
                while let Some(msg) = midi_rx.pop() {
                    msgs.push(msg);
                }

                if !msgs.is_empty() {
                    let first_stamp = msgs
                        .first()
                        .map(|m| match m {
                            MidiMsg::NoteOn(_, _, _, s) => *s,
                            MidiMsg::NoteOff(_, _, s) => *s,
                            MidiMsg::Cc(_, _, _, s) => *s,
                        })
                        .unwrap_or(0);

                    for msg in &msgs {
                        match msg {
                            MidiMsg::NoteOn(ch, note, velocity, s) => {
                                let offset_us = s.saturating_sub(first_stamp);
                                let mut time =
                                    ((offset_us as f64 / 1_000_000.0) * sample_rate as f64) as u32;
                                time = time.min((frames as u32).saturating_sub(1));

                                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                                    if track_cfg.midi_channel.is_none()
                                        || track_cfg.midi_channel == Some(*ch)
                                    {
                                        let event = NoteOnEvent::new(
                                            time,
                                            Pckn::new(0u16, *ch as u16, *note, 0u32),
                                            *velocity as f64,
                                        );
                                        tracks_events[track_idx].0.push(&event);
                                    }
                                }
                            }
                            MidiMsg::NoteOff(ch, note, s) => {
                                let offset_us = s.saturating_sub(first_stamp);
                                let mut time =
                                    ((offset_us as f64 / 1_000_000.0) * sample_rate as f64) as u32;
                                time = time.min((frames as u32).saturating_sub(1));

                                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                                    if track_cfg.midi_channel.is_none()
                                        || track_cfg.midi_channel == Some(*ch)
                                    {
                                        let event = NoteOffEvent::new(
                                            time,
                                            Pckn::new(0u16, *ch as u16, *note, 0u32),
                                            0.0,
                                        );
                                        tracks_events[track_idx].0.push(&event);
                                    }
                                }
                            }
                            MidiMsg::Cc(ch, controller, value, s) => {
                                let offset_us = s.saturating_sub(first_stamp);
                                let mut time =
                                    ((offset_us as f64 / 1_000_000.0) * sample_rate as f64) as u32;
                                time = time.min((frames as u32).saturating_sub(1));

                                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                                    if track_cfg.midi_channel.is_none()
                                        || track_cfg.midi_channel == Some(*ch)
                                    {
                                        let event =
                                            MidiEvent::new(time, 0, [0xB0 | *ch, *controller, *value]);
                                        tracks_events[track_idx].0.push(&event);
                                    }
                                }
                            }
                        }
                    }
                }

                for (track_idx, _track_cfg) in config_tracks.iter().enumerate() {
                    let processors = &mut tracks_processors[track_idx];
                    let (input_events_buffer, output_events_buffer) = &mut tracks_events[track_idx];
                    let intermediate_buffers = &mut tracks_intermediate_buffers[track_idx];
                    let input_ports = &mut input_ports_vec[track_idx];
                    let output_ports = &mut output_ports_vec[track_idx];

                    for ch in 0..channels {
                        intermediate_buffers[0][ch][..frames].fill(0.0);
                    }

                    let mut current_in_buf = 0;

                    for audio_processor in processors.iter_mut() {
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

                        let input_events = InputEvents::from_buffer(&*input_events_buffer);
                        let mut output_events = OutputEvents::from_buffer(output_events_buffer);

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
                            data[frame * channels + ch] +=
                                intermediate_buffers[final_buf][ch][frame] * master_volume;
                        }
                    }
                }

                for sample in data.iter_mut() {
                    *sample = sample.tanh();
                }
            },
            |err| eprintln!("Stream error: {}\r", err),
            None,
        )?;

        let presets = get_available_presets();

        println!("\r\n🚀 Engine running at {}Hz!\r", sample_rate);
        println!("👉 Active Preset: {}\r", current_preset);
        println!("👉 Press 'r' to reload, 'q' or Esc to quit.\r");

        for (i, p) in presets.iter().enumerate() {
            if i < 9 {
                println!("👉 Press '{}' to load {}\r", i + 1, p);
            }
        }

        output_stream.play()?;

        loop {
            if poll(Duration::from_millis(50))? {
                if let Event::Key(event) = read()? {
                    match event.code {
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            println!("\r\n🔄 Reloading audio engine...\r");
                            return Ok(Some(current_preset.to_string()));
                        }
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                            println!("\r\n🛑 Shutting down...\r");
                            return Ok(None);
                        }
                        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                            println!("\r\n🛑 Shutting down...\r");
                            return Ok(None);
                        }
                        // 1-9 Hot Swapping!
                        KeyCode::Char(c) if c.is_digit(10) => {
                            let digit = c.to_digit(10).unwrap() as usize;
                            if digit > 0 && digit <= presets.len() {
                                let new_preset = &presets[digit - 1];
                                println!("\r\n🔄 Hot-swapping to preset: {}\r", new_preset);
                                return Ok(Some(new_preset.clone()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
