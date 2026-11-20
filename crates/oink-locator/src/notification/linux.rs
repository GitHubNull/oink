//! Linux desktop toasts are not implemented; the surface seam stays headless.
use super::{Click, Frame, Surface};

pub(super) struct Backend;

impl Backend {
    pub(super) fn new() -> Option<Self> {
        None
    }
}

impl Surface for Backend {
    fn present(&mut self, _frames: Vec<Frame>) {}
    fn clicks(&mut self) -> Vec<Click> {
        Vec::new()
    }
    fn reduced_motion(&self) -> bool {
        true
    }
}
