pub use glam;
use glam::{Vec2, Vec3};

/// Parsed face vertex: indices into vertex/texcoord/normal arrays (0-based).
#[derive(Clone, Copy, Debug)]
pub struct FaceVertex {
    pub v: usize,
    pub vt: Option<usize>,
    pub vn: Option<usize>,
}

/// A parsed OBJ model (data only, no I/O).
pub struct Model {
    pub vertices: Vec<Vec3>,
    pub texcoords: Vec<Vec2>,
    pub normals: Vec<Vec3>,
    pub faces: Vec<[FaceVertex; 3]>,
}
