use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualDesktopInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopProjectAssociation {
    pub desktop_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_name: Option<String>,
    pub project_id: String,
    pub updated_at: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopStateSnapshot {
    pub supported: bool,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub pin_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub association_hotkey: Option<String>,
    pub current_desktop: Option<VirtualDesktopInfo>,
    pub project_associations: Vec<DesktopProjectAssociation>,
}

#[allow(dead_code)] // Emitted via tauri::Emitter once Phase 3 (events) lands
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopProjectActivation {
    pub project_id: String,
    pub desktop_id: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct IpcResult<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> IpcResult<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}
