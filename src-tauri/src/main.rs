// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = sanctel_lib::run_cli_subcommand_if_requested() {
        std::process::exit(code);
    }
    sanctel_lib::run()
}
