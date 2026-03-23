use slint::platform::{PointerEventButton, WindowEvent};
use slint::LogicalPosition;

extern "C" {
    fn touch_init();
    fn touch_read(x: *mut u16, y: *mut u16) -> bool;
}

/// Minimum displacement (px) before a press is treated as a drag/scroll.
/// GT911 has ±15–20 px of natural jitter even for a stationary finger.
/// Threshold must be larger than that to avoid classifying jitter as drag.
const DRAG_THRESHOLD: f32 = 30.0;

pub struct TouchController {
    pressed:    bool,
    is_drag:    bool,
    press_pos:  LogicalPosition,   // position of the original PointerPressed
    last_pos:   LogicalPosition,   // last reported position (for PointerMoved)
}

impl TouchController {
    pub fn init() -> Self {
        unsafe { touch_init() };
        let origin = LogicalPosition::new(0.0, 0.0);
        Self {
            pressed:   false,
            is_drag:   false,
            press_pos: origin,
            last_pos:  origin,
        }
    }

    /// Poll the touch panel and dispatch Slint pointer events to `window`.
    /// Returns `true` if a new touch press was detected this frame.
    pub fn poll(&mut self, window: &slint::platform::software_renderer::MinimalSoftwareWindow) -> bool {
        let mut x: u16 = 0;
        let mut y: u16 = 0;
        let is_pressed = unsafe { touch_read(&mut x, &mut y) };

        let mut new_press = false;
        if is_pressed {
            let pos = LogicalPosition::new(x as f32, y as f32);

            if !self.pressed {
                // ── Finger just touched ───────────────────────────────
                self.pressed   = true;
                self.is_drag   = false;
                self.press_pos = pos;
                self.last_pos  = pos;
                new_press      = true;
                window.dispatch_event(WindowEvent::PointerPressed {
                    position: pos,
                    button:   PointerEventButton::Left,
                });
            } else {
                // ── Finger still down ─────────────────────────────────
                let dx = (pos.x - self.press_pos.x).abs();
                let dy = (pos.y - self.press_pos.y).abs();

                if !self.is_drag && (dx >= DRAG_THRESHOLD || dy >= DRAG_THRESHOLD) {
                    // Threshold crossed — switch to drag mode
                    self.is_drag = true;
                }

                if self.is_drag {
                    // Only forward PointerMoved when actually dragging
                    let moved_x = (pos.x - self.last_pos.x).abs();
                    let moved_y = (pos.y - self.last_pos.y).abs();
                    if moved_x >= 1.0 || moved_y >= 1.0 {
                        window.dispatch_event(WindowEvent::PointerMoved { position: pos });
                        self.last_pos = pos;
                    }
                }
                // If not dragging: no PointerMoved — Slint keeps the
                // pointer grab and will fire clicked on release.
            }
        } else if self.pressed {
            // ── Finger lifted ─────────────────────────────────────────
            // For a tap (no drag), release at the original press position
            // to guarantee the element under PointerPressed receives the click.
            let release_pos = if self.is_drag { self.last_pos } else { self.press_pos };
            window.dispatch_event(WindowEvent::PointerReleased {
                position: release_pos,
                button:   PointerEventButton::Left,
            });
            self.pressed = false;
            self.is_drag = false;
        }
        new_press
    }
}
