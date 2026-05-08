// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod api;
mod error;
mod state;
mod types;

use agent::manager::AgentManager;
use api::agent::SharedAgentManager;
use api::desktop::SharedDesktop;
use api::watcher::SharedWatchers;
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
    let terminals: api::terminal::Terminals =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let agent_manager: SharedAgentManager = Arc::new(Mutex::new(AgentManager::default()));
    let watchers: SharedWatchers = Arc::new(Mutex::new(std::collections::HashMap::new()));
    // Clone before .manage() moves ownership — both setup task and Tauri state
    // manager share the same underlying map via Arc.
    let watchers_for_setup = Arc::clone(&watchers);

    tauri::Builder::default()
        .manage(desktop_state)
        .manage(terminals)
        .manage(agent_manager)
        .manage(watchers)
        .setup(move |app| {
            // Auto-start file watchers for every registered project so the
            // Kanban board refreshes automatically from the first load.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                api::watcher::watch_all_projects(handle, watchers_for_setup).await;
            });
            Ok(())
        })
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
            // Project domain (Phase 2 round 3)
            api::project::project_list,
            api::project::project_add,
            api::project::project_remove,
            api::project::project_update_settings,
            api::project::project_set_auto_resume_after_rate_limit,
            api::project::project_set_rdr_enabled,
            api::project::tab_state_get,
            api::project::tab_state_save,
            api::project::kanban_preferences_get,
            api::project::kanban_preferences_save,
            api::project::project_env_get,
            api::project::project_env_update,
            api::project::project_initialize,
            api::project::project_check_version,
            // File explorer + screenshot + debug + diagnostics (Phase 2 round 4 light)
            api::file::file_explorer_list,
            api::file::file_explorer_read,
            api::screenshot::screenshot_get_sources,
            api::screenshot::screenshot_capture,
            api::debug::debug_get_info,
            api::debug::debug_open_logs_folder,
            api::debug::debug_copy_debug_info,
            api::debug::debug_get_recent_errors,
            api::debug::debug_list_log_files,
            api::debug::debug_trigger_crash,
            api::diagnostics::diag_get_usage_state,
            api::diagnostics::diag_get_rdr_state,
            api::diagnostics::diag_force_usage_fetch,
            api::diagnostics::diag_send_test_rdr,
            // Task domain (Phase 2 round 4a + 4e — CRUD + archive)
            api::task::task_list,
            api::task::task_create,
            api::task::task_delete,
            api::task::task_update,
            api::task::task_archive,
            api::task::task_unarchive,
            api::task::task_toggle_rdr,
            api::task::task_submit_review,
            api::task::task_update_status,
            api::task::task_resume_paused,
            api::task::task_load_image_thumbnail,
            api::task::task_refine_description,
            api::task::task_get_logs,
            api::task::task_watch_logs,
            api::task::task_unwatch_logs,
            // Terminal subsystem (Phase 4 spike — PTY foundation)
            api::terminal::terminal_create,
            api::terminal::terminal_input,
            api::terminal::terminal_resize,
            api::terminal::terminal_destroy,
            api::terminal::terminal_check_alive,
            // Agent execution subsystem (Phase 5 — Rust → Python spawning)
            api::agent::agent_start,
            api::agent::agent_stop,
            api::agent::agent_recover,
            api::agent::agent_check_running,
            // Shell domain
            api::shell::shell_open_external,
            api::shell::shell_select_directory,
            api::shell::shell_get_default_project_location,
            api::shell::shell_open_terminal,
            api::shell::shell_create_project_folder,
            api::shell::shell_search_all_projects,
            // Git domain
            api::git::git_get_branches,
            api::git::git_get_branches_with_info,
            api::git::git_get_current_branch,
            api::git::git_detect_main_branch,
            api::git::git_check_status,
            api::git::git_initialize,
            // Worktree domain
            api::worktree::worktree_detect_tools,
            api::worktree::worktree_get_status,
            api::worktree::worktree_get_diff,
            api::worktree::worktree_check_changes,
            api::worktree::worktree_discard,
            api::worktree::worktree_discard_orphaned,
            api::worktree::worktree_list,
            api::worktree::worktree_open_in_ide,
            api::worktree::worktree_open_in_terminal,
            api::worktree::worktree_merge,
            api::worktree::worktree_merge_preview,
            api::worktree::worktree_create_pr,
            api::worktree::worktree_clear_staged,
            // Claude profiles domain
            api::profiles::claude_profiles_get,
            api::profiles::claude_profile_save,
            api::profiles::claude_profile_delete,
            api::profiles::claude_profile_rename,
            api::profiles::claude_profile_set_active,
            api::profiles::claude_profile_switch,
            api::profiles::claude_profile_initialize,
            api::profiles::claude_profile_set_token,
            api::profiles::claude_profile_authenticate,
            api::profiles::claude_profile_verify_auth,
            api::profiles::claude_auto_switch_get,
            api::profiles::claude_auto_switch_update,
            // Specs file watcher (Phase 6b — Kanban auto-refresh)
            api::watcher::task_watch_project,
            api::watcher::task_unwatch_project,
            // GitHub domain (Phase 7 — gh CLI backend)
            api::github::github_check_cli,
            api::github::github_check_auth,
            api::github::github_get_token,
            api::github::github_get_user,
            api::github::github_start_auth,
            api::github::github_detect_repo,
            api::github::github_get_branches,
            api::github::github_list_user_repos,
            api::github::github_list_orgs,
            api::github::github_create_repo,
            api::github::github_add_remote,
            api::github::github_check_connection,
            api::github::github_get_repositories,
            api::github::github_get_issues,
            api::github::github_get_issue,
            api::github::github_get_issue_comments,
            api::github::github_import_issues,
            api::github::github_pr_list,
            api::github::github_pr_list_more,
            api::github::github_pr_get,
            api::github::github_pr_get_diff,
            api::github::github_pr_get_review,
            api::github::github_pr_get_reviews_batch,
            api::github::github_pr_check_new_commits,
            api::github::github_pr_check_merge_readiness,
            api::github::github_pr_get_logs,
            api::github::github_workflows_awaiting_approval,
            api::github::github_pr_post_review,
            api::github::github_pr_delete_review,
            api::github::github_pr_merge,
            api::github::github_pr_assign,
            api::github::github_pr_post_comment,
            api::github::github_pr_mark_review_posted,
            api::github::github_pr_update_branch,
            api::github::github_workflow_approve,
            api::github::github_autofix_get_config,
            api::github::github_autofix_save_config,
            api::github::github_triage_get_config,
            api::github::github_triage_save_config,
            api::github::github_triage_get_results,
            api::github::github_create_release,
            api::github::github_pr_review,
            api::github::github_pr_review_cancel,
            api::github::github_pr_fix,
            api::github::github_pr_followup_review,
            api::github::github_investigate_issue,
            api::github::github_autofix_start,
            api::github::github_autofix_stop,
            api::github::github_autofix_get_queue,
            api::github::github_autofix_check_labels,
            api::github::github_autofix_check_new,
            api::github::github_autofix_batch,
            api::github::github_autofix_get_batches,
            api::github::github_autofix_analyze_preview,
            api::github::github_autofix_approve_batches,
            api::github::github_triage_run,
            api::github::github_triage_apply_labels,
            api::github::github_suggest_version,
            api::github::github_pr_status_poll_start,
            api::github::github_pr_status_poll_stop,
            api::github::github_pr_memory_get,
            api::github::github_pr_memory_search,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
