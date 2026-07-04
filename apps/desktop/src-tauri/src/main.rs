// Ferroterm desktop — a tabbed terminal built on the ferroterm web component
// (front-end) and portable-pty (back-end). Each tab is one PTY session.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod battery;
mod pty;

/// Bridge for front-end diagnostics to land in the dev console / stderr.
#[tauri::command]
fn debug_log(msg: String) {
    eprintln!("[js] {msg}");
}

fn main() {
    tauri::Builder::default()
        .manage(pty::PtyManager::default())
        .invoke_handler(tauri::generate_handler![
            pty::pty_spawn,
            pty::pty_write,
            pty::pty_resize,
            pty::pty_kill,
            battery::battery_status,
            debug_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ferroterm desktop");
}
