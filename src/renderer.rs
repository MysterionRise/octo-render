use glam::{Mat4, Vec2, Vec3, Vec4};
use image::{Rgb, RgbImage};
use std::fs::File;
use std::io;
use std::io::BufRead;
use io::BufReader;

/// Parsed face vertex: indices into vertex/texcoord/normal arrays (0-based).
#[derive(Clone, Copy, Debug)]
pub struct FaceVertex {
    pub v: usize,
    pub vt: Option<usize>,
    pub vn: Option<usize>,
}

/// A parsed OBJ model.
pub struct Model {
    pub vertices: Vec<Vec3>,
    pub texcoords: Vec<Vec2>,
    pub normals: Vec<Vec3>,
    pub faces: Vec<[FaceVertex; 3]>,
}

impl Model {
    pub fn from_file(filename: &str) -> Self {
        let file = File::open(filename).unwrap();
        let reader = BufReader::new(file);
        let mut vertices = Vec::new();
        let mut texcoords = Vec::new();
        let mut normals = Vec::new();
        let mut faces = Vec::new();

        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim();
            if line.starts_with("f ") {
                let mut parts = line.split_whitespace();
                parts.next();
                let mut face_verts = Vec::new();
                for part in parts {
                    face_verts.push(parse_face_vertex(part));
                }
                if face_verts.len() >= 3 {
                    faces.push([face_verts[0], face_verts[1], face_verts[2]]);
                }
            } else if line.starts_with("vt ") {
                let mut parts = line.split_whitespace();
                parts.next();
                let u = parts.next().unwrap().parse::<f32>().unwrap();
                let v = parts.next().unwrap().parse::<f32>().unwrap();
                texcoords.push(Vec2::new(u, v));
            } else if line.starts_with("vn ") {
                let mut parts = line.split_whitespace();
                parts.next();
                let x = parts.next().unwrap().parse::<f32>().unwrap();
                let y = parts.next().unwrap().parse::<f32>().unwrap();
                let z = parts.next().unwrap().parse::<f32>().unwrap();
                normals.push(Vec3::new(x, y, z));
            } else if line.starts_with("v ") {
                let mut parts = line.split_whitespace();
                parts.next();
                let x = parts.next().unwrap().parse::<f32>().unwrap();
                let y = parts.next().unwrap().parse::<f32>().unwrap();
                let z = parts.next().unwrap().parse::<f32>().unwrap();
                vertices.push(Vec3::new(x, y, z));
            }
        }

        Model { vertices, texcoords, normals, faces }
    }
}

/// Parse a face vertex specification like "v", "v/vt", "v/vt/vn", "v//vn".
fn parse_face_vertex(s: &str) -> FaceVertex {
    let parts: Vec<&str> = s.split('/').collect();
    let v = parts[0].parse::<usize>().unwrap() - 1; // OBJ is 1-indexed
    let vt = parts.get(1).and_then(|s| {
        if s.is_empty() { None } else { Some(s.parse::<usize>().unwrap() - 1) }
    });
    let vn = parts.get(2).and_then(|s| {
        if s.is_empty() { None } else { Some(s.parse::<usize>().unwrap() - 1) }
    });
    FaceVertex { v, vt, vn }
}

/// Legacy OBJ reader for backwards compatibility with benchmarks.
pub fn read_waveform_obj_file(filename: &str) -> (Vec<Vec3>, Vec<[i32; 3]>) {
    let model = Model::from_file(filename);
    let faces: Vec<[i32; 3]> = model.faces.iter().map(|f| {
        [(f[0].v + 1) as i32, (f[1].v + 1) as i32, (f[2].v + 1) as i32]
    }).collect();
    (model.vertices, faces)
}

pub fn draw_line(mut x1: i32, mut y1: i32, mut x2: i32, mut y2: i32, image: &mut RgbImage, colour: Rgb<u8>) {
    let mut steep: bool = false;
    if i32::abs(x1 - x2) < i32::abs(y1 - y2) {
        steep = true;
        std::mem::swap(&mut x1, &mut y1);
        std::mem::swap(&mut x2, &mut y2);
    }
    if x1 > x2 {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut y1, &mut y2);
    }

    let dx = x2 - x1;
    let derror2 = i32::abs(y2 - y1) * 2;
    let mut error2 = 0;
    let mut y = y1;
    for x in x1..x2 {
        if steep {
            *image.get_pixel_mut(y as u32, x as u32) = colour;
        } else {
            *image.get_pixel_mut(x as u32, y as u32) = colour;
        }
        error2 += derror2;
        if error2 > dx {
            y += if y2 > y1 { 1 } else { -1 };
            error2 -= dx * 2;
        }
    }
}

