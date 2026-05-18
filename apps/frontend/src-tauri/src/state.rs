use crate::types::{DesktopProjectAssociation, DesktopStateSnapshot, VirtualDesktopInfo};

#[derive(Default)]
pub struct DesktopState {
    pub pin_enabled: bool,
    pub current_desktop: Option<VirtualDesktopInfo>,
    pub associations: Vec<DesktopProjectAssociation>,
}

impl DesktopState {
    pub fn snapshot(&self) -> DesktopStateSnapshot {
        DesktopStateSnapshot {
            supported: cfg!(target_os = "windows"),
            available: false,
            error: None,
            pin_enabled: self.pin_enabled,
            association_hotkey: None,
            current_desktop: self.current_desktop.clone(),
            project_associations: self.associations.clone(),
        }
    }

    pub fn set_pin(&mut self, enabled: bool) -> DesktopStateSnapshot {
        self.pin_enabled = enabled;
        self.snapshot()
    }

    pub fn associate(&mut self, project_id: String) -> DesktopStateSnapshot {
        self.associations.retain(|a| a.project_id != project_id);
        if let Some(desktop) = &self.current_desktop {
            self.associations.push(DesktopProjectAssociation {
                desktop_id: desktop.id.clone(),
                desktop_number: desktop.number,
                desktop_name: desktop.name.clone(),
                project_id,
                updated_at: chrono::Utc::now().to_rfc3339(),
                source: "ui".to_string(),
            });
        }
        self.snapshot()
    }

    pub fn clear_association(&mut self, project_id: &str) -> DesktopStateSnapshot {
        self.associations.retain(|a| a.project_id != project_id);
        self.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_desktop() -> VirtualDesktopInfo {
        VirtualDesktopInfo {
            id: "vd-1".into(),
            number: Some(1),
            name: Some("Desktop 1".into()),
            visible: Some(true),
        }
    }

    #[test]
    fn default_snapshot_is_unavailable() {
        let snap = DesktopState::default().snapshot();
        assert!(!snap.pin_enabled);
        assert!(!snap.available);
        assert!(snap.project_associations.is_empty());
        assert!(snap.current_desktop.is_none());
    }

    #[test]
    fn set_pin_persists() {
        let mut st = DesktopState::default();
        let snap = st.set_pin(true);
        assert!(snap.pin_enabled);
        assert!(st.pin_enabled);
        st.set_pin(false);
        assert!(!st.pin_enabled);
    }

    #[test]
    fn associate_with_no_current_desktop_is_noop() {
        let mut st = DesktopState::default();
        let snap = st.associate("proj-1".into());
        assert!(snap.project_associations.is_empty());
    }

    fn state_with_current_desktop() -> DesktopState {
        DesktopState {
            current_desktop: Some(fake_desktop()),
            ..Default::default()
        }
    }

    #[test]
    fn associate_with_current_desktop_creates_link() {
        let mut st = state_with_current_desktop();
        let snap = st.associate("proj-1".into());
        assert_eq!(snap.project_associations.len(), 1);
        assert_eq!(snap.project_associations[0].project_id, "proj-1");
        assert_eq!(snap.project_associations[0].desktop_id, "vd-1");
        assert_eq!(snap.project_associations[0].source, "ui");
    }

    #[test]
    fn re_associating_same_project_replaces_link() {
        let mut st = state_with_current_desktop();
        st.associate("proj-1".into());
        st.associate("proj-1".into());
        assert_eq!(st.associations.len(), 1);
    }

    #[test]
    fn clear_association_removes_only_that_project() {
        let mut st = state_with_current_desktop();
        st.associate("proj-1".into());
        st.associate("proj-2".into());
        let snap = st.clear_association("proj-1");
        assert_eq!(snap.project_associations.len(), 1);
        assert_eq!(snap.project_associations[0].project_id, "proj-2");
    }

    #[test]
    fn snapshot_reports_supported_per_os() {
        let snap = DesktopState::default().snapshot();
        assert_eq!(snap.supported, cfg!(target_os = "windows"));
    }
}
