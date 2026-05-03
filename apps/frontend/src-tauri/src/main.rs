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
            // Desktop domain (Phase 1)
            api::desktop::desktop_state_get,
            api::desktop::desktop_pin_set,
            api::desktop::desktop_project_associate,
            api::desktop::desktop_project_clear,
            // Settings domain (Phase 2 round 1)
            api::settings::settings_get,
            api::settings::settings_save,
            api::settings::app_version,
            api::settings::get_sentry_dsn,
            api::settings::get_sentry_config,
            api::settings::settings_get_cli_tools_info,
            api::settings::settings_claude_code_get_onboarding_status,
            api::settings::provider_accounts_get,
            api::settings::spellcheck_set_languages,
            api::settings::autobuild_source_env_get,
            // Claude Code domain (Phase 2 round 2)
            api::claude_code::claude_code_check_version,
            api::claude_code::claude_code_get_installations,
            api::claude_code::claude_code_get_versions,
            api::claude_code::claude_code_install,
            api::claude_code::claude_code_install_version,
            api::claude_code::claude_code_set_active_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
