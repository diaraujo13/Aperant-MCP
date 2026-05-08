use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

pub struct RunningAgent {
    pub task_id: String,
    /// Sending on this channel signals the monitor task to kill the child process.
    pub kill_tx: tokio::sync::oneshot::Sender<()>,
    pub started_at: SystemTime,
}

pub struct AgentManager {
    pub agents: Arc<Mutex<HashMap<String, RunningAgent>>>,
}

impl Default for AgentManager {
    fn default() -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
