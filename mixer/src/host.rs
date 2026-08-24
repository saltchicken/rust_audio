use clack_host::prelude::*;

pub struct MixerHost;

impl HostHandlers for MixerHost {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}
