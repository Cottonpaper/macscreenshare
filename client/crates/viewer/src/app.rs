//! winit application: window, render loop and input forwarding.

use std::sync::Arc;

use macss_protocol::InputEvent;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::geometry::{self, Rect};
use crate::input;
use crate::renderer::Renderer;
use crate::sink::FrameSlot;
use crate::UserEvent;

/// Callback invoked for every input event produced by the window.
pub type InputCallback = Box<dyn Fn(InputEvent) + Send>;

/// Pointer-capture mode. `Locked` hides the local cursor and delivers relative
/// motion (best for remote control); `Confined` keeps it visible but bounded to
/// the window. We prefer `Locked` and fall back to `Confined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grab {
    None,
    Confined,
    Locked,
}

/// Drives the viewer window. Construct with [`ViewerApp::new`] and hand it to
/// `EventLoop::run_app`.
pub struct ViewerApp {
    slot: Arc<FrameSlot>,
    input: InputCallback,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    rect: Rect,
    frame_size: Option<(u32, u32)>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    failure: Option<String>,
    /// Whether clicking the window captures the pointer (`--no-grab` clears it).
    grab_enabled: bool,
    grab: Grab,
    /// Set once grabbing is known to be unsupported, so we stop retrying.
    grab_failed: bool,
}

impl ViewerApp {
    pub fn new(slot: Arc<FrameSlot>, input: InputCallback, grab_enabled: bool) -> Self {
        Self {
            slot,
            input,
            window: None,
            renderer: None,
            rect: Rect::full(0.0, 0.0),
            frame_size: None,
            cursor: (0.5, 0.5),
            modifiers: ModifiersState::empty(),
            failure: None,
            grab_enabled,
            grab: Grab::None,
            grab_failed: false,
        }
    }

    /// Set when window/GPU creation failed, for the caller to report.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn send(&self, event: InputEvent) {
        (self.input)(event);
    }

    fn grabbed(&self) -> bool {
        self.grab != Grab::None
    }

    /// Capture the pointer, preferring a hard lock (hidden cursor, unbounded
    /// relative motion) and falling back to confinement. No-op when disabled,
    /// already captured, or known-unsupported.
    fn capture_pointer(&mut self) {
        if self.grabbed() || !self.grab_enabled || self.grab_failed {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.set_cursor_grab(CursorGrabMode::Locked).is_ok() {
            let _ = window.set_cursor_visible(false);
            self.grab = Grab::Locked;
            tracing::info!("pointer captured — press Esc to release");
            return;
        }
        if window.set_cursor_grab(CursorGrabMode::Confined).is_ok() {
            self.grab = Grab::Confined;
            tracing::info!("pointer confined to the window — press Esc to release");
            return;
        }
        // Not supported on this platform/window; keep absolute input working.
        self.grab_failed = true;
        tracing::warn!("pointer grab unsupported; using absolute cursor input");
    }

    /// Release the pointer and restore the local cursor.
    fn release_pointer(&mut self) {
        if !self.grabbed() {
            return;
        }
        if let Some(window) = self.window.clone() {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            let _ = window.set_cursor_visible(true);
        }
        self.grab = Grab::None;
        tracing::info!("pointer released");
    }

    fn redraw(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        if let Some(frame) = self.slot.take() {
            if self.frame_size.is_none() {
                tracing::info!(
                    "first frame received: {}x{} {:?}",
                    frame.width,
                    frame.height,
                    frame.format
                );
            }
            self.frame_size = Some((frame.width, frame.height));
            renderer.upload(&frame);
        }

        let size = renderer.size();
        self.rect = match self.frame_size {
            Some((fw, fh)) => {
                geometry::fit_rect(size.width as f32, size.height as f32, fw as f32, fh as f32)
            }
            None => Rect::full(size.width as f32, size.height as f32),
        };
        renderer.render(self.rect);
    }
}

impl ApplicationHandler<UserEvent> for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("MacScreenShare")
            .with_inner_size(LogicalSize::new(1440.0, 900.0));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("could not create a window: {error}"));
                event_loop.exit();
                return;
            }
        };

        match Renderer::new(window.clone()) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                self.failure = Some(format!("{error:#}"));
                event_loop.exit();
                return;
            }
        }

        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.release_pointer();
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
            }

            WindowEvent::Focused(false) => self.release_pointer(),

            WindowEvent::RedrawRequested => self.redraw(),

            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // Esc releases a captured pointer rather than reaching the host.
                if self.grabbed()
                    && event.state == ElementState::Pressed
                    && matches!(event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.release_pointer();
                    return;
                }
                if let PhysicalKey::Code(code) = event.physical_key {
                    let bits = input::modifier_bits(self.modifiers);
                    if let Some(mapped) = input::key_event(code, event.state, bits) {
                        self.send(mapped);
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // While locked, the OS reports relative motion instead; leaving
                // the virtual cursor untouched keeps the two sources from
                // fighting over the position.
                if self.grab == Grab::Locked {
                    return;
                }
                if let Some((nx, ny)) =
                    geometry::to_normalized(self.rect, position.x as f32, position.y as f32)
                {
                    self.cursor = (nx, ny);
                    self.send(InputEvent::MouseMove { x: nx, y: ny });
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed && button == MouseButton::Left {
                    self.capture_pointer();
                }
                if let Some(mapped) = input::mouse_button(button) {
                    self.send(InputEvent::MouseButton {
                        button: mapped,
                        down: state == ElementState::Pressed,
                        x: self.cursor.0,
                        y: self.cursor.1,
                    });
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.send(input::scroll_event(delta));
            }

            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        // Relative motion only arrives (and only makes sense) while locked.
        if self.grab != Grab::Locked {
            return;
        }
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            self.cursor = geometry::accumulate_norm(self.rect, self.cursor, dx as f32, dy as f32);
            self.send(InputEvent::MouseMove { x: self.cursor.0, y: self.cursor.1 });
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::FrameAvailable => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }
}
