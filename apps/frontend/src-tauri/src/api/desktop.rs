use crate::error::AppResult;
use crate::state::DesktopState;
use crate::types::{DesktopStateSnapshot, IpcResult};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

pub type SharedDesktop = Arc<Mutex<DesktopState>>;

#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_state_get(
    state: State<'_, SharedDesktop>,
) -> AppResult<IpcResult<DesktopStateSnapshot>> {
    let st = state.lock().await;
    Ok(IpcResult::ok(st.snapshot()))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_pin_set(
    enabled: bool,
    state: State<'_, SharedDesktop>,
) -> AppResult<IpcResult<DesktopStateSnapshot>> {
    let mut st = state.lock().await;
    Ok(IpcResult::ok(st.set_pin(enabled)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_project_associate(
    project_id: String,
    state: State<'_, SharedDesktop>,
) -> AppResult<IpcResult<DesktopStateSnapshot>> {
    let mut st = state.lock().await;
    Ok(IpcResult::ok(st.associate(project_id)))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn desktop_project_clear(
    project_id: String,
    state: State<'_, SharedDesktop>,
) -> AppResult<IpcResult<DesktopStateSnapshot>> {
    let mut st = state.lock().await;
    Ok(IpcResult::ok(st.clear_association(&project_id)))
}
