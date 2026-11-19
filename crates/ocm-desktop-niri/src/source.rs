//! [`NiriSource`]: calloop [`EventSource`] over the Niri event stream.
//!
//! Mirrors the hand-rolled `WaylandSource` pattern: an inner
//! `calloop::generic::Generic` owning a duplicate of the socket fd,
//! readiness-driven reads, and one neutral event at a time to the loop
//! callback.
//!
//! Error policy (important): calloop **aborts the whole loop** when a
//! source's `process_events` returns an error (verified in calloop
//! 0.14.5 `loop_logic.rs`: `disp.process_events(..)?`). A dead Niri
//! connection must therefore never surface as `Err` — the source emits
//! [`EnvironmentEvent::ProviderLost`] once and returns
//! `PostAction::Remove`, so calloop drops it and the app keeps running.

use std::io;
use std::os::unix::net::UnixStream;

use calloop::generic::Generic;
use calloop::{Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use ocm_desktop::{DesktopProvider, EnvironmentEvent};

use crate::NiriProvider;

/// Errors surfaced by the source (registration only; runtime read
/// failures become `ProviderLost` events, see the module docs).
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("event loop registration failure: {0}")]
    Registration(#[from] calloop::Error),
    #[error("failed to prepare the niri event source: {0}")]
    Setup(#[from] io::Error),
}

pub struct NiriSource {
    provider: NiriProvider,
    fd: Generic<UnixStream, io::Error>,
}

impl NiriSource {
    pub fn new(provider: NiriProvider) -> Result<Self, SourceError> {
        // A duplicate fd for epoll readiness; the provider's own stream
        // stays the read side.
        let fd = provider.stream.try_clone()?;
        Ok(Self {
            provider,
            fd: Generic::new(fd, Interest::READ, Mode::Level),
        })
    }

    pub fn provider_mut(&mut self) -> &mut NiriProvider {
        &mut self.provider
    }

    pub fn provider(&self) -> &NiriProvider {
        &self.provider
    }
}

impl calloop::EventSource for NiriSource {
    type Event = EnvironmentEvent;
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
            // Read errors (EOF, dead socket) end the subscription: tell
            // the app once, then have calloop remove this source.
            // Returning Err here would kill the whole event loop.
            match self.provider.poll_events() {
                Ok(events) => {
                    for event in events {
                        callback(event, &mut ());
                    }
                }
                Err(e) => {
                    tracing::warn!("niri event stream lost: {e}");
                    callback(EnvironmentEvent::ProviderLost, &mut ());
                    return Ok(PostAction::Remove);
                }
            }
        }
        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> Result<(), calloop::Error> {
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
