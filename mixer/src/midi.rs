use midir::{Ignore, MidiInput, MidiInputConnection};

#[derive(Debug, Clone, Copy)]
pub enum MidiMsg {
    NoteOn(u8, f32),
    NoteOff(u8),
}

pub fn connect_midi<F>(target_channel: Option<u8>, mut on_message: F) -> Option<MidiInputConnection<()>>
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
        
        if let Some(ch) = target_channel {
            println!("✅ Bound to MIDI: {} (Listening on Channel {})\r", port_name, ch);
        } else {
            println!("✅ Bound to MIDI: {} (Listening on All Channels)\r", port_name);
        }

        let conn = midi_in
            .connect(
                port,
                "midir-read-input",
                move |_, message, _| {
                    if message.len() >= 3 {
                        let status_nibble = message[0] & 0xF0;
                        let channel = message[0] & 0x0F;

                        // Ignore messages not matching target channel (if one is configured)
                        if let Some(target) = target_channel {
                            if channel != target {
                                return;
                            }
                        }

                        let note = message[1];
                        let velocity = message[2];
                        let normalized_vel = velocity as f32 / 127.0;

                        if status_nibble == 0x90 && velocity > 0 {
                            on_message(MidiMsg::NoteOn(note, normalized_vel));
                        } else if status_nibble == 0x80 || (status_nibble == 0x90 && velocity == 0)
                        {
                            on_message(MidiMsg::NoteOff(note));
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
