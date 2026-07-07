use glam::{Mat4, Vec2, Vec3, Vec4};
use image::{imageops::flip_vertical_in_place, ImageBuffer, Rgb, RgbImage};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

const EPSILON: f32 = 1e-6;
const DEFAULT_BACKGROUND: Rgb<u8> = Rgb([18, 20, 24]);

/// Parsed face vertex: indices into vertex/texcoord/normal arrays (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceVertex {
    pub v: usize,
    pub vt: Option<usize>,
    pub vn: Option<usize>,
}

/// A parsed OBJ model.
#[derive(Clone, Debug)]
pub struct Model {
    pub vertices: Vec<Vec3>,
    pub texcoords: Vec<Vec2>,
    pub normals: Vec<Vec3>,
    pub faces: Vec<[FaceVertex; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelBounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl ModelBounds {
    pub fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn radius(self) -> f32 {
        self.max.distance(self.center()).max(EPSILON)
    }
}

impl Model {
    /// Load an OBJ model with triangulated polygon faces and non-panicking errors.
    pub fn load(filename: impl AsRef<Path>) -> Result<Self, String> {
        load_obj(filename.as_ref())
    }

    /// Compatibility wrapper for older examples and benchmarks.
    pub fn from_file(filename: &str) -> Self {
        Self::load(filename)
            .unwrap_or_else(|err| panic!("failed to load OBJ '{}': {}", filename, err))
    }

    pub fn bounds(&self) -> Option<ModelBounds> {
        let mut iter = self.vertices.iter().copied();
        let first = iter.next()?;
        let mut min = first;
        let mut max = first;
        for v in iter {
            min = min.min(v);
            max = max.max(v);
        }
        Some(ModelBounds { min, max })
    }

    pub fn face_normal(&self, face_idx: usize) -> Vec3 {
        let face = self.faces[face_idx];
        let v0 = self.vertices[face[0].v];
        let v1 = self.vertices[face[1].v];
        let v2 = self.vertices[face[2].v];
        safe_normal((v2 - v0).cross(v1 - v0), Vec3::Z)
    }

    pub fn vertex_normal(&self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let fv = self.faces[face_idx][vert_idx];
        fv.vn
            .map(|ni| safe_normal(self.normals[ni], self.face_normal(face_idx)))
            .unwrap_or_else(|| self.face_normal(face_idx))
    }
}

/// Legacy OBJ reader for backwards compatibility with benchmarks.
pub fn read_waveform_obj_file(filename: &str) -> (Vec<Vec3>, Vec<[i32; 3]>) {
    let model = Model::from_file(filename);
    let faces: Vec<[i32; 3]> = model
        .faces
        .iter()
        .map(|f| {
            [
                (f[0].v + 1) as i32,
                (f[1].v + 1) as i32,
                (f[2].v + 1) as i32,
            ]
        })
        .collect();
    (model.vertices, faces)
}

fn load_obj(path: &Path) -> Result<Model, String> {
    let file = File::open(path)
        .map_err(|err| format!("failed to open OBJ '{}': {}", path.display(), err))?;
    let reader = BufReader::new(file);

    let mut vertices = Vec::new();
    let mut texcoords = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::new();

    for (line_idx, line_result) in reader.lines().enumerate() {
        let line_num = line_idx + 1;
        let line = line_result.map_err(|err| {
            format!(
                "{}:{}: failed to read line: {}",
                path.display(),
                line_num,
                err
            )
        })?;
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(kind) = parts.next() else {
            continue;
        };

        match kind {
            "v" => {
                let x = parse_f32(parts.next(), path, line_num, "vertex x")?;
                let y = parse_f32(parts.next(), path, line_num, "vertex y")?;
                let z = parse_f32(parts.next(), path, line_num, "vertex z")?;
                vertices.push(Vec3::new(x, y, z));
            }
            "vt" => {
                let u = parse_f32(parts.next(), path, line_num, "texcoord u")?;
                let v = parse_f32(parts.next(), path, line_num, "texcoord v")?;
                texcoords.push(Vec2::new(u, v));
            }
            "vn" => {
                let x = parse_f32(parts.next(), path, line_num, "normal x")?;
                let y = parse_f32(parts.next(), path, line_num, "normal y")?;
                let z = parse_f32(parts.next(), path, line_num, "normal z")?;
                normals.push(safe_normal(Vec3::new(x, y, z), Vec3::Z));
            }
            "f" => {
                let face_verts = parts
                    .map(|part| {
                        parse_face_vertex(
                            part,
                            vertices.len(),
                            texcoords.len(),
                            normals.len(),
                            path,
                            line_num,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if face_verts.len() < 3 {
                    return Err(format!(
                        "{}:{}: face has {} vertices; at least 3 are required",
                        path.display(),
                        line_num,
                        face_verts.len()
                    ));
                }
                for i in 1..face_verts.len() - 1 {
                    faces.push([face_verts[0], face_verts[i], face_verts[i + 1]]);
                }
            }
            _ => {}
        }
    }

    if vertices.is_empty() {
        return Err(format!("{}: OBJ contains no vertices", path.display()));
    }
    if faces.is_empty() {
        return Err(format!(
            "{}: OBJ contains no renderable faces",
            path.display()
        ));
    }

    Ok(Model {
        vertices,
        texcoords,
        normals,
        faces,
    })
}

fn parse_f32(
    value: Option<&str>,
    path: &Path,
    line_num: usize,
    label: &str,
) -> Result<f32, String> {
    let value =
        value.ok_or_else(|| format!("{}:{}: missing {} value", path.display(), line_num, label))?;
    value.parse::<f32>().map_err(|err| {
        format!(
            "{}:{}: invalid {} value '{}': {}",
            path.display(),
            line_num,
            label,
            value,
            err
        )
    })
}

fn parse_face_vertex(
    value: &str,
    vertex_count: usize,
    texcoord_count: usize,
    normal_count: usize,
    path: &Path,
    line_num: usize,
) -> Result<FaceVertex, String> {
    let parts: Vec<&str> = value.split('/').collect();
    let v = resolve_obj_index(
        parts.first().copied().unwrap_or(""),
        vertex_count,
        path,
        line_num,
        "vertex",
    )?;
    let vt = match parts.get(1).copied() {
        Some("") | None => None,
        Some(raw) => Some(resolve_obj_index(
            raw,
            texcoord_count,
            path,
            line_num,
            "texcoord",
        )?),
    };
    let vn = match parts.get(2).copied() {
        Some("") | None => None,
        Some(raw) => Some(resolve_obj_index(
            raw,
            normal_count,
            path,
            line_num,
            "normal",
        )?),
    };
    Ok(FaceVertex { v, vt, vn })
}

fn resolve_obj_index(
    raw: &str,
    count: usize,
    path: &Path,
    line_num: usize,
    label: &str,
) -> Result<usize, String> {
    let index = raw.parse::<isize>().map_err(|err| {
        format!(
            "{}:{}: invalid {} index '{}': {}",
            path.display(),
            line_num,
            label,
            raw,
            err
        )
    })?;

    let resolved = if index > 0 {
        (index - 1) as usize
    } else if index < 0 {
        let relative = (-index) as usize;
        if relative > count {
            return Err(format!(
                "{}:{}: {} index '{}' references before the first item",
                path.display(),
                line_num,
                label,
                raw
            ));
        }
        count - relative
    } else {
        return Err(format!(
            "{}:{}: OBJ {} indices are 1-based; 0 is invalid",
            path.display(),
            line_num,
            label
        ));
    };

    if resolved >= count {
        return Err(format!(
            "{}:{}: {} index '{}' resolves to {}, but only {} {} values exist",
            path.display(),
            line_num,
            label,
            raw,
            resolved,
            count,
            label
        ));
    }

    Ok(resolved)
}

fn safe_normal(v: Vec3, fallback: Vec3) -> Vec3 {
    if v.x.is_finite() && v.y.is_finite() && v.z.is_finite() && v.length_squared() > EPSILON {
        v.normalize()
    } else {
        fallback
    }
}

fn put_pixel_checked(image: &mut RgbImage, x: i32, y: i32, colour: Rgb<u8>) {
    if x >= 0 && y >= 0 && x < image.width() as i32 && y < image.height() as i32 {
        *image.get_pixel_mut(x as u32, y as u32) = colour;
    }
}

pub fn draw_line(
    mut x1: i32,
    mut y1: i32,
    mut x2: i32,
    mut y2: i32,
    image: &mut RgbImage,
    colour: Rgb<u8>,
) {
    let mut steep = false;
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
    for x in x1..=x2 {
        if steep {
            put_pixel_checked(image, y, x, colour);
        } else {
            put_pixel_checked(image, x, y, colour);
        }
        error2 += derror2;
        if dx != 0 && error2 > dx {
            y += if y2 > y1 { 1 } else { -1 };
            error2 -= dx * 2;
        }
    }
}

fn draw_line_with_state(
    mut x1: i32,
    mut y1: i32,
    mut x2: i32,
    mut y2: i32,
    image: &mut RgbImage,
    colour: Rgb<u8>,
    render_state: RenderState,
) {
    if !render_state.color_write {
        return;
    }

    let mut steep = false;
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
    for x in x1..=x2 {
        let (px, py) = if steep { (y, x) } else { (x, y) };
        if render_state
            .scissor
            .map(|scissor| scissor.contains(px, py))
            .unwrap_or(true)
        {
            put_pixel_checked(image, px, py, colour);
        }
        error2 += derror2;
        if dx != 0 && error2 > dx {
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

    let denom = dot00 * dot11 - dot01 * dot01;
    if denom.abs() < 1e-10 {
        return None;
    }
    let inv_denom = 1.0 / denom;

    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    Some(Vec3::new(1.0 - u - v, v, u))
}

pub fn perspective_correct_barycentric(bc: Vec3, reciprocal_w: [f32; 3]) -> Vec3 {
    let weighted = Vec3::new(
        bc.x * reciprocal_w[0],
        bc.y * reciprocal_w[1],
        bc.z * reciprocal_w[2],
    );
    let sum = weighted.x + weighted.y + weighted.z;
    if sum.abs() < EPSILON {
        bc
    } else {
        weighted / sum
    }
}

pub fn triangle(v0: Vec2, v1: Vec2, v2: Vec2, image: &mut RgbImage, colour: Rgb<u8>) {
    let (width, height) = image.dimensions();
    let min_x = v0.x.min(v1.x).min(v2.x).floor().max(0.0) as i32;
    let max_x = v0.x.max(v1.x).max(v2.x).ceil().min(width as f32 - 1.0) as i32;
    let min_y = v0.y.min(v1.y).min(v2.y).floor().max(0.0) as i32;
    let max_y = v0.y.max(v1.y).max(v2.y).ceil().min(height as f32 - 1.0) as i32;

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0, v1, v2, p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    put_pixel_checked(image, x, y, colour);
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
    let z = safe_normal(eye - center, Vec3::Z);
    let x = safe_normal(up.cross(z), Vec3::X);
    let y = safe_normal(z.cross(x), Vec3::Y);

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

pub fn transform_vector(m: &Mat4, v: Vec3) -> Vec3 {
    let v4 = *m * Vec4::new(v.x, v.y, v.z, 0.0);
    Vec3::new(v4.x, v4.y, v4.z)
}

fn transform_homogeneous(m: &Mat4, v: Vec3) -> Vec4 {
    *m * Vec4::new(v.x, v.y, v.z, 1.0)
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
    triangle_shader_perspective(screen_coords, [1.0, 1.0, 1.0], shader, zbuffer, image);
}

/// Rasterize with perspective-correct interpolation for fragment varyings.
pub fn triangle_shader_perspective(
    screen_coords: &[Vec3; 3],
    reciprocal_w: [f32; 3],
    shader: &dyn Shader,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
) {
    triangle_shader_perspective_with_state(
        screen_coords,
        reciprocal_w,
        shader,
        zbuffer,
        image,
        DepthState::default(),
    );
}

pub fn triangle_shader_perspective_with_state(
    screen_coords: &[Vec3; 3],
    reciprocal_w: [f32; 3],
    shader: &dyn Shader,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    depth_state: DepthState,
) {
    let state = RenderState {
        depth: depth_state,
        ..RenderState::default()
    };
    triangle_shader_perspective_with_render_state(
        screen_coords,
        identity_source_barycentric(),
        reciprocal_w,
        shader,
        zbuffer,
        image,
        state,
    );
}

fn triangle_shader_perspective_with_render_state(
    screen_coords: &[Vec3; 3],
    source_barycentric: [Vec3; 3],
    reciprocal_w: [f32; 3],
    shader: &dyn Shader,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    render_state: RenderState,
) {
    let (width, height) = image.dimensions();
    let v0 = screen_coords[0];
    let v1 = screen_coords[1];
    let v2 = screen_coords[2];

    let Some((min_x, max_x, min_y, max_y)) =
        raster_bounds(screen_coords, width, height, render_state.scissor)
    else {
        return;
    };

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0.truncate(), v1.truncate(), v2.truncate(), p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                    let idx = x as usize + y as usize * width as usize;
                    if depth_passes(z, zbuffer[idx], render_state.depth) {
                        let source_bc = source_barycentric[0] * bc.x
                            + source_barycentric[1] * bc.y
                            + source_barycentric[2] * bc.z;
                        let corrected = perspective_correct_barycentric(source_bc, reciprocal_w);
                        if let Some(colour) = shader.fragment(corrected) {
                            if render_state.depth.write_enabled {
                                zbuffer[idx] = z;
                            }
                            if render_state.color_write {
                                put_pixel_checked(image, x, y, colour);
                            }
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
    triangle_zbuf_with_state(v0, v1, v2, zbuffer, image, colour, DepthState::default());
}

pub fn triangle_zbuf_with_state(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    colour: Rgb<u8>,
    depth_state: DepthState,
) {
    let state = RenderState {
        depth: depth_state,
        ..RenderState::default()
    };
    triangle_zbuf_with_render_state(v0, v1, v2, zbuffer, image, colour, state);
}

fn triangle_zbuf_with_render_state(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    colour: Rgb<u8>,
    render_state: RenderState,
) {
    let (width, height) = image.dimensions();
    let coords = [v0, v1, v2];
    let Some((min_x, max_x, min_y, max_y)) =
        raster_bounds(&coords, width, height, render_state.scissor)
    else {
        return;
    };

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = barycentric(v0.truncate(), v1.truncate(), v2.truncate(), p) {
                if bc.x >= 0.0 && bc.y >= 0.0 && bc.z >= 0.0 {
                    let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                    let idx = x as usize + y as usize * width as usize;
                    if depth_passes(z, zbuffer[idx], render_state.depth) {
                        if render_state.depth.write_enabled {
                            zbuffer[idx] = z;
                        }
                        if render_state.color_write {
                            put_pixel_checked(image, x, y, colour);
                        }
                    }
                }
            }
        }
    }
}

fn depth_passes(z: f32, current: f32, depth_state: DepthState) -> bool {
    !depth_state.test_enabled || z > current
}

fn identity_source_barycentric() -> [Vec3; 3] {
    [Vec3::X, Vec3::Y, Vec3::Z]
}

fn raster_bounds(
    screen_coords: &[Vec3; 3],
    width: u32,
    height: u32,
    scissor: Option<ScissorRect>,
) -> Option<(i32, i32, i32, i32)> {
    let mut min_x = screen_coords[0]
        .x
        .min(screen_coords[1].x)
        .min(screen_coords[2].x)
        .floor()
        .max(0.0) as i32;
    let mut max_x = screen_coords[0]
        .x
        .max(screen_coords[1].x)
        .max(screen_coords[2].x)
        .ceil()
        .min(width as f32 - 1.0) as i32;
    let mut min_y = screen_coords[0]
        .y
        .min(screen_coords[1].y)
        .min(screen_coords[2].y)
        .floor()
        .max(0.0) as i32;
    let mut max_y = screen_coords[0]
        .y
        .max(screen_coords[1].y)
        .max(screen_coords[2].y)
        .ceil()
        .min(height as f32 - 1.0) as i32;

    if let Some(scissor) = scissor {
        if scissor.width == 0 || scissor.height == 0 {
            return None;
        }
        min_x = min_x.max(scissor.x as i32);
        min_y = min_y.max(scissor.y as i32);
        max_x = max_x.min((scissor.x + scissor.width - 1) as i32);
        max_y = max_y.min((scissor.y + scissor.height - 1) as i32);
    }

    (min_x <= max_x && min_y <= max_y).then_some((min_x, max_x, min_y, max_y))
}

// --- Built-in shaders ---

/// Gouraud shader: per-vertex lighting interpolated across the triangle.
pub struct GouraudShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub light_dir: Vec3,
    pub material: Material,
    pub varying_intensity: [f32; 3],
}

impl<'a> GouraudShader<'a> {
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        light_dir: Vec3,
        material: Material,
    ) -> Self {
        GouraudShader {
            model,
            transform,
            model_view,
            light_dir: safe_normal(light_dir, Vec3::new(1.0, 1.0, -1.0).normalize()),
            material,
            varying_intensity: [0.0; 3],
        }
    }
}

impl Shader for GouraudShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        let normal = self.model.vertex_normal(face_idx, vert_idx);

        let normal_matrix = self.model_view.inverse().transpose();
        let n = safe_normal(transform_vector(&normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        let colour = shade_material(self.material.base_color, self.material, intensity, 0.0);
        if colour == Rgb([0, 0, 0]) {
            return None;
        }
        Some(colour)
    }
}

/// Texture-mapped shader with Gouraud lighting and optional diffuse texture.
pub struct TextureShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub light_dir: Vec3,
    pub diffuse: Option<&'a RgbImage>,
    pub sampler: TextureSampler,
    pub material: Material,
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
        sampler: TextureSampler,
        material: Material,
    ) -> Self {
        TextureShader {
            model,
            transform,
            model_view,
            light_dir: safe_normal(light_dir, Vec3::new(1.0, 1.0, -1.0).normalize()),
            diffuse,
            sampler,
            material,
            varying_intensity: [0.0; 3],
            varying_uv: [Vec2::ZERO; 3],
        }
    }
}

impl Shader for TextureShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];

        if let Some(ti) = face[vert_idx].vt {
            self.varying_uv[vert_idx] = self.model.texcoords[ti];
        }

        let normal = self.model.vertex_normal(face_idx, vert_idx);
        let normal_matrix = self.model_view.inverse().transpose();
        let n = safe_normal(transform_vector(&normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        if intensity <= 0.0 && self.material.ambient <= 0.0 {
            return None;
        }

        let base = if let Some(tex) = self.diffuse {
            let uv =
                self.varying_uv[0] * bc.x + self.varying_uv[1] * bc.y + self.varying_uv[2] * bc.z;
            tint_rgb(
                sample_texture(tex, uv, self.sampler),
                self.material.base_color,
            )
        } else {
            self.material.base_color
        };
        Some(shade_material(base, self.material, intensity, 0.0))
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
    pub sampler: TextureSampler,
    pub material: Material,
    pub varying_uv: [Vec2; 3],
    pub varying_pos: [Vec3; 3],
}

impl<'a> NormalMapShader<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        light_dir: Vec3,
        diffuse: Option<&'a RgbImage>,
        normal_map: &'a RgbImage,
        sampler: TextureSampler,
        material: Material,
    ) -> Self {
        NormalMapShader {
            model,
            transform,
            model_view,
            light_dir: safe_normal(light_dir, Vec3::new(1.0, 1.0, -1.0).normalize()),
            diffuse,
            normal_map,
            sampler,
            material,
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
        let uv = self.varying_uv[0] * bc.x + self.varying_uv[1] * bc.y + self.varying_uv[2] * bc.z;

        let np = sample_texture(self.normal_map, uv, self.sampler);
        let tangent_normal = Vec3::new(
            np[0] as f32 / 255.0 * 2.0 - 1.0,
            np[1] as f32 / 255.0 * 2.0 - 1.0,
            np[2] as f32 / 255.0 * 2.0 - 1.0,
        );

        let dp1 = self.varying_pos[1] - self.varying_pos[0];
        let dp2 = self.varying_pos[2] - self.varying_pos[0];
        let duv1 = self.varying_uv[1] - self.varying_uv[0];
        let duv2 = self.varying_uv[2] - self.varying_uv[0];
        let face_normal = safe_normal(dp1.cross(dp2), Vec3::Z);
        let det = duv1.x * duv2.y - duv1.y * duv2.x;

        let world_normal = if det.abs() < 1e-10 {
            face_normal
        } else {
            let inv_det = 1.0 / det;
            let tangent = safe_normal((dp1 * duv2.y - dp2 * duv1.y) * inv_det, Vec3::X);
            let bitangent = safe_normal((dp2 * duv1.x - dp1 * duv2.x) * inv_det, Vec3::Y);
            safe_normal(
                tangent * tangent_normal.x
                    + bitangent * tangent_normal.y
                    + face_normal * tangent_normal.z,
                face_normal,
            )
        };

        let normal_matrix = self.model_view.inverse().transpose();
        let n = safe_normal(transform_vector(&normal_matrix, world_normal), Vec3::Z);
        let intensity = n.dot(self.light_dir).max(0.0);
        if intensity <= 0.0 && self.material.ambient <= 0.0 {
            return None;
        }

        let base = self
            .diffuse
            .map(|tex| {
                tint_rgb(
                    sample_texture(tex, uv, self.sampler),
                    self.material.base_color,
                )
            })
            .unwrap_or(self.material.base_color);
        Some(shade_material(base, self.material, intensity, 0.0))
    }
}

/// Normal visualization shader for debug output and no-texture assets.
pub struct NormalDebugShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub varying_normal: [Vec3; 3],
}

impl<'a> NormalDebugShader<'a> {
    pub fn new(model: &'a Model, transform: Mat4, model_view: Mat4) -> Self {
        NormalDebugShader {
            model,
            transform,
            model_view,
            varying_normal: [Vec3::Z; 3],
        }
    }
}

impl Shader for NormalDebugShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        let normal = self.model.vertex_normal(face_idx, vert_idx);
        let normal_matrix = self.model_view.inverse().transpose();
        self.varying_normal[vert_idx] =
            safe_normal(transform_vector(&normal_matrix, normal), Vec3::Z);
        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let normal = safe_normal(
            self.varying_normal[0] * bc.x
                + self.varying_normal[1] * bc.y
                + self.varying_normal[2] * bc.z,
            Vec3::Z,
        );
        Some(Rgb([
            ((normal.x * 0.5 + 0.5) * 255.0) as u8,
            ((normal.y * 0.5 + 0.5) * 255.0) as u8,
            ((normal.z * 0.5 + 0.5) * 255.0) as u8,
        ]))
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
        Some(Rgb([255, 255, 255]))
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
    pub shadow_options: ShadowOptions,
    pub material: Material,
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
        shadow_options: ShadowOptions,
        material: Material,
    ) -> Self {
        ShadowShader {
            model,
            transform,
            model_view,
            shadow_transform,
            light_dir: safe_normal(light_dir, Vec3::new(1.0, 1.0, -1.0).normalize()),
            shadow_buffer,
            shadow_width,
            shadow_height,
            shadow_options,
            material,
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

        let normal = self.model.vertex_normal(face_idx, vert_idx);
        let normal_matrix = self.model_view.inverse().transpose();
        let n = safe_normal(transform_vector(&normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Rgb<u8>> {
        let intensity = self.varying_intensity[0] * bc.x
            + self.varying_intensity[1] * bc.y
            + self.varying_intensity[2] * bc.z;
        if intensity <= 0.0 && self.material.ambient <= 0.0 {
            return None;
        }

        let world_pos = self.varying_world_pos[0] * bc.x
            + self.varying_world_pos[1] * bc.y
            + self.varying_world_pos[2] * bc.z;
        let shadow_coord = transform_point(&self.shadow_transform, world_pos);

        let shadow = sample_shadow(
            self.shadow_buffer,
            self.shadow_width,
            self.shadow_height,
            shadow_coord,
            self.shadow_options,
        );

        Some(shade_material(
            self.material.base_color,
            self.material,
            intensity * shadow,
            0.0,
        ))
    }
}

fn sample_shadow(
    shadow_buffer: &[f32],
    width: u32,
    height: u32,
    shadow_coord: Vec3,
    options: ShadowOptions,
) -> f32 {
    let sx = shadow_coord.x.round() as i32;
    let sy = shadow_coord.y.round() as i32;

    match options.filter {
        ShadowFilter::None => sample_shadow_tap(
            shadow_buffer,
            width,
            height,
            sx,
            sy,
            shadow_coord.z,
            options.bias,
        ),
        ShadowFilter::Pcf => {
            let mut lit = 0.0;
            let mut samples = 0.0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    lit += sample_shadow_tap(
                        shadow_buffer,
                        width,
                        height,
                        sx + dx,
                        sy + dy,
                        shadow_coord.z,
                        options.bias,
                    );
                    samples += 1.0;
                }
            }
            lit / samples
        }
    }
}

fn sample_shadow_tap(
    shadow_buffer: &[f32],
    width: u32,
    height: u32,
    sx: i32,
    sy: i32,
    receiver_z: f32,
    bias: f32,
) -> f32 {
    if sx < 0 || sx >= width as i32 || sy < 0 || sy >= height as i32 {
        return 1.0;
    }
    let idx = sx as usize + sy as usize * width as usize;
    if shadow_buffer[idx].is_finite() && shadow_buffer[idx] > receiver_z + bias {
        0.35
    } else {
        1.0
    }
}

// --- Ambient Occlusion ---

/// Compute screen-space ambient occlusion from the z-buffer.
/// Returns an occlusion map (0.0 = fully occluded, 1.0 = no occlusion).
pub fn compute_ssao(
    zbuffer: &[f32],
    width: u32,
    height: u32,
    radius: i32,
    num_samples: usize,
) -> Vec<f32> {
    let mut ao = vec![1.0f32; (width * height) as usize];

    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let idx = x as usize + y as usize * width as usize;
            let z = zbuffer[idx];
            if !z.is_finite() {
                continue;
            }

            let mut occlusion = 0.0f32;
            let mut samples = 0;
            let step = ((2 * radius) as f32 / num_samples.max(1) as f32).max(1.0) as usize;
            for dy in (-radius..=radius).step_by(step) {
                for dx in (-radius..=radius).step_by(step) {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let sx = x + dx;
                    let sy = y + dy;
                    if sx < 0 || sx >= width as i32 || sy < 0 || sy >= height as i32 {
                        continue;
                    }
                    let sidx = sx as usize + sy as usize * width as usize;
                    let sz = zbuffer[sidx];
                    if sz.is_finite() && sz > z + 0.5 {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Wireframe,
    Flat,
    Gouraud,
    Texture,
    NormalMap,
    NormalDebug,
    Shadow,
    Ssao,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CullMode {
    Off,
    Front,
    Back,
}

impl CullMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CullMode::Off => "off",
            CullMode::Front => "front",
            CullMode::Back => "back",
        }
    }
}

impl FromStr for CullMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(CullMode::Off),
            "front" => Ok(CullMode::Front),
            "back" => Ok(CullMode::Back),
            _ => Err(format!(
                "unknown cull mode '{}'; expected one of: off, front, back",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontFace {
    Ccw,
    Cw,
}

impl FrontFace {
    pub fn as_str(self) -> &'static str {
        match self {
            FrontFace::Ccw => "ccw",
            FrontFace::Cw => "cw",
        }
    }
}

impl FromStr for FrontFace {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ccw" => Ok(FrontFace::Ccw),
            "cw" => Ok(FrontFace::Cw),
            _ => Err(format!(
                "unknown front-face '{}'; expected one of: ccw, cw",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMode {
    Solid,
    Wireframe,
}

impl FillMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FillMode::Solid => "solid",
            FillMode::Wireframe => "wireframe",
        }
    }
}

impl FromStr for FillMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "solid" => Ok(FillMode::Solid),
            "wireframe" => Ok(FillMode::Wireframe),
            _ => Err(format!(
                "unknown fill mode '{}'; expected one of: solid, wireframe",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureWrap {
    Repeat,
    Clamp,
}

impl TextureWrap {
    pub fn as_str(self) -> &'static str {
        match self {
            TextureWrap::Repeat => "repeat",
            TextureWrap::Clamp => "clamp",
        }
    }
}

impl FromStr for TextureWrap {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "repeat" => Ok(TextureWrap::Repeat),
            "clamp" => Ok(TextureWrap::Clamp),
            _ => Err(format!(
                "unknown texture wrap '{}'; expected one of: repeat, clamp",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFilter {
    Nearest,
    Linear,
}

impl TextureFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            TextureFilter::Nearest => "nearest",
            TextureFilter::Linear => "linear",
        }
    }
}

impl FromStr for TextureFilter {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nearest" => Ok(TextureFilter::Nearest),
            "linear" => Ok(TextureFilter::Linear),
            _ => Err(format!(
                "unknown texture filter '{}'; expected one of: nearest, linear",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureSampler {
    pub wrap: TextureWrap,
    pub filter: TextureFilter,
}

impl Default for TextureSampler {
    fn default() -> Self {
        TextureSampler {
            wrap: TextureWrap::Repeat,
            filter: TextureFilter::Nearest,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthState {
    pub test_enabled: bool,
    pub write_enabled: bool,
}

impl Default for DepthState {
    fn default() -> Self {
        DepthState {
            test_enabled: true,
            write_enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScissorRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ScissorRect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        let x = x as i64;
        let y = y as i64;
        let min_x = self.x as i64;
        let min_y = self.y as i64;
        let max_x = min_x + self.width as i64;
        let max_y = min_y + self.height as i64;
        x >= min_x && x < max_x && y >= min_y && y < max_y
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub base_color: Rgb<u8>,
    pub ambient: f32,
    pub diffuse: f32,
    pub specular: f32,
    pub shininess: f32,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            base_color: Rgb([220, 220, 220]),
            ambient: 0.0,
            diffuse: 1.0,
            specular: 0.0,
            shininess: 32.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowFilter {
    None,
    Pcf,
}

impl ShadowFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowFilter::None => "none",
            ShadowFilter::Pcf => "pcf",
        }
    }
}

impl FromStr for ShadowFilter {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(ShadowFilter::None),
            "pcf" => Ok(ShadowFilter::Pcf),
            _ => Err(format!(
                "unknown shadow filter '{}'; expected one of: none, pcf",
                value
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowOptions {
    pub size: u32,
    pub bias: f32,
    pub filter: ShadowFilter,
}

impl Default for ShadowOptions {
    fn default() -> Self {
        ShadowOptions {
            size: 0,
            bias: 4.0,
            filter: ShadowFilter::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderState {
    pub cull_mode: CullMode,
    pub front_face: FrontFace,
    pub depth: DepthState,
    pub fill_mode: FillMode,
    pub scissor: Option<ScissorRect>,
    pub clear_color: Rgb<u8>,
    pub clear_depth: f32,
    pub color_write: bool,
}

impl Default for RenderState {
    fn default() -> Self {
        RenderState {
            cull_mode: CullMode::Off,
            front_face: FrontFace::Ccw,
            depth: DepthState::default(),
            fill_mode: FillMode::Solid,
            scissor: None,
            clear_color: DEFAULT_BACKGROUND,
            clear_depth: f32::NEG_INFINITY,
            color_write: true,
        }
    }
}

impl RenderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RenderMode::Wireframe => "wireframe",
            RenderMode::Flat => "flat",
            RenderMode::Gouraud => "gouraud",
            RenderMode::Texture => "texture",
            RenderMode::NormalMap => "normal-map",
            RenderMode::NormalDebug => "normal-debug",
            RenderMode::Shadow => "shadow",
            RenderMode::Ssao => "ssao",
        }
    }

    pub fn all() -> &'static [&'static str] {
        &[
            "wireframe",
            "flat",
            "gouraud",
            "texture",
            "normal-map",
            "normal-debug",
            "shadow",
            "ssao",
        ]
    }
}

impl FromStr for RenderMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "wireframe" => Ok(RenderMode::Wireframe),
            "flat" => Ok(RenderMode::Flat),
            "gouraud" => Ok(RenderMode::Gouraud),
            "texture" => Ok(RenderMode::Texture),
            "normal-map" => Ok(RenderMode::NormalMap),
            "normal-debug" | "normal" | "normals" => Ok(RenderMode::NormalDebug),
            "shadow" => Ok(RenderMode::Shadow),
            "ssao" | "ao" => Ok(RenderMode::Ssao),
            _ => Err(format!(
                "unknown shader '{}'; expected one of: {}",
                value,
                RenderMode::all().join(", ")
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub mode: RenderMode,
    pub camera_eye: Option<Vec3>,
    pub light_dir: Vec3,
    pub debug_outputs: bool,
    pub render_state: RenderState,
    pub texture: Option<RgbImage>,
    pub normal_map: Option<RgbImage>,
    pub texture_sampler: TextureSampler,
    pub material: Material,
    pub shadow: ShadowOptions,
    pub near_plane: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 1024,
            height: 1024,
            mode: RenderMode::Shadow,
            camera_eye: None,
            light_dir: Vec3::new(1.0, 1.0, -1.0).normalize(),
            debug_outputs: false,
            render_state: RenderState::default(),
            texture: None,
            normal_map: None,
            texture_sampler: TextureSampler::default(),
            material: Material::default(),
            shadow: ShadowOptions::default(),
            near_plane: 0.01,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DebugImages {
    pub depth: Option<RgbImage>,
    pub shadow: Option<RgbImage>,
    pub normals: Option<RgbImage>,
    pub ao: Option<RgbImage>,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderStats {
    pub width: u32,
    pub height: u32,
    pub vertices: usize,
    pub triangles: usize,
    pub mode: RenderMode,
}

#[derive(Clone, Debug)]
pub struct RenderOutput {
    pub image: RgbImage,
    pub zbuffer: Vec<f32>,
    pub debug: DebugImages,
    pub stats: RenderStats,
}

struct RenderTransforms {
    transform: Mat4,
    model_view: Mat4,
    view_to_screen: Mat4,
    light_transform: Mat4,
    light_model_view: Mat4,
    light_view_to_screen: Mat4,
}

pub fn render_model(model: &Model, options: &RenderOptions) -> RenderOutput {
    let width = options.width.max(1);
    let height = options.height.max(1);
    let (shadow_width, shadow_height) = shadow_dimensions(options, width, height);
    let transforms = build_transforms(model, options, width, height, shadow_width, shadow_height);
    let mut image = ImageBuffer::from_pixel(width, height, options.render_state.clear_color);
    let mut zbuffer = vec![options.render_state.clear_depth; (width * height) as usize];
    let mut debug = DebugImages::default();

    if options.mode == RenderMode::Wireframe
        || options.render_state.fill_mode == FillMode::Wireframe
    {
        render_wireframe(model, &transforms, options, &mut image);
    } else {
        match options.mode {
            RenderMode::Wireframe => unreachable!("wireframe mode is handled before solid modes"),
            RenderMode::Flat => render_flat(
                model,
                &transforms,
                options,
                options.light_dir,
                &mut zbuffer,
                &mut image,
            ),
            RenderMode::Gouraud => {
                let mut shader = GouraudShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    options.light_dir,
                    options.material,
                );
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                );
            }
            RenderMode::Texture => {
                let checker = checker_texture(128);
                let diffuse = options
                    .texture
                    .as_ref()
                    .or_else(|| (!model.texcoords.is_empty()).then_some(&checker));
                let mut shader = TextureShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    options.light_dir,
                    diffuse,
                    options.texture_sampler,
                    options.material,
                );
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                );
            }
            RenderMode::NormalMap => {
                let neutral_normal_map = neutral_normal_map(2);
                let normal_map = options.normal_map.as_ref().unwrap_or(&neutral_normal_map);
                let diffuse = options.texture.as_ref();
                let mut shader = NormalMapShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    options.light_dir,
                    diffuse,
                    normal_map,
                    options.texture_sampler,
                    options.material,
                );
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                );
            }
            RenderMode::NormalDebug => {
                let mut shader =
                    NormalDebugShader::new(model, transforms.transform, transforms.model_view);
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                );
            }
            RenderMode::Shadow | RenderMode::Ssao => {
                let shadow_zbuffer =
                    render_shadow_map(model, &transforms, shadow_width, shadow_height, options);
                if options.debug_outputs {
                    debug.shadow = Some(zbuffer_to_image(
                        &shadow_zbuffer,
                        shadow_width,
                        shadow_height,
                    ));
                }

                let mut shader = ShadowShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    transforms.light_transform,
                    options.light_dir,
                    &shadow_zbuffer,
                    shadow_width,
                    shadow_height,
                    options.shadow,
                    options.material,
                );
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                );

                if options.mode == RenderMode::Ssao {
                    let ao = compute_ssao(&zbuffer, width, height, 5, 8);
                    if options.debug_outputs {
                        debug.ao = Some(ao_to_image(&ao, width, height));
                    }
                    apply_ssao(&mut image, &ao, width, height);
                }
            }
        }
    }

    if options.debug_outputs && debug.normals.is_none() {
        debug.normals = Some(render_normal_debug(
            model,
            &transforms,
            width,
            height,
            options,
        ));
    }
    if options.debug_outputs && debug.depth.is_none() {
        debug.depth = Some(zbuffer_to_image(&zbuffer, width, height));
    }

    flip_vertical_in_place(&mut image);
    flip_debug_images(&mut debug);

    RenderOutput {
        image,
        zbuffer,
        debug,
        stats: RenderStats {
            width,
            height,
            vertices: model.vertices.len(),
            triangles: model.faces.len(),
            mode: options.mode,
        },
    }
}

fn build_transforms(
    model: &Model,
    options: &RenderOptions,
    width: u32,
    height: u32,
    shadow_width: u32,
    shadow_height: u32,
) -> RenderTransforms {
    let bounds = model.bounds().unwrap_or(ModelBounds {
        min: Vec3::splat(-1.0),
        max: Vec3::splat(1.0),
    });
    let model_center = bounds.center();
    let model_scale = 2.0 / bounds.radius();
    let model_matrix =
        Mat4::from_scale(Vec3::splat(model_scale)) * Mat4::from_translation(-model_center);

    let eye = options.camera_eye.unwrap_or(Vec3::new(0.0, 0.25, 3.0));
    let center = Vec3::ZERO;
    let up = Vec3::Y;
    let view = lookat(eye, center, up);
    let distance = eye.distance(center).max(1.0);
    let proj = projection(-1.0 / distance);
    let margin_x = width as f32 * 0.08;
    let margin_y = height as f32 * 0.08;
    let vp = viewport(
        margin_x,
        margin_y,
        width as f32 - margin_x * 2.0,
        height as f32 - margin_y * 2.0,
        255.0,
    );
    let view_to_screen = vp * proj;
    let transform = view_to_screen * view * model_matrix;
    let model_view = view * model_matrix;

    let light_dir = safe_normal(options.light_dir, Vec3::new(1.0, 1.0, -1.0).normalize());
    let light_eye = light_dir * 3.0;
    let light_view = lookat(light_eye, center, up);
    let light_proj = projection(-1.0 / light_eye.distance(center).max(1.0));
    let shadow_margin_x = shadow_width as f32 * 0.08;
    let shadow_margin_y = shadow_height as f32 * 0.08;
    let shadow_vp = viewport(
        shadow_margin_x,
        shadow_margin_y,
        shadow_width as f32 - shadow_margin_x * 2.0,
        shadow_height as f32 - shadow_margin_y * 2.0,
        255.0,
    );
    let light_view_to_screen = shadow_vp * light_proj;
    let light_transform = light_view_to_screen * light_view * model_matrix;
    let light_model_view = light_view * model_matrix;
    RenderTransforms {
        transform,
        model_view,
        view_to_screen,
        light_transform,
        light_model_view,
        light_view_to_screen,
    }
}

fn render_wireframe(
    model: &Model,
    transforms: &RenderTransforms,
    options: &RenderOptions,
    image: &mut RgbImage,
) {
    let colour = Rgb([235, 238, 245]);
    for face_idx in 0..model.faces.len() {
        for primitive in
            clipped_primitives_for_face(model, transforms, face_idx, options.near_plane)
        {
            if should_cull_screen_triangle_with_front_face(
                &primitive.screen_coords,
                options.render_state.cull_mode,
                options.render_state.front_face,
            ) {
                continue;
            }
            for edge in [(0, 1), (1, 2), (2, 0)] {
                draw_line_with_state(
                    primitive.screen_coords[edge.0].x.round() as i32,
                    primitive.screen_coords[edge.0].y.round() as i32,
                    primitive.screen_coords[edge.1].x.round() as i32,
                    primitive.screen_coords[edge.1].y.round() as i32,
                    image,
                    colour,
                    options.render_state,
                );
            }
        }
    }
}

fn render_flat(
    model: &Model,
    transforms: &RenderTransforms,
    options: &RenderOptions,
    light_dir: Vec3,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
) {
    let normal_matrix = transforms.model_view.inverse().transpose();
    let light_dir = safe_normal(light_dir, Vec3::new(1.0, 1.0, -1.0).normalize());
    for face_idx in 0..model.faces.len() {
        let normal = safe_normal(
            transform_vector(&normal_matrix, model.face_normal(face_idx)),
            Vec3::Z,
        );
        let intensity = normal.dot(light_dir).max(0.0);
        let shaded = shade_material(
            options.material.base_color,
            options.material,
            intensity,
            0.0,
        );
        if shaded == Rgb([0, 0, 0]) {
            continue;
        }
        for primitive in
            clipped_primitives_for_face(model, transforms, face_idx, options.near_plane)
        {
            if should_cull_screen_triangle_with_front_face(
                &primitive.screen_coords,
                options.render_state.cull_mode,
                options.render_state.front_face,
            ) {
                continue;
            }
            triangle_zbuf_with_render_state(
                primitive.screen_coords[0],
                primitive.screen_coords[1],
                primitive.screen_coords[2],
                zbuffer,
                image,
                shaded,
                options.render_state,
            );
        }
    }
}

fn render_shader<S: Shader>(
    model: &Model,
    transforms: &RenderTransforms,
    shader: &mut S,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    options: &RenderOptions,
) {
    for face_idx in 0..model.faces.len() {
        let _: [Vec3; 3] = std::array::from_fn(|j| shader.vertex(face_idx, j));
        let reciprocal_w = reciprocal_w_for_face(model, transforms.transform, face_idx);
        for primitive in
            clipped_primitives_for_face(model, transforms, face_idx, options.near_plane)
        {
            if should_cull_screen_triangle_with_front_face(
                &primitive.screen_coords,
                options.render_state.cull_mode,
                options.render_state.front_face,
            ) {
                continue;
            }
            triangle_shader_perspective_with_render_state(
                &primitive.screen_coords,
                primitive.source_barycentric,
                reciprocal_w,
                shader,
                zbuffer,
                image,
                options.render_state,
            );
        }
    }
}

fn reciprocal_w_for_face(model: &Model, transform: Mat4, face_idx: usize) -> [f32; 3] {
    let face = model.faces[face_idx];
    std::array::from_fn(|j| {
        let clip = transform_homogeneous(&transform, model.vertices[face[j].v]);
        if clip.w.abs() > EPSILON {
            1.0 / clip.w
        } else {
            1.0
        }
    })
}

fn should_cull_screen_triangle_with_front_face(
    screen_coords: &[Vec3; 3],
    cull_mode: CullMode,
    front_face: FrontFace,
) -> bool {
    if cull_mode == CullMode::Off {
        return false;
    }

    let a = screen_coords[0].truncate();
    let b = screen_coords[1].truncate();
    let c = screen_coords[2].truncate();
    let signed_area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let front_facing = match front_face {
        FrontFace::Ccw => signed_area > 0.0,
        FrontFace::Cw => signed_area < 0.0,
    };

    match cull_mode {
        CullMode::Off => false,
        CullMode::Front => front_facing,
        CullMode::Back => !front_facing,
    }
}

#[derive(Clone, Copy, Debug)]
struct ClippedVertex {
    view_pos: Vec3,
    source_bc: Vec3,
}

#[derive(Clone, Copy, Debug)]
struct ClippedPrimitive {
    screen_coords: [Vec3; 3],
    source_barycentric: [Vec3; 3],
}

fn clipped_primitives_for_face(
    model: &Model,
    transforms: &RenderTransforms,
    face_idx: usize,
    near_plane: f32,
) -> Vec<ClippedPrimitive> {
    let face = model.faces[face_idx];
    let triangle = [
        ClippedVertex {
            view_pos: transform_point(&transforms.model_view, model.vertices[face[0].v]),
            source_bc: Vec3::X,
        },
        ClippedVertex {
            view_pos: transform_point(&transforms.model_view, model.vertices[face[1].v]),
            source_bc: Vec3::Y,
        },
        ClippedVertex {
            view_pos: transform_point(&transforms.model_view, model.vertices[face[2].v]),
            source_bc: Vec3::Z,
        },
    ];

    clip_varying_triangle_against_near_plane(triangle, near_plane)
        .into_iter()
        .map(|tri| ClippedPrimitive {
            screen_coords: std::array::from_fn(|i| {
                transform_point(&transforms.view_to_screen, tri[i].view_pos)
            }),
            source_barycentric: std::array::from_fn(|i| tri[i].source_bc),
        })
        .collect()
}

fn clip_varying_triangle_against_near_plane(
    triangle: [ClippedVertex; 3],
    near: f32,
) -> Vec<[ClippedVertex; 3]> {
    let near_z = -near.max(EPSILON);
    let mut polygon = Vec::with_capacity(4);

    for i in 0..3 {
        let current = triangle[i];
        let next = triangle[(i + 1) % 3];
        let current_inside = current.view_pos.z <= near_z;
        let next_inside = next.view_pos.z <= near_z;

        match (current_inside, next_inside) {
            (true, true) => polygon.push(next),
            (true, false) => polygon.push(intersect_clipped_vertex(current, next, near_z)),
            (false, true) => {
                polygon.push(intersect_clipped_vertex(current, next, near_z));
                polygon.push(next);
            }
            (false, false) => {}
        }
    }

    match polygon.len() {
        0..=2 => Vec::new(),
        3 => vec![[polygon[0], polygon[1], polygon[2]]],
        4 => vec![
            [polygon[0], polygon[1], polygon[2]],
            [polygon[0], polygon[2], polygon[3]],
        ],
        _ => unreachable!("triangle clipping can produce at most four vertices"),
    }
}

fn intersect_clipped_vertex(a: ClippedVertex, b: ClippedVertex, near_z: f32) -> ClippedVertex {
    let t = (near_z - a.view_pos.z) / (b.view_pos.z - a.view_pos.z);
    ClippedVertex {
        view_pos: a.view_pos + (b.view_pos - a.view_pos) * t,
        source_bc: a.source_bc + (b.source_bc - a.source_bc) * t,
    }
}

/// Clip a view-space triangle against the near plane.
///
/// This helper follows the conventional camera convention where visible geometry
/// is at negative view-space z values and the near plane is `z = -near`.
pub fn clip_triangle_against_near_plane(triangle: [Vec3; 3], near: f32) -> Vec<[Vec3; 3]> {
    let near_z = -near.max(EPSILON);
    let mut polygon = Vec::with_capacity(4);

    for i in 0..3 {
        let current = triangle[i];
        let next = triangle[(i + 1) % 3];
        let current_inside = current.z <= near_z;
        let next_inside = next.z <= near_z;

        match (current_inside, next_inside) {
            (true, true) => polygon.push(next),
            (true, false) => polygon.push(intersect_near(current, next, near_z)),
            (false, true) => {
                polygon.push(intersect_near(current, next, near_z));
                polygon.push(next);
            }
            (false, false) => {}
        }
    }

    match polygon.len() {
        0..=2 => Vec::new(),
        3 => vec![[polygon[0], polygon[1], polygon[2]]],
        4 => vec![
            [polygon[0], polygon[1], polygon[2]],
            [polygon[0], polygon[2], polygon[3]],
        ],
        _ => unreachable!("triangle clipping can produce at most four vertices"),
    }
}

fn intersect_near(a: Vec3, b: Vec3, near_z: f32) -> Vec3 {
    let t = (near_z - a.z) / (b.z - a.z);
    a + (b - a) * t
}

fn render_shadow_map(
    model: &Model,
    transforms: &RenderTransforms,
    width: u32,
    height: u32,
    options: &RenderOptions,
) -> Vec<f32> {
    let mut shadow_image = ImageBuffer::new(width, height);
    let mut shadow_zbuffer = vec![options.render_state.clear_depth; (width * height) as usize];
    let mut depth_shader = DepthShader {
        model,
        transform: transforms.light_transform,
    };
    let shadow_transforms = RenderTransforms {
        transform: transforms.light_transform,
        model_view: transforms.light_model_view,
        view_to_screen: transforms.light_view_to_screen,
        light_transform: transforms.light_transform,
        light_model_view: transforms.light_model_view,
        light_view_to_screen: transforms.light_view_to_screen,
    };
    render_shader(
        model,
        &shadow_transforms,
        &mut depth_shader,
        &mut shadow_zbuffer,
        &mut shadow_image,
        options,
    );
    shadow_zbuffer
}

fn render_normal_debug(
    model: &Model,
    transforms: &RenderTransforms,
    width: u32,
    height: u32,
    options: &RenderOptions,
) -> RgbImage {
    let mut image = ImageBuffer::from_pixel(width, height, DEFAULT_BACKGROUND);
    let mut zbuffer = vec![f32::NEG_INFINITY; (width * height) as usize];
    let mut shader = NormalDebugShader::new(model, transforms.transform, transforms.model_view);
    render_shader(
        model,
        transforms,
        &mut shader,
        &mut zbuffer,
        &mut image,
        options,
    );
    image
}

fn shadow_dimensions(options: &RenderOptions, width: u32, height: u32) -> (u32, u32) {
    if options.shadow.size == 0 {
        (width, height)
    } else {
        let size = options.shadow.size.max(1);
        (size, size)
    }
}

fn checker_texture(size: u32) -> RgbImage {
    ImageBuffer::from_fn(size, size, |x, y| {
        let tile = ((x / 16) + (y / 16)) % 2;
        if tile == 0 {
            Rgb([238, 238, 238])
        } else {
            Rgb([80, 110, 180])
        }
    })
}

fn neutral_normal_map(size: u32) -> RgbImage {
    ImageBuffer::from_pixel(size.max(1), size.max(1), Rgb([128, 128, 255]))
}

pub fn sample_texture(texture: &RgbImage, uv: Vec2, sampler: TextureSampler) -> Rgb<u8> {
    let (tw, th) = texture.dimensions();
    let (u, v) = match sampler.wrap {
        TextureWrap::Repeat => (uv.x.rem_euclid(1.0), uv.y.rem_euclid(1.0)),
        TextureWrap::Clamp => (uv.x.clamp(0.0, 1.0), uv.y.clamp(0.0, 1.0)),
    };
    match sampler.filter {
        TextureFilter::Nearest => {
            let tx = (u * (tw - 1) as f32).round() as u32;
            let ty = ((1.0 - v) * (th - 1) as f32).round() as u32;
            *texture.get_pixel(tx, ty)
        }
        TextureFilter::Linear => {
            let x = u * (tw - 1) as f32;
            let y = (1.0 - v) * (th - 1) as f32;
            let x0 = x.floor() as u32;
            let y0 = y.floor() as u32;
            let x1 = (x0 + 1).min(tw - 1);
            let y1 = (y0 + 1).min(th - 1);
            let tx = x - x.floor();
            let ty = y - y.floor();
            let c00 = *texture.get_pixel(x0, y0);
            let c10 = *texture.get_pixel(x1, y0);
            let c01 = *texture.get_pixel(x0, y1);
            let c11 = *texture.get_pixel(x1, y1);
            lerp_rgb(lerp_rgb(c00, c10, tx), lerp_rgb(c01, c11, tx), ty)
        }
    }
}

fn lerp_rgb(a: Rgb<u8>, b: Rgb<u8>, t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    Rgb([
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ])
}

fn tint_rgb(pixel: Rgb<u8>, tint: Rgb<u8>) -> Rgb<u8> {
    Rgb([
        (pixel[0] as u16 * tint[0] as u16 / 255) as u8,
        (pixel[1] as u16 * tint[1] as u16 / 255) as u8,
        (pixel[2] as u16 * tint[2] as u16 / 255) as u8,
    ])
}

fn shade_material(
    base: Rgb<u8>,
    material: Material,
    diffuse_intensity: f32,
    specular: f32,
) -> Rgb<u8> {
    let diffuse = diffuse_intensity.max(0.0) * material.diffuse.max(0.0);
    let specular =
        specular.max(0.0).powf(material.shininess.max(1.0) / 32.0) * material.specular.max(0.0);
    let factor = (material.ambient.max(0.0) + diffuse).clamp(0.0, 1.0);
    Rgb([
        (base[0] as f32 * factor + 255.0 * specular).clamp(0.0, 255.0) as u8,
        (base[1] as f32 * factor + 255.0 * specular).clamp(0.0, 255.0) as u8,
        (base[2] as f32 * factor + 255.0 * specular).clamp(0.0, 255.0) as u8,
    ])
}

pub fn zbuffer_to_image(zbuffer: &[f32], width: u32, height: u32) -> RgbImage {
    let finite_values = zbuffer.iter().copied().filter(|z| z.is_finite());
    let (mut min_z, mut max_z) = (f32::INFINITY, f32::NEG_INFINITY);
    for z in finite_values {
        min_z = min_z.min(z);
        max_z = max_z.max(z);
    }
    if !min_z.is_finite() || !max_z.is_finite() || (max_z - min_z).abs() < EPSILON {
        return ImageBuffer::from_pixel(width, height, Rgb([0, 0, 0]));
    }

    ImageBuffer::from_fn(width, height, |x, y| {
        let idx = x as usize + y as usize * width as usize;
        let z = zbuffer[idx];
        if z.is_finite() {
            let v = ((z - min_z) / (max_z - min_z) * 255.0) as u8;
            Rgb([v, v, v])
        } else {
            Rgb([0, 0, 0])
        }
    })
}

pub fn ao_to_image(ao: &[f32], width: u32, height: u32) -> RgbImage {
    ImageBuffer::from_fn(width, height, |x, y| {
        let idx = x as usize + y as usize * width as usize;
        let v = (ao[idx].clamp(0.0, 1.0) * 255.0) as u8;
        Rgb([v, v, v])
    })
}

fn flip_debug_images(debug: &mut DebugImages) {
    if let Some(image) = &mut debug.depth {
        flip_vertical_in_place(image);
    }
    if let Some(image) = &mut debug.shadow {
        flip_vertical_in_place(image);
    }
    if let Some(image) = &mut debug.normals {
        flip_vertical_in_place(image);
    }
    if let Some(image) = &mut debug.ao {
        flip_vertical_in_place(image);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_obj(contents: &str) -> String {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("octo-render-test-{id}.obj"));
        fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn tiny_model() -> Model {
        Model {
            vertices: vec![
                Vec3::new(-0.6, -0.6, 0.0),
                Vec3::new(0.6, -0.6, 0.0),
                Vec3::new(0.0, 0.6, 0.0),
            ],
            texcoords: Vec::new(),
            normals: vec![Vec3::new(0.0, 0.0, -1.0)],
            faces: vec![[
                FaceVertex {
                    v: 0,
                    vt: None,
                    vn: Some(0),
                },
                FaceVertex {
                    v: 1,
                    vt: None,
                    vn: Some(0),
                },
                FaceVertex {
                    v: 2,
                    vt: None,
                    vn: Some(0),
                },
            ]],
        }
    }

    #[test]
    fn obj_loader_triangulates_quads_and_resolves_negative_indices() {
        let path = temp_obj(
            "\
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
f -4 -3 -2 -1
",
        );
        let model = Model::load(path).unwrap();
        assert_eq!(model.vertices.len(), 4);
        assert_eq!(model.faces.len(), 2);
        assert_eq!(model.faces[0][0].v, 0);
        assert_eq!(model.faces[1][2].v, 3);
    }

    #[test]
    fn obj_loader_reports_malformed_input() {
        let path = temp_obj("v 0 nope 0\nf 1 2 3\n");
        let err = Model::load(path).unwrap_err();
        assert!(err.contains("invalid vertex y value"));
    }

    #[test]
    fn obj_loader_parses_uvs_and_normals() {
        let path = temp_obj(
            "\
v 0 0 0
v 1 0 0
v 0 1 0
vt 0 0
vt 1 0
vt 0 1
vn 0 0 -1
f 1/1/1 2/2/1 3/3/1
",
        );
        let model = Model::load(path).unwrap();
        assert_eq!(model.texcoords.len(), 3);
        assert_eq!(model.normals.len(), 1);
        assert_eq!(model.faces[0][1].vt, Some(1));
        assert_eq!(model.faces[0][2].vn, Some(0));
    }

    #[test]
    fn draw_line_includes_both_endpoints() {
        let red = Rgb([255, 0, 0]);
        let mut image = ImageBuffer::from_pixel(4, 1, Rgb([0, 0, 0]));
        draw_line(0, 0, 2, 0, &mut image, red);
        assert_eq!(*image.get_pixel(0, 0), red);
        assert_eq!(*image.get_pixel(1, 0), red);
        assert_eq!(*image.get_pixel(2, 0), red);
        assert_eq!(*image.get_pixel(3, 0), Rgb([0, 0, 0]));
    }

    #[test]
    fn triangle_covers_expected_pixels() {
        let red = Rgb([255, 0, 0]);
        let mut image = ImageBuffer::from_pixel(4, 4, Rgb([0, 0, 0]));
        triangle(
            Vec2::new(0.0, 0.0),
            Vec2::new(3.0, 0.0),
            Vec2::new(0.0, 3.0),
            &mut image,
            red,
        );
        assert_eq!(*image.get_pixel(1, 1), red);
        assert_eq!(*image.get_pixel(3, 3), Rgb([0, 0, 0]));
    }

    #[test]
    fn zbuffer_keeps_nearest_written_triangle() {
        let red = Rgb([255, 0, 0]);
        let green = Rgb([0, 255, 0]);
        let mut image = ImageBuffer::from_pixel(4, 4, Rgb([0, 0, 0]));
        let mut zbuffer = vec![f32::NEG_INFINITY; 16];
        triangle_zbuf(
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(3.0, 0.0, 0.1),
            Vec3::new(0.0, 3.0, 0.1),
            &mut zbuffer,
            &mut image,
            red,
        );
        triangle_zbuf(
            Vec3::new(0.0, 0.0, 0.2),
            Vec3::new(3.0, 0.0, 0.2),
            Vec3::new(0.0, 3.0, 0.2),
            &mut zbuffer,
            &mut image,
            green,
        );
        assert_eq!(*image.get_pixel(1, 1), green);
    }

    #[test]
    fn depth_state_can_disable_test_and_writes() {
        let red = Rgb([255, 0, 0]);
        let green = Rgb([0, 255, 0]);
        let mut image = ImageBuffer::from_pixel(4, 4, Rgb([0, 0, 0]));
        let mut zbuffer = vec![0.5; 16];

        triangle_zbuf_with_state(
            Vec3::new(0.0, 0.0, 0.1),
            Vec3::new(3.0, 0.0, 0.1),
            Vec3::new(0.0, 3.0, 0.1),
            &mut zbuffer,
            &mut image,
            red,
            DepthState {
                test_enabled: false,
                write_enabled: false,
            },
        );
        assert_eq!(*image.get_pixel(1, 1), red);
        assert_eq!(zbuffer[5], 0.5);

        triangle_zbuf_with_state(
            Vec3::new(0.0, 0.0, 0.6),
            Vec3::new(3.0, 0.0, 0.6),
            Vec3::new(0.0, 3.0, 0.6),
            &mut zbuffer,
            &mut image,
            green,
            DepthState::default(),
        );
        assert_eq!(*image.get_pixel(1, 1), green);
        assert_eq!(zbuffer[5], 0.6);
    }

    #[test]
    fn viewport_maps_origin_to_center() {
        let vp = viewport(0.0, 0.0, 100.0, 80.0, 255.0);
        let p = transform_point(&vp, Vec3::ZERO);
        assert!((p.x - 50.0).abs() < EPSILON);
        assert!((p.y - 40.0).abs() < EPSILON);
        assert!((p.z - 127.5).abs() < EPSILON);
    }

    #[test]
    fn ssao_output_is_deterministic_and_bounded() {
        let zbuffer = vec![1.0, 1.0, 1.0, 1.0, 0.0, 2.0, 1.0, 1.0, 1.0];
        let ao = compute_ssao(&zbuffer, 3, 3, 1, 4);
        assert_eq!(ao.len(), 9);
        assert!(ao.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(ao[4] < 1.0);
    }

    #[test]
    fn culling_classifies_screen_space_winding() {
        let ccw = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        assert!(should_cull_screen_triangle_with_front_face(
            &ccw,
            CullMode::Front,
            FrontFace::Ccw
        ));
        assert!(!should_cull_screen_triangle_with_front_face(
            &ccw,
            CullMode::Back,
            FrontFace::Ccw
        ));
        assert!(!should_cull_screen_triangle_with_front_face(
            &ccw,
            CullMode::Off,
            FrontFace::Ccw
        ));
        assert!(!should_cull_screen_triangle_with_front_face(
            &ccw,
            CullMode::Front,
            FrontFace::Cw
        ));
        assert!(should_cull_screen_triangle_with_front_face(
            &ccw,
            CullMode::Back,
            FrontFace::Cw
        ));
    }

    #[test]
    fn texture_sampling_supports_repeat_and_clamp_wrap() {
        let tex = ImageBuffer::from_fn(2, 2, |x, y| match (x, y) {
            (0, 0) => Rgb([255, 0, 0]),
            (1, 0) => Rgb([0, 255, 0]),
            (0, 1) => Rgb([0, 0, 255]),
            _ => Rgb([255, 255, 255]),
        });
        let repeated = sample_texture(
            &tex,
            Vec2::new(1.25, 0.25),
            TextureSampler {
                wrap: TextureWrap::Repeat,
                filter: TextureFilter::Nearest,
            },
        );
        let clamped = sample_texture(
            &tex,
            Vec2::new(1.25, 0.25),
            TextureSampler {
                wrap: TextureWrap::Clamp,
                filter: TextureFilter::Nearest,
            },
        );
        assert_ne!(repeated, clamped);
    }

    #[test]
    fn texture_sampling_supports_linear_filtering() {
        let tex = ImageBuffer::from_fn(2, 2, |x, y| match (x, y) {
            (0, 0) => Rgb([0, 0, 0]),
            (1, 0) => Rgb([100, 0, 0]),
            (0, 1) => Rgb([0, 100, 0]),
            _ => Rgb([100, 100, 0]),
        });
        let nearest = sample_texture(
            &tex,
            Vec2::new(0.5, 0.5),
            TextureSampler {
                wrap: TextureWrap::Clamp,
                filter: TextureFilter::Nearest,
            },
        );
        let linear = sample_texture(
            &tex,
            Vec2::new(0.5, 0.5),
            TextureSampler {
                wrap: TextureWrap::Clamp,
                filter: TextureFilter::Linear,
            },
        );
        assert_ne!(nearest, linear);
        assert_eq!(linear, Rgb([50, 50, 0]));
    }

    #[test]
    fn near_plane_clip_splits_crossing_triangle() {
        let clipped = clip_triangle_against_near_plane(
            [
                Vec3::new(-1.0, -1.0, -2.0),
                Vec3::new(1.0, -1.0, -2.0),
                Vec3::new(0.0, 1.0, 0.5),
            ],
            0.1,
        );
        assert_eq!(clipped.len(), 2);
        assert!(clipped
            .iter()
            .flatten()
            .all(|vertex| vertex.z <= -0.1 + EPSILON));
    }

    #[test]
    fn near_plane_clip_preserves_source_barycentric_coordinates() {
        let clipped = clip_varying_triangle_against_near_plane(
            [
                ClippedVertex {
                    view_pos: Vec3::new(-1.0, -1.0, -2.0),
                    source_bc: Vec3::X,
                },
                ClippedVertex {
                    view_pos: Vec3::new(1.0, -1.0, -2.0),
                    source_bc: Vec3::Y,
                },
                ClippedVertex {
                    view_pos: Vec3::new(0.0, 1.0, 0.5),
                    source_bc: Vec3::Z,
                },
            ],
            0.1,
        );
        assert_eq!(clipped.len(), 2);
        assert!(clipped.iter().flatten().all(|vertex| {
            (vertex.source_bc.x + vertex.source_bc.y + vertex.source_bc.z - 1.0).abs() < EPSILON
        }));
    }

    #[test]
    fn scissor_limits_raster_output() {
        let clear = Rgb([3, 4, 5]);
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, -1.0),
            render_state: RenderState {
                clear_color: clear,
                scissor: Some(ScissorRect {
                    x: 24,
                    y: 24,
                    width: 8,
                    height: 8,
                }),
                ..RenderState::default()
            },
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        let changed = output
            .image
            .pixels()
            .filter(|pixel| **pixel != clear)
            .count();
        assert!(changed > 0);
        assert!(changed <= 8 * 8);
    }

    #[test]
    fn color_write_can_be_disabled_while_depth_writes_continue() {
        let clear = Rgb([9, 8, 7]);
        let options = RenderOptions {
            width: 32,
            height: 32,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, -1.0),
            render_state: RenderState {
                clear_color: clear,
                color_write: false,
                ..RenderState::default()
            },
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        assert!(output.image.pixels().all(|pixel| *pixel == clear));
        assert!(output.zbuffer.iter().any(|z| z.is_finite()));
    }

    #[test]
    fn normal_map_mode_uses_supplied_normal_texture() {
        let options = RenderOptions {
            width: 32,
            height: 32,
            mode: RenderMode::NormalMap,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, 1.0),
            normal_map: Some(neutral_normal_map(2)),
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        assert_eq!(output.image.dimensions(), (32, 32));
        assert!(output
            .image
            .pixels()
            .any(|pixel| *pixel != DEFAULT_BACKGROUND));
    }

    #[test]
    fn shadow_options_control_debug_shadow_map_size() {
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Shadow,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, -1.0),
            debug_outputs: true,
            shadow: ShadowOptions {
                size: 32,
                bias: 2.0,
                filter: ShadowFilter::Pcf,
            },
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        assert_eq!(output.debug.shadow.as_ref().unwrap().dimensions(), (32, 32));
    }

    #[test]
    fn debug_outputs_include_depth_normals_shadow_and_ao() {
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Ssao,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, -1.0),
            debug_outputs: true,
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        assert_eq!(output.debug.depth.as_ref().unwrap().dimensions(), (64, 64));
        assert_eq!(
            output.debug.normals.as_ref().unwrap().dimensions(),
            (64, 64)
        );
        assert_eq!(output.debug.shadow.as_ref().unwrap().dimensions(), (64, 64));
        assert_eq!(output.debug.ao.as_ref().unwrap().dimensions(), (64, 64));
    }

    #[test]
    fn perspective_correct_barycentric_weights_by_reciprocal_w() {
        let corrected = perspective_correct_barycentric(Vec3::splat(1.0 / 3.0), [1.0, 0.5, 0.25]);
        assert!(corrected.x > corrected.y);
        assert!(corrected.y > corrected.z);
        assert!((corrected.x + corrected.y + corrected.z - 1.0).abs() < EPSILON);
    }

    #[test]
    fn flat_render_golden_checksum_stays_stable() {
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, -1.0),
            debug_outputs: true,
            ..RenderOptions::default()
        };
        let output = render_model(&tiny_model(), &options);
        assert_eq!(output.image.dimensions(), (64, 64));
        assert_eq!(image_checksum(&output.image), 2_577_244);
        assert!(output.debug.depth.is_some());
        assert!(output.debug.normals.is_some());
    }

    #[test]
    fn teapot_flat_render_matches_golden_image_with_tolerance() {
        let model = Model::load("resources/teapot.obj").unwrap();
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.25, 3.0)),
            light_dir: Vec3::new(1.0, 1.0, -1.0),
            debug_outputs: false,
            ..RenderOptions::default()
        };
        let output = render_model(&model, &options);
        let golden = image::load_from_memory(include_bytes!("../tests/golden/flat-teapot-64.png"))
            .unwrap()
            .to_rgb8();
        assert_images_close(&output.image, &golden, 1);
    }

    fn image_checksum(image: &RgbImage) -> u64 {
        image
            .pixels()
            .map(|p| p[0] as u64 + p[1] as u64 * 3 + p[2] as u64 * 7)
            .sum()
    }

    fn assert_images_close(actual: &RgbImage, expected: &RgbImage, tolerance: u8) {
        assert_eq!(actual.dimensions(), expected.dimensions());
        for (idx, (actual, expected)) in actual.pixels().zip(expected.pixels()).enumerate() {
            for channel in 0..3 {
                let delta = actual[channel].abs_diff(expected[channel]);
                assert!(
                    delta <= tolerance,
                    "pixel {idx} channel {channel} differs by {delta}; actual={actual:?} expected={expected:?}"
                );
            }
        }
    }
}
