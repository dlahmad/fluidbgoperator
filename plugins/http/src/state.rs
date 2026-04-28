use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use fluidbg_plugin_sdk::PluginInceptorRuntime;

use crate::config::Config;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Config,
    pub(crate) runtime: PluginInceptorRuntime,
    pub(crate) mode: Arc<AtomicU8>,
    pub(crate) active_requests: Arc<AtomicUsize>,
    pub(crate) traffic_percent: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeMode {
    Active = 0,
    Draining = 1,
    Idle = 2,
}

impl RuntimeMode {
    pub(crate) fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Draining,
            2 => Self::Idle,
            _ => Self::Active,
        }
    }
}

impl AppState {
    pub(crate) fn runtime_mode(&self) -> RuntimeMode {
        RuntimeMode::from_u8(self.mode.load(Ordering::SeqCst))
    }

    pub(crate) fn set_runtime_mode(&self, mode: RuntimeMode) {
        self.mode.store(mode as u8, Ordering::SeqCst);
    }
}

pub(crate) struct ActiveRequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    pub(crate) fn try_new(mode: Arc<AtomicU8>, active_requests: Arc<AtomicUsize>) -> Option<Self> {
        if RuntimeMode::from_u8(mode.load(Ordering::SeqCst)) != RuntimeMode::Active {
            return None;
        }
        active_requests.fetch_add(1, Ordering::SeqCst);
        if RuntimeMode::from_u8(mode.load(Ordering::SeqCst)) != RuntimeMode::Active {
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
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn active_request_guard_rejects_after_drain_starts() {
        let mode = Arc::new(AtomicU8::new(RuntimeMode::Active as u8));
        let active = Arc::new(AtomicUsize::new(0));

        let guard = ActiveRequestGuard::try_new(mode.clone(), active.clone())
            .expect("request before drain should be admitted");
        assert_eq!(active.load(Ordering::SeqCst), 1);

        mode.store(RuntimeMode::Draining as u8, Ordering::SeqCst);
        assert!(ActiveRequestGuard::try_new(mode, active.clone()).is_none());
        assert_eq!(active.load(Ordering::SeqCst), 1);

        drop(guard);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn active_request_guard_cleans_up_all_concurrent_admissions_when_drain_starts() {
        let mode = Arc::new(AtomicU8::new(RuntimeMode::Active as u8));
        let active = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(33));

        let mut workers = Vec::new();
        for _ in 0..32 {
            let mode = mode.clone();
            let active = active.clone();
            let start = start.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                ActiveRequestGuard::try_new(mode, active)
            }));
        }

        start.wait();
        mode.store(RuntimeMode::Draining as u8, Ordering::SeqCst);

        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker panicked"))
            .collect::<Vec<_>>();
        assert_eq!(
            active.load(Ordering::SeqCst),
            admitted.iter().filter(|guard| guard.is_some()).count()
        );
        drop(admitted);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(ActiveRequestGuard::try_new(mode, active.clone()).is_none());
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn active_request_guard_rejects_while_idle() {
        let mode = Arc::new(AtomicU8::new(RuntimeMode::Idle as u8));
        let active = Arc::new(AtomicUsize::new(0));

        assert!(ActiveRequestGuard::try_new(mode, active.clone()).is_none());
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
