//! A pixel-perfect camera: integer zoom, a centre in world pixels, and clamping so the
//! view never shows more than one strip of void around the world. All integer math, so
//! every mapping is exact and the GPU and CPU paths agree.

/// Floor division for signed values.
#[inline]
pub fn floor_div(a: i64, b: i64) -> i64 {
    debug_assert!(b > 0);
    (a - if a < 0 { b - 1 } else { 0 }) / b
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelCamera {
    zoom: u32,
    /// World pixel at the centre of the view (before clamping).
    centre: (i64, i64),
    viewport: (u32, u32),
    world_px: (u32, u32),
    tile_px: u32,
}

impl PixelCamera {
    pub const MIN_ZOOM: u32 = 1;
    pub const MAX_ZOOM: u32 = 8;

    /// A camera looking at the middle of a `world_cells` world.
    pub fn new(
        world_cells: (u32, u32),
        tile_px: u32,
        viewport: (u32, u32),
        zoom: u32,
    ) -> PixelCamera {
        let world_px = (world_cells.0 * tile_px, world_cells.1 * tile_px);
        let mut cam = PixelCamera {
            zoom: zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM),
            centre: (world_px.0 as i64 / 2, world_px.1 as i64 / 2),
            viewport: (viewport.0.max(1), viewport.1.max(1)),
            world_px,
            tile_px,
        };
        cam.clamp_centre();
        cam
    }

    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    pub fn viewport(&self) -> (u32, u32) {
        self.viewport
    }

    pub fn world_px(&self) -> (u32, u32) {
        self.world_px
    }

    pub fn tile_px(&self) -> u32 {
        self.tile_px
    }

    pub fn centre(&self) -> (i64, i64) {
        self.centre
    }

    /// World pixels covered by the view (rounded up).
    pub fn view_size(&self) -> (i64, i64) {
        (
            self.viewport.0.div_ceil(self.zoom) as i64,
            self.viewport.1.div_ceil(self.zoom) as i64,
        )
    }

    fn axis_origin(centre: i64, view: i64, world: i64) -> i64 {
        if view >= world {
            (world - view) / 2
        } else {
            (centre - view / 2).clamp(0, world - view)
        }
    }

    /// Bottom-left of the view in world pixels (negative when the view is wider than
    /// the world, which centres it).
    pub fn origin(&self) -> (i64, i64) {
        let (vw, vh) = self.view_size();
        (
            Self::axis_origin(self.centre.0, vw, self.world_px.0 as i64),
            Self::axis_origin(self.centre.1, vh, self.world_px.1 as i64),
        )
    }

    fn clamp_centre(&mut self) {
        let (vw, vh) = self.view_size();
        let clamp = |c: i64, view: i64, world: i64| {
            if view >= world {
                world / 2
            } else {
                c.clamp(view / 2, world - view + view / 2)
            }
        };
        self.centre = (
            clamp(self.centre.0, vw, self.world_px.0 as i64),
            clamp(self.centre.1, vh, self.world_px.1 as i64),
        );
    }

    pub fn set_viewport(&mut self, viewport: (u32, u32)) {
        self.viewport = (viewport.0.max(1), viewport.1.max(1));
        self.clamp_centre();
    }

    /// Moves the view by world pixels.
    pub fn pan(&mut self, dx: i64, dy: i64) {
        self.centre = (self.centre.0 + dx, self.centre.1 + dy);
        self.clamp_centre();
    }

    /// The world pixel under a screen pixel (`sy` counts down from the top).
    pub fn screen_to_world(&self, screen: (u32, u32)) -> (i64, i64) {
        let (ox, oy) = self.origin();
        let sy = screen.1.min(self.viewport.1 - 1);
        (
            ox + (screen.0 / self.zoom) as i64,
            oy + ((self.viewport.1 - 1 - sy) / self.zoom) as i64,
        )
    }

    /// The cell under a screen pixel, or `None` outside the world.
    pub fn screen_to_cell(&self, screen: (u32, u32)) -> Option<(usize, usize)> {
        let (wx, wy) = self.screen_to_world(screen);
        let t = self.tile_px as i64;
        let (cx, cy) = (floor_div(wx, t), floor_div(wy, t));
        let (w, h) = (self.world_px.0 as i64 / t, self.world_px.1 as i64 / t);
        if cx < 0 || cy < 0 || cx >= w || cy >= h {
            None
        } else {
            Some((cx as usize, cy as usize))
        }
    }

    /// Sets the zoom while keeping the world pixel under `screen` in place.
    pub fn set_zoom_at(&mut self, screen: (u32, u32), zoom: u32) {
        let zoom = zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let anchor = self.screen_to_world(screen);
        self.zoom = zoom;
        let (vw, vh) = self.view_size();
        let sy = screen.1.min(self.viewport.1 - 1);
        let origin = (
            anchor.0 - (screen.0 / zoom) as i64,
            anchor.1 - ((self.viewport.1 - 1 - sy) / zoom) as i64,
        );
        self.centre = (origin.0 + vw / 2, origin.1 + vh / 2);
        self.clamp_centre();
    }

    pub fn zoom_by(&mut self, screen: (u32, u32), steps: i32) {
        let zoom =
            (self.zoom as i32 + steps).clamp(Self::MIN_ZOOM as i32, Self::MAX_ZOOM as i32) as u32;
        self.set_zoom_at(screen, zoom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_div_rounds_toward_negative_infinity() {
        assert_eq!(floor_div(7, 8), 0);
        assert_eq!(floor_div(-1, 8), -1);
        assert_eq!(floor_div(-8, 8), -1);
        assert_eq!(floor_div(-9, 8), -2);
        assert_eq!(floor_div(16, 8), 2);
    }

    #[test]
    fn origin_clamps_to_the_world_and_centres_small_worlds() {
        let mut cam = PixelCamera::new((64, 64), 8, (256, 128), 2);
        assert_eq!(cam.view_size(), (128, 64));
        assert_eq!(cam.origin(), (192, 224));
        cam.pan(-10_000, 10_000);
        assert_eq!(cam.origin(), (0, 448));
        cam.pan(10_000, -10_000);
        assert_eq!(cam.origin(), (384, 0));
        let small = PixelCamera::new((8, 8), 8, (640, 480), 4);
        assert_eq!(small.view_size(), (160, 120));
        assert_eq!(small.origin(), ((64 - 160) / 2, (64 - 120) / 2));
    }

    #[test]
    fn screen_to_cell_round_trips_every_pixel_at_several_zooms() {
        for zoom in [1, 3, 8] {
            let cam = PixelCamera::new((40, 30), 8, (97, 61), zoom);
            let (ox, oy) = cam.origin();
            for sy in 0..61u32 {
                for sx in 0..97u32 {
                    let (wx, wy) = cam.screen_to_world((sx, sy));
                    assert_eq!(wx, ox + (sx / zoom) as i64);
                    assert_eq!(wy, oy + ((60 - sy) / zoom) as i64);
                    let cell = cam.screen_to_cell((sx, sy));
                    let expect = {
                        let (cx, cy) = (floor_div(wx, 8), floor_div(wy, 8));
                        (cx >= 0 && cy >= 0 && cx < 40 && cy < 30)
                            .then_some((cx as usize, cy as usize))
                    };
                    assert_eq!(cell, expect, "zoom {zoom} screen ({sx}, {sy})");
                }
            }
        }
    }

    #[test]
    fn zoom_at_keeps_the_cursor_cell_and_clamps_zoom() {
        let mut cam = PixelCamera::new((128, 128), 8, (640, 400), 2);
        let cursor = (500, 77);
        let before = cam.screen_to_cell(cursor);
        cam.zoom_by(cursor, 1);
        assert_eq!(cam.zoom(), 3);
        assert_eq!(cam.screen_to_cell(cursor), before);
        cam.zoom_by(cursor, 100);
        assert_eq!(cam.zoom(), PixelCamera::MAX_ZOOM);
        assert_eq!(cam.screen_to_cell(cursor), before);
        cam.zoom_by(cursor, -100);
        assert_eq!(cam.zoom(), PixelCamera::MIN_ZOOM);
    }

    #[test]
    fn resizing_keeps_the_view_inside_the_world() {
        let mut cam = PixelCamera::new((32, 32), 8, (64, 64), 1);
        cam.pan(1000, 1000);
        assert_eq!(cam.origin(), (192, 192));
        cam.set_viewport((300, 300));
        assert_eq!(cam.origin(), ((256 - 300) / 2, (256 - 300) / 2));
    }
}
