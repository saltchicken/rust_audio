use crate::config::MixerConfig;
use crate::host::MixerHost;
use crate::midi::{connect_midi, MidiMsg};

use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::event::{poll, read, Event, KeyCode, KeyModifiers};
use ringbuf::HeapRb;
use std::time::Duration;

pub struct AudioEngine {
    config: MixerConfig,
    host: cpal::Host,
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
            let desired = (*min).max(64).min(*max);
            output_stream_config.buffer_size = cpal::BufferSize::Fixed(desired);
            println!("⚡ Requested Output Buffer Size: {} frames\r", desired);
        }

        Ok((output_device, output_stream_config))
    }

    pub fn run(&mut self) -> anyhow::Result<bool> {
        let (output_device, output_stream_config) = self.setup_output_device()?;
        let channels = output_stream_config.channels as usize;
        let sample_rate = output_stream_config.sample_rate.0;

        // --- MIDI SETUP ---
        // Increase capacity to handle dense generative MIDI bursts from Strudel
        let midi_rb = HeapRb::<MidiMsg>::new(8192);
        let (mut midi_tx, mut midi_rx) = midi_rb.split();

        let _midi_connection = connect_midi(move |msg| {
            let _ = midi_tx.push(msg);
        });

        // --- LIVE INPUT SETUP ---
        let mut opt_consumer = None;
        let mut _input_stream_guard = None;

        let any_live_input = self.config.track.iter().any(|t| t.enable_live_input);

        if any_live_input {
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

            let target_sr = cpal::SampleRate(sample_rate);
            let supported_input_config = input_device
                .supported_input_configs()?
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
                    input_device
                        .default_input_config()
                        .expect("No default input config available")
                });

            let mut input_stream_config: cpal::StreamConfig = supported_input_config.clone().into();

            if let cpal::SupportedBufferSize::Range { min, max } =
                supported_input_config.buffer_size()
            {
                let desired = (*min).max(64).min(*max);
                input_stream_config.buffer_size = cpal::BufferSize::Fixed(desired);
                println!("⚡ Requested Input Buffer Size: {} frames\r", desired);
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
            println!("✅ Live input disabled for all tracks\r");
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

            // Expose the track index to the plugins being loaded in this track via the environment
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

        let mut interleaved_in = vec![0.0f32; max_frames as usize * channels];
        let master_volume = self.config.master_volume.unwrap_or(1.0);
        let config_tracks = self.config.track.clone();

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

                data.fill(0.0);

                for (in_ev, _) in &mut tracks_events {
                    in_ev.clear();
                }

                // Pull all MIDI messages for this block
                let mut msgs = Vec::new();
                while let Some(msg) = midi_rx.pop() {
                    msgs.push(msg);
                }

                // Map absolute midir timestamps to relative frame offsets
                if !msgs.is_empty() {
                    let first_stamp = msgs.first().map(|m| match m {
                        MidiMsg::NoteOn(_, _, _, s) => *s,
                        MidiMsg::NoteOff(_, _, s) => *s,
                    }).unwrap_or(0);

                    for msg in msgs {
                        match msg {
                            MidiMsg::NoteOn(ch, note, velocity, s) => {
                                let offset_us = s.saturating_sub(first_stamp);
                                let mut time = ((offset_us as f64 / 1_000_000.0) * sample_rate as f64) as u32;
                                time = time.min((frames as u32).saturating_sub(1));

                                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                                    if track_cfg.midi_channel.is_none()
                                        || track_cfg.midi_channel == Some(ch)
                                    {
                                        let event = NoteOnEvent::new(
                                            time,
                                            Pckn::new(0u16, ch as u16, note, 0u32),
                                            velocity as f64,
                                        );
                                        tracks_events[track_idx].0.push(&event);
                                    }
                                }
                            }
                            MidiMsg::NoteOff(ch, note, s) => {
                                let offset_us = s.saturating_sub(first_stamp);
                                let mut time = ((offset_us as f64 / 1_000_000.0) * sample_rate as f64) as u32;
                                time = time.min((frames as u32).saturating_sub(1));

                                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                                    if track_cfg.midi_channel.is_none()
                                        || track_cfg.midi_channel == Some(ch)
                                    {
                                        let event = NoteOffEvent::new(
                                            time,
                                            Pckn::new(0u16, ch as u16, note, 0u32),
                                            0.0,
                                        );
                                        tracks_events[track_idx].0.push(&event);
                                    }
                                }
                            }
                        }
                    }
                }

                for (track_idx, track_cfg) in config_tracks.iter().enumerate() {
                    let processors = &mut tracks_processors[track_idx];
                    let (input_events_buffer, output_events_buffer) = &mut tracks_events[track_idx];
                    let intermediate_buffers = &mut tracks_intermediate_buffers[track_idx];
                    let input_ports = &mut input_ports_vec[track_idx];
                    let output_ports = &mut output_ports_vec[track_idx];

                    for frame in 0..frames {
                        for ch in 0..channels {
                            if track_cfg.enable_live_input {
                                intermediate_buffers[0][ch][frame] =
                                    interleaved_in[frame * channels + ch];
                            } else {
                                intermediate_buffers[0][ch][frame] = 0.0;
                            }
                        }
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

        println!("\r\n🚀 Engine running at {}Hz!\r", sample_rate);
        println!("👉 Press 'r' to reload config, 'q' or Esc to quit.\r");

        output_stream.play()?;

        loop {
            if poll(Duration::from_millis(50))? {
                if let Event::Key(event) = read()? {
                    match event.code {
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
