# Rust Audio Player

<img height="550" alt="image" src="https://github.com/user-attachments/assets/89444a97-3fac-4617-80dc-b3a5b6962c76" />

A small cross‑platform desktop audio player built with Iced (wgpu) for the UI and Rodio + Symphonia for audio playback/decoding.

- UI: Iced 0.13 (wgpu backend, async via Tokio)
- Audio: Rodio 0.21 with Symphonia decoders
- File picker: rfd
- Settings: directories + serde(json)

## Features

- Choose a folder and list its audio files (non‑recursive)
- Play/Pause/Stop, Previous/Next (auto‑advance when a track ends)
- Seek bar with current time and total duration
- Search box to filter the visible list
- Light/Dark theme toggle (Sun/Moon icon)
- Remembers last theme and last chosen folder across runs

Supported file types scanned by default:
mp3, flac, wav, ogg, opus, aac, m4a, alac, aiff, aif

Symphonia is enabled with the "all" feature, so many other formats may decode as well, but only the above extensions are scanned for listing.

## Requirements

- Rust (recommended: 1.85+ for Edition 2024)
- A working GPU/graphics driver (wgpu chooses a backend such as DX12/Vulkan/Metal automatically)
- Audio output device recognized by the OS

Platform notes:
- Windows: Uses the Windows subsystem in release builds (no extra console window)
- Linux: Ensure your audio stack (ALSA/PulseAudio/PipeWire) is configured. A modern Vulkan/OpenGL stack is recommended for wgpu.
- macOS: Metal is used by default via wgpu

## Build and Run

Quick start (debug build):

```powershell
# from the repository root
cargo run
```

Optimized build:

```powershell
cargo run --release
```

The compiled binary will be in `target/release/` (e.g., `rust-audio-player.exe` on Windows).

## Using the App

- Click "Choose Folder" to pick a directory with audio files. The app lists supported files in that folder (not recursive).
- Double‑click a track to start playing it.
- Single‑click behavior: if audio is already loaded, a single click toggles pause/resume.
- Use the playback controls:
  - Previous: If more than ~3 seconds into the current track, it restarts; otherwise it goes to the previous track in the filtered list.
  - Play/Pause: Toggles playback. If nothing is loaded yet and a track is selected, it starts that track.
  - Next: Jumps to the next track in the filtered list.
  - Stop: Stops playback and clears the current track.
- Seek bar: Drag to a position; the app applies the seek when you release and resumes if it was previously playing.
- Search: Type to filter the list by filename (case‑insensitive substring).
- Theme: Toggle Light/Dark with the Sun/Moon button.

Auto‑advance: When a track finishes, the next visible track in the current filter starts automatically. If the last track finishes, playback stops.

## Configuration and Data

The app stores a small JSON settings file containing:
- `dark_mode`: Light/Dark theme preference
- `last_folder`: The last opened folder (if it still exists)

Locations (using `directories::ProjectDirs`):
- Windows: `%APPDATA%/RustSamples/RustAudioPlayer/settings.json`
- Linux: `~/.config/RustSamples/RustAudioPlayer/settings.json`
- macOS: `~/Library/Application Support/RustSamples/RustAudioPlayer/settings.json`

Deleting this file resets the app to defaults.

## Troubleshooting

- No audio output: Ensure an output device is available and not exclusively held by another app. Update audio drivers if needed.
- Playback stutters or UI doesn’t open: Update GPU drivers. wgpu selects a backend automatically; you can try forcing one via the `WGPU_BACKEND` env var (e.g., `vulkan`, `dx12`, `metal`).
- Duration/seek not showing: Some formats don’t expose duration via Rodio; this app probes with Symphonia as a fallback. If probing still fails, seek/time may be unavailable.
- Nothing shows after choosing a folder: Only the extensions listed above are scanned; ensure files have one of those extensions (case‑insensitive).

## Development Notes

- The app is structured with a small `lib` exposing `run_app()` and a simple `main` that calls it. UI and logic live in `src/app.rs` using Iced’s functional API.
- SVG assets for controls are embedded via `include_bytes!` for portability.
- The directory scan is currently shallow (non‑recursive).

## Credits

- [Iced](https://github.com/iced-rs/iced) for the UI
- [Rodio](https://github.com/RustAudio/rodio) and [Symphonia](https://github.com/pdeljanov/Symphonia) for audio playback and decoding
- [rfd](https://github.com/PolyMeilex/rfd) for the native file dialog

---

If you run into issues or have ideas for improvements, feel free to open an issue or PR.
