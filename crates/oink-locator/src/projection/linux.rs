//! Linux projection surfaces are not implemented yet; report them unavailable.
use super::{Frame, Rect};

pub(super) struct Backend;

impl Backend {
    pub(super) fn new(_health: std::sync::Arc<crate::diagnostics::Health>) -> Option<Self> {
        None
    }
    pub(super) fn present(&mut self, _frames: Vec<Frame>) {}
    pub(super) fn hide(&mut self) {}
    pub(super) fn pump(&mut self) {}
}

// X11 desktop coordinates are device pixels; density is not applied separately.
pub(super) fn annotation_unit(_screen: Rect) -> f64 {
    1.0
}
