// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod error;
mod state;
mod types;

use api::desktop::SharedDesktop;
use state::DesktopState;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "auto_claude_tauri=info".into()),
        )
        .init();

    info!("Aperant-MCP Tauri shell starting");

    let desktop_state: SharedDesktop = Arc::new(Mutex::new(DesktopState::default()));

    tauri::Builder::default()
        .manage(desktop_state)
        .invoke_handler(tauri::generate_handler![
            api::desktop::desktop_state_get,
            api::desktop::desktop_pin_set,
            api::desktop::desktop_project_associate,
            api::desktop::desktop_project_clear,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