fn barycentric(a: Vec2, b: Vec2, c: Vec2, p: Vec2) -> Option<Vec3> {
    let v0 = c - a;
    let v1 = b - a;
    let v2 = p - a;

    let dot00 = v0.dot(v0);
    let dot01 = v0.dot(v1);
    let dot02 = v0.dot(v2);
    let dot11 = v1.dot(v1);
    let dot12 = v1.dot(v2);

    let inv_denom = dot00 * dot11 - dot01 * dot01;
    if inv_denom.abs() < 1e-10 {
        return None;
    }
    let inv_denom = 1.0 / inv_denom;

    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    Some(Vec3::new(1.0 - u - v, v, u))
}

pub fn triangle(v0: Vec2, v1: Vec2, v2: Vec2, image: &mut RgbImage, colour: Rgb<u8>) {
    let (width, height) = image.dimensions();
    let min_x = v0.x.min(v1.x).min(v2.x).max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).min(width as f32 - 1.0) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).max(0.0) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).min(height as f32 - 1.0) as i32;

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0, v1, v2, p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    *image.get_pixel_mut(x as u32, y as u32) = colour;
                }
            }
        }
    }
}

// --- Transformation matrices ---

pub fn viewport(x: f32, y: f32, w: f32, h: f32, depth: f32) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(w / 2.0, 0.0, 0.0, 0.0),
        Vec4::new(0.0, h / 2.0, 0.0, 0.0),
        Vec4::new(0.0, 0.0, depth / 2.0, 0.0),
        Vec4::new(x + w / 2.0, y + h / 2.0, depth / 2.0, 1.0),
    )
}

pub fn projection(coeff: f32) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    *m.col_mut(2) = Vec4::new(0.0, 0.0, 1.0, coeff);
    m
}

pub fn lookat(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    let z = (eye - center).normalize();
    let x = up.cross(z).normalize();
    let y = z.cross(x).normalize();

    let mut m = Mat4::IDENTITY;
    for i in 0..3 {
        *m.col_mut(i) = Vec4::new(x[i], y[i], z[i], 0.0);
    }
    m.col_mut(3)[0] = -x.dot(eye);
    m.col_mut(3)[1] = -y.dot(eye);
    m.col_mut(3)[2] = -z.dot(eye);
    m
}

pub fn transform_point(m: &Mat4, v: Vec3) -> Vec3 {
    let v4 = *m * Vec4::new(v.x, v.y, v.z, 1.0);
    Vec3::new(v4.x / v4.w, v4.y / v4.w, v4.z / v4.w)
}

// --- Shader system ---

/// Shader trait: vertex shader transforms vertices, fragment shader computes pixel colors.
pub trait Shader {
    /// Process vertex `vert_idx` (0..3) of face `face_idx`. Returns screen-space position.
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3;
    /// Compute fragment color given barycentric coordinates. Return None to discard.
    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>>;
}

/// Rasterize a triangle using a shader, with z-buffer.
pub fn triangle_shader(
    screen_coords: &[Vec3; 3],
    shader: &dyn Shader,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
) {
    let (width, height) = image.dimensions();
    let v0 = screen_coords[0];
    let v1 = screen_coords[1];
    let v2 = screen_coords[2];

    let min_x = v0.x.min(v1.x).min(v2.x).max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).min(width as f32 - 1.0) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).max(0.0) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).min(height as f32 - 1.0) as i32;

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0.truncate(), v1.truncate(), v2.truncate(), p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                    let idx = x as usize + y as usize * width as usize;
                    if z > zbuffer[idx] {
                        if let Some(colour) = shader.fragment(bc) {
                            zbuffer[idx] = z;
                            *image.get_pixel_mut(x as u32, y as u32) = colour;
                        }
                    }
                }
            }
        }
    }
}

