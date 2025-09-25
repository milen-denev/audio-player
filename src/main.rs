#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> iced::Result {
    rust_audio_player::run_app()
}