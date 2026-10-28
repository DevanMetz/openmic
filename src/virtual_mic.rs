//! The Linux equivalent of VB-Cable: a null sink OpenMic plays into, and a
//! remap source that Discord, OBS and browsers can pick as their microphone.
//! The sink is 48 kHz stereo. On PulseAudio its buffer is capped at 50 ms;
//! the module's own default is 2 s.

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

    use anyhow::{Context, Result, bail};

    pub const SINK_NAME: &str = "openmic_cable";
    pub const SOURCE_NAME: &str = "openmic_mic";
    const SINK_DESCRIPTION: &str = "OpenMic Virtual Cable";
    const SOURCE_DESCRIPTION: &str = "OpenMic Microphone";
    const TIMEOUT: Duration = Duration::from_secs(3);
    /// The DSP core and Discord both run at 48 kHz. A null sink left at the
    /// daemon default (often 44.1 kHz) makes both sides resample.
    const RATE_ARG: &str = "rate=48000";
    const FORMAT_ARG: &str = "format=float32le";
    const CHANNELS_ARG: &str = "channels=2";
    /// PulseAudio's null sink otherwise advertises a 2 s buffer. `norewinds`
    /// caps it at 50 ms. PipeWire and PulseAudio older than 13 reject the
    /// argument, so the load falls back without it.
    const LATENCY_CAP_ARG: &str = "norewinds=true";

    /// Module ids this process owns, removed again on exit.
    static LOADED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Listed {
        id: String,
        /// This module already has the arguments OpenMic would load today.
        current: bool,
        capped: bool,
    }

    #[derive(Default)]
    struct OurModules {
        sinks: Vec<Listed>,
        sources: Vec<Listed>,
    }

    struct ReadyCable {
        sink: String,
        source: String,
        capped: bool,
    }

    impl OurModules {
        /// One sink and one source at the current format. Anything else,
        /// including a cable from before the 48 kHz format, is replaced.
        fn ready(&self) -> Option<ReadyCable> {
            (self.sinks.len() == 1
                && self.sources.len() == 1
                && self.sinks[0].current
                && self.sources[0].current)
                .then(|| ReadyCable {
                    sink: self.sinks[0].id.clone(),
                    source: self.sources[0].id.clone(),
                    capped: self.sinks[0].capped,
                })
        }

        /// Sources first, so a remap is detached before its null sink goes.
        fn ids(&self) -> impl Iterator<Item = &String> {
            self.sources
                .iter()
                .chain(&self.sinks)
                .map(|module| &module.id)
        }
    }

    fn sink_args(latency_cap: bool) -> Vec<String> {
        let mut args = vec![
            format!("sink_name={SINK_NAME}"),
            format!("sink_properties='device.description=\"{SINK_DESCRIPTION}\"'"),
            RATE_ARG.to_owned(),
            FORMAT_ARG.to_owned(),
            CHANNELS_ARG.to_owned(),
        ];
        if latency_cap {
            args.push(LATENCY_CAP_ARG.to_owned());
        }
        args
    }

    fn source_args() -> Vec<String> {
        vec![
            format!("master={SINK_NAME}.monitor"),
            format!("source_name={SOURCE_NAME}"),
            format!("source_properties='device.description=\"{SOURCE_DESCRIPTION}\"'"),
        ]
    }

    fn ready_line(capped: bool) -> String {
        format!(
            "virtual microphone ready: {SOURCE_NAME} (48 kHz{})",
            if capped { ", 50 ms buffer" } else { "" }
        )
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
        // will be removed on exit; a partial or outdated pair is replaced.
        if let Some(cable) = ours.ready() {
            remember(cable.sink, cable.source);
            crate::logfile::info(ready_line(cable.capped));
            return Ok(());
        }
        for id in ours.ids() {
            let _ = unload(id);
        }

        let (sink, capped) = load_sink()?;
        let source = match load("module-remap-source", &source_args()) {
            Ok(source) => source,
            Err(e) => {
                let _ = unload(&sink);
                return Err(e).context("create OpenMic Microphone");
            }
        };

        remember(sink, source);
        crate::logfile::info(ready_line(capped));
        Ok(())
    }

    /// Ask for the 50 ms cap, then load the same cable without it when the
    /// server refuses `norewinds` (PipeWire, or PulseAudio before 13).
    /// A timeout is not a refusal: the module may already exist, and a
    /// second load would leave two cables.
    fn load_sink() -> Result<(String, bool)> {
        match load("module-null-sink", &sink_args(true)) {
            Ok(id) => Ok((id, true)),
            Err(error) if server_refused(&error) => load("module-null-sink", &sink_args(false))
                .map(|id| (id, false))
                .context("create OpenMic Virtual Cable"),
            Err(error) => Err(error).context("create OpenMic Virtual Cable"),
        }
    }

    /// pactl exited with an error, or could not be started. The server did
    /// not keep the module. A timeout or an unreadable success did.
    fn server_refused(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| {
            let message = cause.to_string();
            message.contains(" failed:") || message.starts_with("run pactl ")
        })
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
                crate::logfile::warn(format_args!(
                    "virtual microphone module {id} not removed: {e:#}"
                ));
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

    fn has_token(line: &str, token: &str) -> bool {
        line.split_whitespace().any(|field| field == token)
    }

    /// Descriptions are stored with real spaces or with Pulse's `\s` escapes.
    fn has_description(line: &str, description: &str) -> bool {
        line.contains(description) || line.contains(&description.replace(' ', "\\s"))
    }

    /// The `pactl list short modules` rows that belong to OpenMic.
    fn our_modules(text: &str) -> OurModules {
        let mut ours = OurModules::default();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(id) = fields.next() else { continue };
            let Some(name) = fields.next() else { continue };
            if !id.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if name == "module-null-sink" && has_token(line, &format!("sink_name={SINK_NAME}")) {
                let current = has_token(line, RATE_ARG)
                    && has_token(line, FORMAT_ARG)
                    && has_token(line, CHANNELS_ARG)
                    && has_description(line, SINK_DESCRIPTION);
                ours.sinks.push(Listed {
                    id: id.to_owned(),
                    current,
                    capped: current && has_token(line, LATENCY_CAP_ARG),
                });
            } else if name == "module-remap-source"
                && has_token(line, &format!("source_name={SOURCE_NAME}"))
            {
                let current = has_token(line, &format!("master={SINK_NAME}.monitor"))
                    && has_description(line, SOURCE_DESCRIPTION);
                ours.sources.push(Listed {
                    id: id.to_owned(),
                    current,
                    capped: false,
                });
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

        fn module_line(id: &str, name: &str, args: &str) -> String {
            format!("{id}\t{name}\t{args}")
        }

        #[test]
        fn finds_only_the_modules_openmic_loaded() {
            let text = format!(
                "{}\n{}\n{}\n{}\n",
                module_line("10", "module-null-sink", &sink_args(true).join(" ")),
                module_line("11", "module-remap-source", &source_args().join(" ")),
                module_line("12", "module-null-sink", "sink_name=speakers"),
                module_line(
                    "13",
                    "module-remap-source",
                    "master=speakers.monitor source_name=openmic_mic_extra"
                ),
            );
            let ours = our_modules(&text);
            let ready = ours.ready().expect("the current cable is reused");
            assert_eq!(
                (ready.sink, ready.source, ready.capped),
                ("10".into(), "11".into(), true)
            );
            assert!(our_modules("").ids().next().is_none());
        }

        #[test]
        fn an_old_or_partial_cable_is_removed_instead_of_reused() {
            let old = "\
10\tmodule-null-sink\tsink_name=openmic_cable sink_properties=device.description=\"OpenMic Virtual Cable\"
11\tmodule-remap-source\tmaster=openmic_cable.monitor source_name=openmic_mic source_properties='device.description=\"OpenMic Microphone\"'
";
            let ours = our_modules(old);
            assert!(
                ours.ready().is_none(),
                "a cable without the 48 kHz format still has the long buffer"
            );
            assert_eq!(ours.ids().cloned().collect::<Vec<_>>(), ["11", "10"]);

            let rate_lookalike = module_line(
                "4",
                "module-null-sink",
                "sink_name=openmic_cable rate=480000 format=float32le channels=2 sink_properties='device.description=\"OpenMic Virtual Cable\"'",
            );
            assert!(
                our_modules(&rate_lookalike).ready().is_none(),
                "rate=480000 is not 48 kHz"
            );
            assert_eq!(
                our_modules(&rate_lookalike)
                    .ids()
                    .cloned()
                    .collect::<Vec<_>>(),
                ["4"]
            );

            let duplicate = format!(
                "{}\n{}\n{}\n",
                module_line("1", "module-null-sink", &sink_args(false).join(" ")),
                module_line("2", "module-null-sink", &sink_args(false).join(" ")),
                module_line("3", "module-remap-source", &source_args().join(" ")),
            );
            let ours = our_modules(&duplicate);
            assert!(ours.ready().is_none(), "two sinks are not one cable");
            assert_eq!(ours.ids().cloned().collect::<Vec<_>>(), ["3", "1", "2"]);
        }

        #[test]
        fn escaped_descriptions_still_count_as_the_current_cable() {
            let text = format!(
                "{}\n{}\n",
                module_line(
                    "7",
                    "module-null-sink",
                    "sink_name=openmic_cable rate=48000 format=float32le channels=2 sink_properties=device.description=OpenMic\\sVirtual\\sCable",
                ),
                module_line(
                    "8",
                    "module-remap-source",
                    "master=openmic_cable.monitor source_name=openmic_mic source_properties=device.description=OpenMic\\sMicrophone",
                ),
            );
            let ready = our_modules(&text)
                .ready()
                .expect("pipewire may escape spaces");
            assert_eq!(
                (ready.sink, ready.source, ready.capped),
                ("7".into(), "8".into(), false)
            );
        }

        #[test]
        fn the_latency_cap_is_only_the_extra_argument() {
            let mut capped = sink_args(true);
            assert_eq!(capped.pop().as_deref(), Some(LATENCY_CAP_ARG));
            assert_eq!(capped, sink_args(false));
            assert!(sink_args(false).iter().any(|arg| arg == RATE_ARG));
            assert!(
                sink_args(false)
                    .iter()
                    .any(|arg| arg.contains(SINK_DESCRIPTION))
            );
            assert!(
                source_args()
                    .iter()
                    .any(|arg| arg.contains(SOURCE_DESCRIPTION))
            );
            assert_eq!(
                ready_line(true),
                "virtual microphone ready: openmic_mic (48 kHz, 50 ms buffer)"
            );
            assert_eq!(
                ready_line(false),
                "virtual microphone ready: openmic_mic (48 kHz)"
            );
        }

        #[test]
        fn a_refused_latency_cap_can_be_retried_and_a_timeout_cannot() {
            let refused = anyhow::anyhow!("pactl load-module failed: Invalid argument");
            let missing = anyhow::anyhow!("run pactl load-module (is pulseaudio-utils installed?)");
            let timed_out = anyhow::anyhow!("pactl load-module timed out");
            let unreadable = anyhow::anyhow!("pactl did not return a module id");
            assert!(server_refused(&refused));
            assert!(server_refused(&missing));
            assert!(
                !server_refused(&timed_out),
                "the server may already have loaded the module"
            );
            assert!(!server_refused(&unreadable));
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
