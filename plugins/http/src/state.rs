use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use fluidbg_plugin_sdk::PluginRuntime;

use crate::config::Config;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Config,
    pub(crate) runtime: PluginRuntime,
    pub(crate) draining: Arc<AtomicBool>,
    pub(crate) active_requests: Arc<AtomicUsize>,
    pub(crate) traffic_percent: Arc<AtomicUsize>,
}

pub(crate) struct ActiveRequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    pub(crate) fn new(active_requests: Arc<AtomicUsize>) -> Self {
        active_requests.fetch_add(1, Ordering::SeqCst);
        Self { active_requests }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}
