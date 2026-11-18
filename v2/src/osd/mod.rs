//! On-screen display: text and icons rasterised on the CPU once (or once a
//! second for debug text), then drawn by the GPU as alpha-blended quads.
//! Nothing here touches GL; [`scene::compose`] outputs [`Overlay`]s that
//! the renderer draws.

pub mod assets;
pub mod scene;
pub mod text;

use crate::geometry::Rect;

/// Straight-alpha RGBA8 image, row 0 at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

/// Handle to a GPU texture owned by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpriteId(pub usize);

/// One textured quad to draw over the camera image, in screen pixels.
/// The texture colour is multiplied by `tint` (RGBA, 0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overlay {
    pub sprite: SpriteId,
    pub rect: Rect,
    pub tint: [f32; 4],
}

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
