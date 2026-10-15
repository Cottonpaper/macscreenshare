//! winit application: window, render loop and input forwarding.

use std::sync::Arc;

use macss_protocol::InputEvent;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::geometry::{self, Rect};
use crate::input;
use crate::renderer::Renderer;
use crate::sink::FrameSlot;
use crate::UserEvent;

/// Callback invoked for every input event produced by the window.
pub type InputCallback = Box<dyn Fn(InputEvent) + Send>;

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
}

impl ViewerApp {
    pub fn new(slot: Arc<FrameSlot>, input: InputCallback) -> Self {
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
        }
    }

    /// Set when window/GPU creation failed, for the caller to report.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn send(&self, event: InputEvent) {
        (self.input)(event);
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
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
            }

            WindowEvent::RedrawRequested => self.redraw(),

            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let bits = input::modifier_bits(self.modifiers);
                    if let Some(mapped) = input::key_event(code, event.state, bits) {
                        self.send(mapped);
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                if let Some((nx, ny)) =
                    geometry::to_normalized(self.rect, position.x as f32, position.y as f32)
                {
                    self.cursor = (nx, ny);
                    self.send(InputEvent::MouseMove { x: nx, y: ny });
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
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
