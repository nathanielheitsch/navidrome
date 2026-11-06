//! Context cancellation helper — port of `utils/context.go`.

/// A cooperative cancellation token (`CancellationToken`), the Rust analogue
/// of Go's `ctx.Done()`.
pub type Cancellation = cancelled_flag::Flag;

/// Minimal cancellation flag type. Kept local: pulling in `tokio_util` (or a
/// full runtime) for a `Done()` check is more surface than the port needs.
pub mod cancelled_flag {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A shared, cloneable cancellation flag.
    #[derive(Clone, Default)]
    pub struct Flag {
        inner: Arc<AtomicBool>,
    }

    impl Flag {
        /// Creates an uncancelled flag.
        pub fn new() -> Self {
            Self::default()
        }

        /// Marks the flag cancelled.
        pub fn cancel(&self) {
            self.inner.store(true, Ordering::SeqCst);
        }

        /// Reports whether the flag is cancelled (Go `<-ctx.Done()` fired).
        pub fn is_cancelled(&self) -> bool {
            self.inner.load(Ordering::SeqCst)
        }
    }

    impl std::fmt::Debug for Flag {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Flag({})", self.is_cancelled())
        }
    }
}

/// Reports whether `token` is cancelled (`utils.IsCtxDone`).
#[must_use]
pub fn is_ctx_done(token: &Cancellation) -> bool {
    token.is_cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_false_when_not_cancelled() {
        let token = Cancellation::new();
        assert!(!is_ctx_done(&token));
    }

    #[test]
    fn returns_true_when_cancelled() {
        let token = Cancellation::new();
        token.cancel();
        assert!(is_ctx_done(&token));
    }

    #[test]
    fn cancellation_propagates_to_clones() {
        let a = Cancellation::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled());
    }
}
