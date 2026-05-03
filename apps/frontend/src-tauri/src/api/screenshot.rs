//! Screenshot capture domain.
//!
//! Electron's screenshot system uses `desktopCapturer` to enumerate screens
//! and windows, then captures via the renderer's `getUserMedia({mediaSource})`.
//! Tauri 2 has no direct equivalent — full screen capture would need a
//! screen-capture crate (`scrap`, `screenshots`, or platform-native APIs)
//! plus permission handling on macOS.
//!
//! Phase 2 round 4 stubs both endpoints with `devMode: true` so the renderer
//! degrades the screenshot UI gracefully instead of trying to capture and
//! crashing. Real capture is deferred until a dedicated screenshot phase.

use crate::error::AppResult;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSourcesResult {
    pub success: bool,
    pub data: Vec<Value>,
    pub dev_mode: bool,
}

#[derive(Debug, Serialize)]
pub struct ScreenshotCaptureResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn screenshot_get_sources() -> AppResult<ScreenshotSourcesResult> {
    Ok(ScreenshotSourcesResult {
        success: true,
        data: Vec::new(),
        dev_mode: true,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn screenshot_capture(_options: Value) -> AppResult<ScreenshotCaptureResult> {
    Ok(ScreenshotCaptureResult {
        success: false,
        error: Some(
            "Screenshot capture is not yet implemented in the Tauri build (deferred to dedicated phase)".to_string(),
        ),
        data: None,
    })
}
