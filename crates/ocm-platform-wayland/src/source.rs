//! [`WaylandSource`]: a calloop `EventSource` driving the Wayland socket.
//!
//! Register/reregister/unregister delegate to an inner
//! `calloop::generic::Generic` that owns a duplicate of the Wayland socket
//! fd (polling a dup polls the same socket; the real fd stays with the
//! `Connection`). This is the standard calloop composition pattern.

use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use ocm_platform::PlatformEvent;
use wayland_client::{Connection, EventQueue};

use crate::state::DispatchState;

/// Errors surfaced by the event source (fatal; the loop stops).
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("failed to dispatch wayland events: {0}")]
    Dispatch(String),
    #[error("event loop io or registration failure: {0}")]
    Loop(String),
    #[error("io error on the wayland source: {0}")]
    Io(#[from] std::io::Error),
}

/// Calloop event source for a Wayland connection.
///
/// Emits [`PlatformEvent`]s one at a time to the loop callback.
pub struct WaylandSource {
    conn: Connection,
    queue: EventQueue<DispatchState>,
    state: DispatchState,
    fd: Generic<OwnedFd, std::io::Error>,
    shared: Rc<RefCell<crate::state::Shared>>,
}

impl WaylandSource {
    pub(crate) fn new(
        conn: Connection,
        queue: EventQueue<DispatchState>,
        state: DispatchState,
    ) -> Result<Self, std::io::Error> {
        let shared = state.shared.clone();
        // Duplicate the socket fd for epoll readiness polling.
        // (Manager globals bound during setup are intentionally not kept
        // alive here: dropping a proxy does not destroy the server-side
        // object, and every object we still dispatch to lives in `Shared`.)
        let fd = conn.backend().poll_fd().try_clone_to_owned()?;
        Ok(Self {
            conn,
            queue,
            state,
            fd: Generic::new(fd, Interest::READ, Mode::Level),
            shared,
        })
    }
}

impl calloop::EventSource for WaylandSource {
    type Event = PlatformEvent;
    type Metadata = ();
    type Ret = ();
    type Error = SourceError;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        _token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        if readiness.readable {
            // 1. Push pending requests to the server first (cheap when the
            //    buffer is empty; retries naturally on later wakeups if the
            //    socket is congested — our traffic is tiny).
            if let Err(e) = self.conn.flush() {
                tracing::debug!("wayland flush: {e}");
            }
            // 2. Read available events. A failed read is usually a spurious
            //    wakeup (EAGAIN); real protocol errors surface when
            //    dispatching in step 3.
            if let Some(guard) = self.conn.prepare_read() {
                if let Err(e) = guard.read() {
                    tracing::debug!("wayland read (spurious wakeup?): {e}");
                }
            }
            // 3. Dispatch protocol events into the shared state.
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| SourceError::Dispatch(e.to_string()))?;
            // 4. Emit platform events to the loop callback.
            let events: Vec<PlatformEvent> = std::mem::take(&mut self.shared.borrow_mut().pending);
            for event in events {
                callback(event, &mut ());
            }
        }
        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> Result<(), calloop::Error> {
        // calloop 0.14 fixes the error type of register/reregister/unregister
        // to `calloop::Error`; delegation to the inner Generic is direct.
        self.fd.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> Result<(), calloop::Error> {
        self.fd.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> Result<(), calloop::Error> {
        self.fd.unregister(poll)
    }
}
