//! The Linux equivalent of VB-Cable: a null sink OpenMic plays into, and a
//! remap source that Discord, OBS and browsers can pick as their microphone.

#[cfg(target_os = "linux")]
pub use linux::{SOURCE_NAME, activate, shutdown};

/// Nothing to create outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn activate() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn shutdown() {}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};

    pub const SINK_NAME: &str = "openmic_cable";
    pub const SOURCE_NAME: &str = "openmic_mic";
    const SINK_DESCRIPTION: &str = "OpenMic Virtual Cable";
    const SOURCE_DESCRIPTION: &str = "OpenMic Microphone";
    const TIMEOUT: Duration = Duration::from_secs(3);

    /// Module ids this process owns, removed again on exit.
    static LOADED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    #[derive(Default)]
    struct OurModules {
        sinks: Vec<String>,
        sources: Vec<String>,
    }

    impl OurModules {
        fn pair(&self) -> Option<(String, String)> {
            (self.sinks.len() == 1 && self.sources.len() == 1)
                .then(|| (self.sinks[0].clone(), self.sources[0].clone()))
        }

        fn ids(&self) -> impl Iterator<Item = &String> {
            self.sinks.iter().chain(&self.sources)
        }
    }

    pub fn activate() -> Result<()> {
        LOADED.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let ours = match pactl(&["list", "short", "modules"]) {
            Ok(modules) => our_modules(&modules),
            Err(e) => {
                crate::logfile::warn(format_args!("virtual microphone unavailable: {e:#}"));
                OurModules::default()
            }
        };
        // Reuse a complete pair left by an earlier run. It is still ours, and
        // will be removed on exit; a partial pair is replaced below.
        if let Some((sink, source)) = ours.pair() {
            remember(sink, source);
            crate::logfile::info(format_args!("virtual microphone ready: {SOURCE_NAME}"));
            return Ok(());
        }
        for id in ours.ids() {
            let _ = unload(id);
        }

        let sink = load(
            "module-null-sink",
            &[
                format!("sink_name={SINK_NAME}"),
                format!("sink_properties='device.description=\"{SINK_DESCRIPTION}\"'"),
            ],
        )
        .context("create OpenMic Virtual Cable")?;
        let source = match load(
            "module-remap-source",
            &[
                format!("master={SINK_NAME}.monitor"),
                format!("source_name={SOURCE_NAME}"),
                format!("source_properties='device.description=\"{SOURCE_DESCRIPTION}\"'"),
            ],
        ) {
            Ok(source) => source,
            Err(e) => {
                let _ = unload(&sink);
                return Err(e).context("create OpenMic Microphone");
            }
        };

        remember(sink, source);
        crate::logfile::info(format_args!("virtual microphone ready: {SOURCE_NAME}"));
        Ok(())
    }

    fn remember(sink: String, source: String) {
        let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        loaded.clear();
        loaded.push(sink);
        loaded.push(source);
    }

    pub fn shutdown() {
        let ids = {
            let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *loaded)
        };
        for id in ids.into_iter().rev() {
            if let Err(e) = unload(&id) {
                crate::logfile::warn(format_args!("virtual microphone module {id} not removed: {e:#}"));
            }
        }
    }

    fn load(module: &str, args: &[String]) -> Result<String> {
        let mut all = Vec::with_capacity(args.len() + 2);
        all.push("load-module".to_owned());
        all.push(module.to_owned());
        all.extend_from_slice(args);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let output = pactl(&refs)?;
        parse_module_id(&output).context("pactl did not return a module id")
    }

    fn unload(id: &str) -> Result<()> {
        pactl(&["unload-module", id]).map(|_| ())
    }

    /// The `pactl list short modules` rows that belong to OpenMic.
    fn our_modules(text: &str) -> OurModules {
        let mut ours = OurModules::default();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(id) = fields.next() else { continue };
            let Some(name) = fields.next() else { continue };
            let args: Vec<&str> = fields.collect();
            if name == "module-null-sink" && args.contains(&"sink_name=openmic_cable") {
                ours.sinks.push(id.to_owned());
            } else if name == "module-remap-source" && args.contains(&"source_name=openmic_mic") {
                ours.sources.push(id.to_owned());
            }
        }
        ours
    }

    fn parse_module_id(text: &str) -> Option<String> {
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let id = lines.next()?.trim();
        if lines.next().is_some() {
            return None;
        }
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
    }

    /// Run pactl with a short timeout; a missing PulseAudio server or utility
    /// is a normal, non-fatal failure.
    fn pactl(args: &[&str]) -> Result<String> {
        let what = args.first().copied().unwrap_or("pactl");
        let mut child = Command::new("pactl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("run pactl {what} (is pulseaudio-utils installed?)"))?;
        let deadline = Instant::now() + TIMEOUT;
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait_with_output();
                bail!("pactl {what} timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().context("read pactl output")?;
        if !output.status.success() {
            bail!(
                "pactl {what} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn finds_only_the_modules_openmic_loaded() {
            let text = "\
10\tmodule-null-sink\tsink_name=openmic_cable sink_properties=device.description=OpenMic Virtual Cable
11\tmodule-remap-source\tmaster=openmic_cable.monitor source_name=openmic_mic
12\tmodule-null-sink\tsink_name=speakers
13\tmodule-remap-source\tmaster=speakers.monitor source_name=openmic_mic_extra
";
            let ours = our_modules(text);
            assert_eq!(ours.sinks, ["10"]);
            assert_eq!(ours.sources, ["11"]);
            assert_eq!(ours.pair(), Some(("10".into(), "11".into())));
            assert!(our_modules("").ids().next().is_none());
        }

        #[test]
        fn module_ids_are_single_numbers() {
            assert_eq!(parse_module_id("42\n"), Some("42".into()));
            assert_eq!(parse_module_id(" 7 \r\n"), Some("7".into()));
            assert_eq!(parse_module_id(""), None);
            assert_eq!(parse_module_id("42\n43\n"), None, "one load returns one id");
            assert_eq!(parse_module_id("not-a-number"), None);
        }
    }
}
