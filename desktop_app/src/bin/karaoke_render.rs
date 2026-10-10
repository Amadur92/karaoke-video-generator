#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    desktop_app::render::run_cli();
}
