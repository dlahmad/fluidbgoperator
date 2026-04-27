use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use fluidbg_plugin_sdk::PluginInceptorRuntime;

use crate::config::Config;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Config,
    pub(crate) runtime: PluginInceptorRuntime,
    pub(crate) draining: Arc<AtomicBool>,
    pub(crate) active_requests: Arc<AtomicUsize>,
    pub(crate) traffic_percent: Arc<AtomicUsize>,
}

pub(crate) struct ActiveRequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    pub(crate) fn try_new(
        draining: Arc<AtomicBool>,
        active_requests: Arc<AtomicUsize>,
    ) -> Option<Self> {
        if draining.load(Ordering::SeqCst) {
            return None;
        }
        active_requests.fetch_add(1, Ordering::SeqCst);
        if draining.load(Ordering::SeqCst) {
            active_requests.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self { active_requests })
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_request_guard_rejects_after_drain_starts() {
        let draining = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));

        let guard = ActiveRequestGuard::try_new(draining.clone(), active.clone())
            .expect("request before drain should be admitted");
        assert_eq!(active.load(Ordering::SeqCst), 1);

        draining.store(true, Ordering::SeqCst);
        assert!(ActiveRequestGuard::try_new(draining, active.clone()).is_none());
        assert_eq!(active.load(Ordering::SeqCst), 1);

        drop(guard);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
