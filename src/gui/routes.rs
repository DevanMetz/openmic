//! Which devices OpenMic routes through, and the safe defaults it picks.

use crate::config::Settings;

pub const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";

/// VB-Audio Virtual Cable's playback side (Discord records "CABLE Output").
pub fn cable_input(outputs: &[String]) -> Option<&String> {
    outputs.iter().find(|n| n.contains("CABLE Input"))
}

/// Keep valid device choices and fill missing ones: the processed output
/// prefers VB-Cable. The monitor avoids both the cable and the processed
/// output. When VB-Cable has just been installed or the app launches
/// (`prefer_cable`), switch the processed output over to it.
/// Without VB-Cable the processed output stays unset: defaulting to the
/// first device would play the mic out of the speakers (feedback).
/// Choices whose device is absent are only replaced if `replace_missing`.
pub fn choose_routes(
    s: &mut Settings,
    inputs: &[String],
    outputs: &[String],
    prefer_cable: bool,
    replace_missing: bool,
) {
    let needs = |current: &String, pool: &[String]| {
        current.is_empty() || (replace_missing && !pool.contains(current))
    };
    if needs(&s.microphone, inputs) {
        // The cable's recording side carries our own processed output;
        // choosing it as the mic would feed that output back into itself.
        s.microphone = inputs.iter()
            .find(|name| !name.contains("CABLE Output"))
            .cloned()
            .unwrap_or_default();
    }
    let cable = cable_input(outputs);
    if (prefer_cable && cable.is_some()) || needs(&s.output, outputs) {
        s.output = cable.cloned().unwrap_or_default();
    }
    if needs(&s.monitor_output, outputs) {
        s.monitor_output = outputs
            .iter()
            .find(|n| Some(*n) != cable && *n != &s.output)
            .cloned()
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fresh_install_routes_voice_to_vb_cable_and_monitor_elsewhere() {
        let mut s = Settings::default();
        let inputs = names(&["Mic (USB)"]);
        let outputs = names(&["Speakers (Realtek)", "CABLE Input (VB-Audio Virtual Cable)"]);
        choose_routes(&mut s, &inputs, &outputs, false, false);
        assert_eq!(s.microphone, "Mic (USB)");
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
        assert_eq!(s.monitor_output, "Speakers (Realtek)");
    }

    #[test]
    fn never_defaults_the_voice_to_speakers() {
        let mut s = Settings::default();
        let outputs = names(&["Speakers (Realtek)"]);
        choose_routes(&mut s, &names(&["Mic (USB)"]), &outputs, false, false);
        assert_eq!(s.output, "", "would feed the mic back out of the speakers");
        assert_eq!(s.monitor_output, "Speakers (Realtek)");
    }

    #[test]
    fn an_automatic_monitor_waits_for_an_output_other_than_the_cable() {
        let mut s = Settings::default();
        let inputs = names(&["USB mic"]);
        let cable = "CABLE Input (VB-Audio Virtual Cable)";
        let only_cable = names(&[cable]);
        choose_routes(&mut s, &inputs, &only_cable, false, false);
        assert_eq!(s.output, cable);
        assert!(s.monitor_output.is_empty(), "do not create a route the engine must reject");

        choose_routes(&mut s, &inputs, &names(&[cable, "Headphones"]), false, false);
        assert_eq!(s.monitor_output, "Headphones", "fill the unset monitor when a separate output appears");
        choose_routes(&mut s, &inputs, &only_cable, false, false);
        assert_eq!(s.monitor_output, "Headphones", "background polling preserves a saved monitor that is temporarily absent");
        choose_routes(&mut s, &inputs, &only_cable, false, true);
        assert!(s.monitor_output.is_empty(), "Refresh must not replace missing headphones with the voice-call output");
        assert_eq!(s.output, cable);
    }

    #[test]
    fn automatic_monitor_selection_also_avoids_a_custom_processed_output() {
        let processed = "My virtual output";
        let mut s = Settings { output: processed.into(), ..Settings::default() };
        let inputs = names(&["USB mic"]);
        choose_routes(&mut s, &inputs, &names(&[processed]), false, false);
        assert!(s.monitor_output.is_empty());

        let outputs = names(&[processed, "CABLE Input (VB-Audio Virtual Cable)", "Headphones"]);
        choose_routes(&mut s, &inputs, &outputs, false, false);
        assert_eq!(s.output, processed, "keep the explicitly selected voice-call route");
        assert_eq!(s.monitor_output, "Headphones", "neither virtual voice-call route is a monitor default");
    }

    #[test]
    fn automatic_microphone_selection_never_loops_back_the_cable() {
        let mut s = Settings::default();
        let cable = "CABLE Output (VB-Audio Virtual Cable)";
        let outputs = names(&["CABLE Input (VB-Audio Virtual Cable)"]);
        choose_routes(&mut s, &names(&[cable, "USB mic"]), &outputs, false, false);
        assert_eq!(s.microphone, "USB mic");

        choose_routes(&mut s, &names(&[cable]), &outputs, false, true);
        assert!(s.microphone.is_empty(), "wait for a microphone instead of creating a loop");

        s.microphone = cable.into();
        choose_routes(&mut s, &names(&[cable, "USB mic"]), &outputs, false, true);
        assert_eq!(s.microphone, cable, "an explicit route is retained");
    }

    #[test]
    fn launch_always_returns_the_voice_to_vb_cable() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "Speakers (Realtek)".into(),
            ..Default::default()
        };
        let outputs = names(&["Speakers (Realtek)", "CABLE Input (VB-Audio Virtual Cable)"]);
        choose_routes(&mut s, &names(&["Mic (USB)"]), &outputs, true, false);
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
    }

    #[test]
    fn keeps_valid_choices_until_vb_cable_is_installed() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "Speakers (Realtek)".into(),
            monitor_output: "Headphones".into(),
            ..Default::default()
        };
        let inputs = names(&["Mic (USB)"]);
        let outputs = names(&["Speakers (Realtek)", "Headphones"]);
        choose_routes(&mut s, &inputs, &outputs, false, false);
        assert_eq!(s.output, "Speakers (Realtek)");

        let outputs = names(&[
            "Speakers (Realtek)",
            "Headphones",
            "CABLE Input (VB-Audio Virtual Cable)",
        ]);
        choose_routes(&mut s, &inputs, &outputs, false, true);
        assert_eq!(s.output, "Speakers (Realtek)", "a choice made this session survives a refresh");
        choose_routes(&mut s, &inputs, &outputs, true, false);
        assert_eq!(s.output, "CABLE Input (VB-Audio Virtual Cable)");
        assert_eq!(s.monitor_output, "Headphones");
    }

    #[test]
    fn late_usb_mic_is_not_swapped_out_except_by_refresh() {
        let mut s = Settings {
            microphone: "Mic (USB)".into(),
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            ..Default::default()
        };
        let outputs = names(&["CABLE Input (VB-Audio Virtual Cable)", "Speakers"]);
        let without_usb = names(&["Webcam Mic"]);
        choose_routes(&mut s, &without_usb, &outputs, false, false);
        assert_eq!(s.microphone, "Mic (USB)", "startup/poll keeps the saved mic");
        choose_routes(&mut s, &without_usb, &outputs, false, true);
        assert_eq!(s.microphone, "Webcam Mic", "Refresh replaces a device that is gone");
    }
}
