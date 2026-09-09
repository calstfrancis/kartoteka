//! Main-thread handoff for background worker threads.
//!
//! Every long-running operation in this crate (network fetches, PDF identification, git
//! pushes, folder imports) runs on a `std::thread` and reports back to the GTK main loop
//! through one of these channels.
//!
//! This exists because `glib::MainContext::channel` — which every one of those call sites
//! used — was removed in glib 0.19. The gtk-rs migration path is an `async_channel` read
//! from a future spawned on the main context, which is what this wraps. `async-channel`
//! was already in the dependency tree (via `blocking`, pulled in by gio), so using it
//! directly costs no additional build.
//!
//! The wrapper deliberately preserves the removed API's shape and semantics so the call
//! sites read as they always did:
//!
//! * messages are delivered **in order, on the main thread**, one per closure call;
//! * delivery is **wake-on-send** — no polling timer, no latency (this is why the shim
//!   uses an async channel rather than the `std::sync::mpsc` + `glib::timeout_add_local`
//!   polling idiom Zerkalo settled on; `glib::spawn_future_local` runs on the GLib main
//!   context, so no async runtime is involved either way);
//! * returning [`glib::ControlFlow::Break`] stops delivery and drops the receiver;
//! * dropping every sender ends delivery on its own, so a worker that finishes without
//!   the closure ever returning `Break` still cleans up.
//!
//! [`Sender`] is `Send + Clone`, so it moves into a worker thread exactly as before, and
//! its `send` is non-blocking (the channel is unbounded) and infallible-in-practice: it
//! fails only once the receiver is gone, which call sites already discard with `let _ =`.

use glib::ControlFlow;

/// Create a worker channel. Drop-in for the removed `glib::MainContext::channel`.
pub fn channel<T: 'static>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = async_channel::unbounded();
    (Sender { tx }, Receiver { rx })
}

/// The worker-thread half. Cheap to clone; safe to move across threads.
pub struct Sender<T> {
    tx: async_channel::Sender<T>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            tx: self.tx.clone(),
        }
    }
}

impl<T> Sender<T> {
    /// Send a message to the main thread. Never blocks — the channel is unbounded — and
    /// errors only if the receiver has already stopped listening.
    pub fn send(&self, message: T) -> Result<(), async_channel::TrySendError<T>> {
        self.tx.try_send(message)
    }
}

/// The main-thread half. Consumed by [`Receiver::attach`].
pub struct Receiver<T> {
    rx: async_channel::Receiver<T>,
}

impl<T: 'static> Receiver<T> {
    /// Deliver each message to `f` on the main thread until `f` returns
    /// [`ControlFlow::Break`] or every [`Sender`] has been dropped.
    ///
    /// Takes `self` because delivery owns the receiver for its lifetime, which is also
    /// what makes "all senders dropped ⇒ delivery ends" the natural outcome rather than a
    /// leaked future.
    pub fn attach<F>(self, mut f: F)
    where
        F: FnMut(T) -> ControlFlow + 'static,
    {
        let rx = self.rx;
        glib::spawn_future_local(async move {
            while let Ok(message) = rx.recv().await {
                if f(message) == ControlFlow::Break {
                    break;
                }
            }
        });
    }
}
