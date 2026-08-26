use midir::{Ignore, MidiInput, MidiInputConnection};

#[derive(Debug, Clone, Copy)]
pub enum MidiMsg {
    NoteOn(u8, u8, f32, u64), // channel, note, velocity, timestamp_us
    NoteOff(u8, u8, u64),     // channel, note, timestamp_us
    Cc(u8, u8, u8, u64),      // channel, controller, value, timestamp_us
}

pub fn connect_midi<F>(mut on_message: F) -> Option<MidiInputConnection<()>>
where
    F: FnMut(MidiMsg) + Send + 'static,
{
    let mut midi_in = MidiInput::new("Rust Synth Host").ok()?;
    midi_in.ignore(Ignore::None);

    let midi_ports = midi_in.ports();
    if let Some(port) = midi_ports.first() {
        let port_name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| "Unknown USB Device".into());

        println!(
            "✅ Bound to MIDI: {} (Listening on All Channels)\r",
            port_name
        );

        let conn = midi_in
            .connect(
                port,
                "midir-read-input",
                move |stamp_us, message, _| {
                    if message.len() >= 3 {
                        let status_nibble = message[0] & 0xF0;
                        let channel = message[0] & 0x0F;
                        let note_or_cc = message[1];
                        let velocity_or_val = message[2];
                        let normalized_vel = velocity_or_val as f32 / 127.0;

                        if status_nibble == 0x90 && velocity_or_val > 0 {
                            on_message(MidiMsg::NoteOn(channel, note_or_cc, normalized_vel, stamp_us));
                        } else if status_nibble == 0x80 || (status_nibble == 0x90 && velocity_or_val == 0) {
                            on_message(MidiMsg::NoteOff(channel, note_or_cc, stamp_us));
                        } else if status_nibble == 0xB0 {
                            on_message(MidiMsg::Cc(channel, note_or_cc, velocity_or_val, stamp_us));
                        }
                    }
                },
                (),
            )
            .ok()?;
        Some(conn)
    } else {
        println!("⚠️ No MIDI input ports found. Plug in a USB keyboard and restart.\r");
        None
    }
}
