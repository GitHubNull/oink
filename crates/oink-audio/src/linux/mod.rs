//! Linux PulseAudio backend: libpulse threaded-mainloop streams, one thread
//! per capture/playback, plus a shared subscription thread for device change
//! notifications. Never touches default devices except through an explicit
//! recoverable lease, and never logs audio content.
pub(crate) mod devices;
pub(crate) mod enumerate;
pub(crate) mod sys;

/// Unified output identifier prefix; the suffix is the PulseAudio sink name.
pub(crate) const OUTPUT_PREFIX: &str = "pulse:sink:";
/// Oink-private nodes are invisible to enumeration and demand detection.
pub(crate) const PRIVATE_PREFIX: &str = "oink_";

pub(crate) fn output_id(name: &str) -> String {
    format!("{OUTPUT_PREFIX}{name}")
}
pub(crate) fn is_private(name: &str) -> bool {
    name.starts_with(PRIVATE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_the_pulse_prefix() {
        assert_eq!(output_id("alsa_output.pci"), "pulse:sink:alsa_output.pci");
        assert!(output_id("hdmi").starts_with(OUTPUT_PREFIX));
    }

    #[test]
    fn private_names_cover_virtual_nodes_only() {
        assert!(is_private("oink_mic"));
        assert!(is_private("oink_test"));
        assert!(!is_private("alsa_output.pci-0000_00_1f.3.analog-stereo"));
        assert!(!is_private("alsa_input.usb-Mic-00"));
        assert!(!is_private("Oink_Speaker"));
    }
}
