//! Viewer: windowing, GPU rendering and input translation for the client.

pub mod app;
pub mod geometry;
pub mod input;
pub mod renderer;
pub mod sink;

pub use app::ViewerApp;
pub use sink::FrameSlot;

/// Events sent to the winit event loop from the network thread.
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    /// A new frame is waiting in the [`FrameSlot`].
    FrameAvailable,
}
