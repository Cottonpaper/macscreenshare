//! Holds the most recent decoded frame and wakes the event loop when one lands.

use std::sync::Mutex;

use macss_decoder::{DecodedFrame, FrameSink};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// A single-slot mailbox: newer frames overwrite older, unrendered ones.
pub struct FrameSlot {
    latest: Mutex<Option<DecodedFrame>>,
    proxy: EventLoopProxy<UserEvent>,
}

impl FrameSlot {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            latest: Mutex::new(None),
            proxy,
        }
    }

    /// Take the pending frame, if any.
    pub fn take(&self) -> Option<DecodedFrame> {
        let mut guard = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    }

    pub fn has_pending(&self) -> bool {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

impl FrameSink for FrameSlot {
    fn submit(&self, frame: DecodedFrame) {
        {
            let mut guard = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(frame);
        }
        let _ = self.proxy.send_event(UserEvent::FrameAvailable);
    }
}