/// Rasterize a filled triangle with z-buffer support (non-shader version).
pub fn triangle_zbuf(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    colour: Rgb<u8>,
) {
    let (width, height) = image.dimensions();
    let min_x = v0.x.min(v1.x).min(v2.x).max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).min(width as f32 - 1.0) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).max(0.0) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).min(height as f32 - 1.0) as i32;

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0.truncate(), v1.truncate(), v2.truncate(), p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                    let idx = x as usize + y as usize * width as usize;
                    if z > zbuffer[idx] {
                        zbuffer[idx] = z;
                        *image.get_pixel_mut(x as u32, y as u32) = colour;
                    }
                }
            }
        }
    }
}

// --- Built-in shaders ---

/// Gouraud shader: per-vertex lighting interpolated across the triangle.
pub struct GouraudShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub light_dir: Vec3,
    pub varying_intensity: [f32; 3],
}

impl<'a> GouraudShader<'a> {
    pub fn new(model: &'a Model, transform: Mat4, model_view: Mat4, light_dir: Vec3) -> Self {
        GouraudShader {
            model,
            transform,
            model_view,
            light_dir,
            varying_intensity: [0.0; 3],
        }
    }
}

impl Shader for GouraudShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];

        // Compute normal: use OBJ normal if available, otherwise compute from face geometry
        let normal = if let Some(ni) = face[vert_idx].vn {
            self.model.normals[ni]
        } else {
            let v0 = self.model.vertices[face[0].v];
            let v1 = self.model.vertices[face[1].v];
            let v2 = self.model.vertices[face[2].v];
            (v2 - v0).cross(v1 - v0).normalize()
        };

        // Transform normal by the inverse transpose of model-view
        let n = transform_point(&self.model_view.inverse().transpose(), normal).normalize();
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        if intensity <= 0.0 {
            return None;
        }
        let c = (intensity.min(1.0) * 255.0) as u8;
        Some(Rgb([c, c, c]))
    }
}

/// Texture-mapped shader with Gouraud lighting and optional diffuse texture.
pub struct TextureShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub light_dir: Vec3,
    pub diffuse: Option<&'a RgbImage>,
    pub varying_intensity: [f32; 3],
    pub varying_uv: [Vec2; 3],
}

impl<'a> TextureShader<'a> {
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        light_dir: Vec3,
        diffuse: Option<&'a RgbImage>,
    ) -> Self {
        TextureShader {
            model,
            transform,
            model_view,
            light_dir,
            diffuse,
            varying_intensity: [0.0; 3],
            varying_uv: [Vec2::ZERO; 3],
        }
    }
}

impl Shader for TextureShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];

        // Store UV if available
        if let Some(ti) = face[vert_idx].vt {
            self.varying_uv[vert_idx] = self.model.texcoords[ti];
        }

        let normal = if let Some(ni) = face[vert_idx].vn {
            self.model.normals[ni]
        } else {
            let v0 = self.model.vertices[face[0].v];
            let v1 = self.model.vertices[face[1].v];
            let v2 = self.model.vertices[face[2].v];
            (v2 - v0).cross(v1 - v0).normalize()
        };

        let n = transform_point(&self.model_view.inverse().transpose(), normal).normalize();
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        if intensity <= 0.0 {
            return None;
        }

        if let Some(tex) = self.diffuse {
            let uv = self.varying_uv[0] * bc.x
                + self.varying_uv[1] * bc.y
                + self.varying_uv[2] * bc.z;
            let (tw, th) = tex.dimensions();
            let tx = (uv.x * tw as f32).clamp(0.0, tw as f32 - 1.0) as u32;
            let ty = ((1.0 - uv.y) * th as f32).clamp(0.0, th as f32 - 1.0) as u32;
            let pixel = tex.get_pixel(tx, ty);
            let r = (pixel[0] as f32 * intensity.min(1.0)) as u8;
            let g = (pixel[1] as f32 * intensity.min(1.0)) as u8;
            let b = (pixel[2] as f32 * intensity.min(1.0)) as u8;
            Some(Rgb([r, g, b]))
        } else {
            let c = (intensity.min(1.0) * 255.0) as u8;
            Some(Rgb([c, c, c]))
        }
    }
}

/// Normal mapping shader: uses tangent-space normal map for per-pixel lighting.
pub struct NormalMapShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub light_dir: Vec3,
    pub diffuse: Option<&'a RgbImage>,
    pub normal_map: &'a RgbImage,
    pub varying_uv: [Vec2; 3],
    pub varying_pos: [Vec3; 3],
}

