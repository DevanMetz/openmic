# Contributing to OpenMic

OpenMic is a Windows Rust desktop app with real-time audio processing. Focused pull requests for audio behavior, device handling, UI, tests, and documentation are welcome.

## Build and check

Follow the [requirements and build instructions](README.md#requirements) for the Rust MSVC toolchain, Clang, and the optional VB-Audio Virtual Cable. The first build fetches a pinned DeepFilterNet dependency.

```sh
cargo test
cargo run --release
```

Run the app to verify UI or device-routing changes. For audio changes, explain the microphone, output device, noise model, and behavior you tested. Add a focused test when changing signal processing or settings. Include a screenshot for visible UI changes.

Builds without `--release` show **dev build** in the title. They process audio at full speed but never update themselves.

Releases are published by pushing a `vX.Y.Z` tag with notes in `docs/releases/vX.Y.Z.md`. The release workflow signs the executable with the `OPENMIC_SIGNING_KEY` secret (see `scripts/sign_release.py`); the matching public key is `RELEASE_KEY` in `src/update.rs`.

Keep pull requests focused and explain the user-facing effect and checks run. Do not commit private audio recordings, device-specific settings, credentials, or generated build output. Code is under the [MIT license](LICENSE); vendored RNNoise retains its own license.
