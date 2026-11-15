//! Stopping a request or a wait from another thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Hook = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    next: AtomicU64,
    hooks: Mutex<Vec<(u64, Hook)>>,
}

/// Set once, from any thread, to stop what it was given to: a
/// [`Transport::send`](super::Transport::send) (the connection is shut down,
/// so a connect handshake, a wait for headers or a stalled read returns at
/// once) or a wait ([`TimeScale::sleep_unless`](super::TimeScale::sleep_unless)).
/// Clones share the flag. A token stays cancelled: use a new one per run.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<Inner>);

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CancelToken").field(&self.is_cancelled()).finish()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        CancelToken::default()
    }

    /// Stops whatever uses this token. Safe to call more than once.
    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let hooks = std::mem::take(&mut *self.0.hooks.lock().unwrap_or_else(|e| e.into_inner()));
        for (_, hook) in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Runs `hook` on [`cancel`](Self::cancel) (at once if already cancelled)
    /// until the returned guard is dropped.
    pub(crate) fn on_cancel(&self, hook: impl Fn() + Send + Sync + 'static) -> HookGuard<'_> {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        {
            let mut hooks = self.0.hooks.lock().unwrap_or_else(|e| e.into_inner());
            // Checked under the lock `cancel` takes, so a hook can't be missed.
            if !self.is_cancelled() {
                hooks.push((id, Box::new(hook)));
                return HookGuard { token: self, id };
            }
        }
        hook();
        HookGuard { token: self, id }
    }
}

/// Unregisters a hook when dropped.
pub(crate) struct HookGuard<'a> {
    token: &'a CancelToken,
    id: u64,
}

impl Drop for HookGuard<'_> {
    fn drop(&mut self) {
        self.token.0.hooks.lock().unwrap_or_else(|e| e.into_inner()).retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn hooks_run_once_and_only_while_registered() {
        let t = CancelToken::new();
        let n = Arc::new(AtomicUsize::new(0));
        let m = n.clone();
        let kept = t.on_cancel(move || {
            m.fetch_add(1, Ordering::SeqCst);
        });
        let m = n.clone();
        drop(t.on_cancel(move || {
            m.fetch_add(100, Ordering::SeqCst);
        }));
        t.cancel();
        t.cancel();
        assert_eq!(n.load(Ordering::SeqCst), 1);
        drop(kept);
        // Registered after cancelling: runs at once.
        let m = n.clone();
        let _late = t.on_cancel(move || {
            m.fetch_add(10, Ordering::SeqCst);
        });
        assert_eq!(n.load(Ordering::SeqCst), 11);
        assert!(t.clone().is_cancelled());
    }
}