impl<'a> NormalMapShader<'a> {
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        light_dir: Vec3,
        diffuse: Option<&'a RgbImage>,
        normal_map: &'a RgbImage,
    ) -> Self {
        NormalMapShader {
            model,
            transform,
            model_view,
            light_dir,
            diffuse,
            normal_map,
            varying_uv: [Vec2::ZERO; 3],
            varying_pos: [Vec3::ZERO; 3],
        }
    }
}

impl Shader for NormalMapShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        self.varying_pos[vert_idx] = v;

        if let Some(ti) = face[vert_idx].vt {
            self.varying_uv[vert_idx] = self.model.texcoords[ti];
        }

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let uv = self.varying_uv[0] * bc.x
            + self.varying_uv[1] * bc.y
            + self.varying_uv[2] * bc.z;

        // Sample normal from normal map
        let (nw, nh) = self.normal_map.dimensions();
        let nx = (uv.x * nw as f32).clamp(0.0, nw as f32 - 1.0) as u32;
        let ny = ((1.0 - uv.y) * nh as f32).clamp(0.0, nh as f32 - 1.0) as u32;
        let np = self.normal_map.get_pixel(nx, ny);

        // Convert from [0,255] to [-1,1]
        let n = Vec3::new(
            np[0] as f32 / 255.0 * 2.0 - 1.0,
            np[1] as f32 / 255.0 * 2.0 - 1.0,
            np[2] as f32 / 255.0 * 2.0 - 1.0,
        );

        // Compute Darboux basis (tangent space)
        let dp1 = self.varying_pos[1] - self.varying_pos[0];
        let dp2 = self.varying_pos[2] - self.varying_pos[0];
        let duv1 = self.varying_uv[1] - self.varying_uv[0];
        let duv2 = self.varying_uv[2] - self.varying_uv[0];

        let face_normal = dp1.cross(dp2).normalize();
        let det = duv1.x * duv2.y - duv1.y * duv2.x;
        if det.abs() < 1e-10 {
            // Fallback to face normal
            let intensity = face_normal.dot(self.light_dir).max(0.0);
            if intensity <= 0.0 { return None; }
            let c = (intensity * 255.0) as u8;
            return Some(Rgb([c, c, c]));
        }
        let inv_det = 1.0 / det;
        let tangent = (dp1 * duv2.y - dp2 * duv1.y) * inv_det;
        let bitangent = (dp2 * duv1.x - dp1 * duv2.x) * inv_det;
        let tbn_t = tangent.normalize();
        let tbn_b = bitangent.normalize();
        let tbn_n = face_normal;

        // Transform normal from tangent space to world space
        let world_normal = (tbn_t * n.x + tbn_b * n.y + tbn_n * n.z).normalize();

        let mn = transform_point(&self.model_view.inverse().transpose(), world_normal).normalize();
        let intensity = mn.dot(self.light_dir).max(0.0);
        if intensity <= 0.0 {
            return None;
        }

        if let Some(tex) = self.diffuse {
            let (tw, th) = tex.dimensions();
            let tx = (uv.x * tw as f32).clamp(0.0, tw as f32 - 1.0) as u32;
            let ty = ((1.0 - uv.y) * th as f32).clamp(0.0, th as f32 - 1.0) as u32;
            let pixel = tex.get_pixel(tx, ty);
            let r = (pixel[0] as f32 * intensity.min(1.0)) as u8;
            let g = (pixel[1] as f32 * intensity.min(1.0)) as u8;
            let b = (pixel[2] as f32 * intensity.min(1.0)) as u8;
            Some(Rgb([r, g, b]))
        } else {
            let c = (intensity.min(1.0) * 255.0) as u8;
            Some(Rgb([c, c, c]))
        }
    }
}

// --- Shadow mapping ---

/// Depth-only shader for building shadow maps (renders from light's POV).
pub struct DepthShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
}

impl Shader for DepthShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let v = self.model.vertices[self.model.faces[face_idx][vert_idx].v];
        transform_point(&self.transform, v)
    }

    fn fragment(&self, _bc: Vec3) -> Option<Rgb<u8>> {
        Some(Rgb([255, 255, 255])) // Color doesn't matter, z-buffer is what we keep
    }
}

