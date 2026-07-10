# Hyla

A minimal, fast, local-filesystem music player for macOS, written in Rust.

Hyla plays audio files directly from disk behind a deliberately austere
interface. It favors bit-transparent playback and low latency over
features.

## Priorities

In order: **audio quality**, **speed**, **simplicity**. When a change trades
fidelity or simplicity for convenience, Hyla stays lean. It is a personal,
local-only Mac app: no cover art, no metadata browsing, no networking or
streaming, no cross-platform support.

## Stack

| Concern | Crate |
| --- | --- |
| UI | egui / eframe (immediate mode, monospaced) |
| Audio output | cpal (CoreAudio) |
| Decoding | Symphonia (FLAC, MP3, WAV, AAC/ALAC, OGG) |
| Resampling | rubato (only when a file's rate differs from the device) |

The pipeline is a producer/consumer split around one hard rule: cpal's output
callback runs on a real-time thread and must never block. A decode thread
decodes, normalizes, resamples, and pushes interleaved frames into a lock-free
ring buffer; the audio callback only pulls frames, applies volume, and writes
them out.

## Requirements

* macOS
* A recent stable Rust toolchain (`rustup`)
* [cargo-bundle](https://github.com/burtonageo/cargo-bundle) for packaging:
  `cargo install cargo-bundle`

## Build and run

Always build in release. Debug builds may be too slow for glitch-free real-time
audio.

```sh
cargo run --release            # run
cargo build --release          # build the binary
cargo test                     # tests
cargo clippy                   # lints
cargo fmt                      # format
```

By default Hyla scans `~/Music/hyla-library/` at startup. You can also pass a
folder or a file on the command line, or pick a folder from the UI.

## Package a signed app

`release.sh` builds, signs, and packages a distributable bundle in one step. It
runs the fmt and clippy gates, builds the release bundle, code-signs the app so
the whole bundle is sealed, then produces a compressed, drag-to-install disk
image and verifies the app inside it.

```sh
./release.sh                                   # ad-hoc sign (personal use)
./release.sh "Developer ID Application: Name (TEAMID)"   # sign to distribute
```

Outputs:

* `target/release/bundle/osx/Hyla.app`
* `target/release/bundle/dmg/Hyla.dmg`

Install by opening the disk image and dragging `Hyla.app` to Applications.

Ad-hoc signing satisfies Gatekeeper only on the machine that signed it. On
another Mac, the first launch needs a right-click then Open, or removal of the
quarantine attribute. A Developer ID signature plus notarization removes that
step.

## License

Hyla is released under the MIT License. See [LICENSE](LICENSE). JetBrains Mono is
bundled under the SIL Open Font License; see `assets/JetBrainsMono-OFL.txt`.