/// Shadow-mapped Gouraud shader: combines Gouraud shading with shadow map lookup.
pub struct ShadowShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub shadow_transform: Mat4,
    pub light_dir: Vec3,
    pub shadow_buffer: &'a [f32],
    pub shadow_width: u32,
    pub shadow_height: u32,
    pub varying_intensity: [f32; 3],
    pub varying_world_pos: [Vec3; 3],
}

impl<'a> ShadowShader<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        shadow_transform: Mat4,
        light_dir: Vec3,
        shadow_buffer: &'a [f32],
        shadow_width: u32,
        shadow_height: u32,
    ) -> Self {
        ShadowShader {
            model,
            transform,
            model_view,
            shadow_transform,
            light_dir,
            shadow_buffer,
            shadow_width,
            shadow_height,
            varying_intensity: [0.0; 3],
            varying_world_pos: [Vec3::ZERO; 3],
        }
    }
}

impl Shader for ShadowShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        self.varying_world_pos[vert_idx] = v;

        let normal = if let Some(ni) = face[vert_idx].vn {
            self.model.normals[ni]
        } else {
            let v0 = self.model.vertices[face[0].v];
            let v1 = self.model.vertices[face[1].v];
            let v2 = self.model.vertices[face[2].v];
            (v2 - v0).cross(v1 - v0).normalize()
        };

        let n = transform_point(&self.model_view.inverse().transpose(), normal).normalize();
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        if intensity <= 0.0 {
            return None;
        }

        // Look up shadow map
        let world_pos = self.varying_world_pos[0] * bc.x
            + self.varying_world_pos[1] * bc.y
            + self.varying_world_pos[2] * bc.z;
        let shadow_coord = transform_point(&self.shadow_transform, world_pos);

        let sx = shadow_coord.x as i32;
        let sy = shadow_coord.y as i32;
        let mut shadow = 1.0f32;
        if sx >= 0 && sx < self.shadow_width as i32 && sy >= 0 && sy < self.shadow_height as i32 {
            let idx = sx as usize + sy as usize * self.shadow_width as usize;
            if self.shadow_buffer[idx] > shadow_coord.z + 5.0 {
                shadow = 0.3; // in shadow
            }
        }

        let c = (intensity.min(1.0) * shadow * 255.0) as u8;
        Some(Rgb([c, c, c]))
    }
}

// --- Ambient Occlusion ---

/// Compute screen-space ambient occlusion from the z-buffer.
/// Returns an occlusion map (0.0 = fully occluded, 1.0 = no occlusion).
pub fn compute_ssao(zbuffer: &[f32], width: u32, height: u32, radius: i32, num_samples: usize) -> Vec<f32> {
    let mut ao = vec![1.0f32; (width * height) as usize];

    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let idx = x as usize + y as usize * width as usize;
            let z = zbuffer[idx];
            if z <= f32::NEG_INFINITY {
                continue;
            }

            let mut occlusion = 0.0f32;
            let mut samples = 0;

            // Sample in a grid pattern within radius
            let step = (2 * radius) as f32 / num_samples as f32;
            let step = step.max(1.0) as i32;
            for dy in (-radius..=radius).step_by(step as usize) {
                for dx in (-radius..=radius).step_by(step as usize) {
                    let sx = x + dx;
                    let sy = y + dy;
                    if sx < 0 || sx >= width as i32 || sy < 0 || sy >= height as i32 {
                        continue;
                    }
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let sidx = sx as usize + sy as usize * width as usize;
                    let sz = zbuffer[sidx];
                    if sz > z + 0.5 {
                        occlusion += 1.0;
                    }
                    samples += 1;
                }
            }

            if samples > 0 {
                ao[idx] = 1.0 - occlusion / samples as f32;
            }
        }
    }

    ao
}

/// Apply ambient occlusion to an image.
pub fn apply_ssao(image: &mut RgbImage, ao: &[f32], width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let idx = x as usize + y as usize * width as usize;
            let factor = ao[idx].clamp(0.0, 1.0);
            let pixel = image.get_pixel_mut(x, y);
            pixel[0] = (pixel[0] as f32 * factor) as u8;
            pixel[1] = (pixel[1] as f32 * factor) as u8;
            pixel[2] = (pixel[2] as f32 * factor) as u8;
        }
    }
}
