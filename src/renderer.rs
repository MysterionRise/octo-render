use glam::{Mat3, Mat4, Vec2, Vec3, Vec4};
use image::{imageops::flip_vertical_in_place, ImageBuffer, Rgb, RgbImage};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const EPSILON: f32 = 1e-6;
const DEFAULT_BACKGROUND: Rgb<u8> = Rgb([18, 20, 24]);

/// Default direction toward the key light, in normalized scene (world) space.
///
/// Light vectors throughout the renderer point from the surface toward the light,
/// so Lambert's term is `max(dot(n, l), 0)` with an outward normal `n`.
pub const DEFAULT_LIGHT_DIR: Vec3 = Vec3::new(1.0, 1.0, 1.0);

/// Fraction of diffuse light kept where the shadow map reports full occlusion.
const SHADOWED_DIFFUSE: f32 = 0.35;

/// Default crease angle, in degrees, for vertex normals generated when an OBJ
/// has no `vn`: faces meeting at a sharper angle keep a hard edge.
pub const DEFAULT_CREASE_ANGLE_DEG: f32 = 60.0;

/// Options for [`Model::load_with_options`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjLoadOptions {
    /// Generate smooth normals for face corners without `vn`, smoothing across
    /// edges whose faces differ by at most this many degrees. `None` keeps the
    /// faceted per-face fallback.
    pub crease_angle_deg: Option<f32>,
}

impl Default for ObjLoadOptions {
    fn default() -> Self {
        ObjLoadOptions {
            crease_angle_deg: Some(DEFAULT_CREASE_ANGLE_DEG),
        }
    }
}

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
    pub materials: Vec<ObjMaterial>,
    pub face_materials: Vec<Option<usize>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjMaterial {
    pub name: String,
    pub material: Material,
    pub diffuse_map: Option<PathBuf>,
    pub normal_map: Option<PathBuf>,
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
    /// Face corners without `vn` get smooth normals (see
    /// [`Model::generate_missing_normals`]) with the default crease angle.
    pub fn load(filename: impl AsRef<Path>) -> Result<Self, String> {
        Self::load_with_options(filename, ObjLoadOptions::default())
    }

    /// Load an OBJ model with explicit loader options.
    pub fn load_with_options(
        filename: impl AsRef<Path>,
        options: ObjLoadOptions,
    ) -> Result<Self, String> {
        let mut model = load_obj(filename.as_ref())?;
        if let Some(crease_angle_deg) = options.crease_angle_deg {
            model.generate_missing_normals(crease_angle_deg);
        }
        Ok(model)
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

    /// Outward geometric normal of a face, assuming counter-clockwise winding
    /// (the OBJ convention). Only a zero-area face falls back to +Z; small but
    /// valid faces keep their true orientation.
    pub fn face_normal(&self, face_idx: usize) -> Vec3 {
        let face = self.faces[face_idx];
        let v0 = self.vertices[face[0].v];
        let v1 = self.vertices[face[1].v];
        let v2 = self.vertices[face[2].v];
        (v1 - v0).cross(v2 - v0).try_normalize().unwrap_or(Vec3::Z)
    }

    pub fn vertex_normal(&self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let fv = self.faces[face_idx][vert_idx];
        fv.vn
            .map(|ni| safe_normal(self.normals[ni], self.face_normal(face_idx)))
            .unwrap_or_else(|| self.face_normal(face_idx))
    }

    /// Generate smooth vertex normals for face corners that have no `vn`.
    ///
    /// Vertices at the same position are welded first, so seams between
    /// duplicated vertices (common in exported meshes) shade smoothly. Each
    /// corner's normal is the angle-weighted average of the face normals around
    /// its vertex, restricted to faces within `crease_angle_deg` of the corner's
    /// own face; sharper edges stay hard. Angle weighting, unlike area
    /// weighting, does not depend on how polygons were triangulated.
    ///
    /// Corners that already have `vn` are left alone, as are degenerate faces,
    /// which keep the per-face fallback in [`Model::vertex_normal`].
    pub fn generate_missing_normals(&mut self, crease_angle_deg: f32) {
        let needs_normals = self
            .faces
            .iter()
            .any(|face| face.iter().any(|fv| fv.vn.is_none()));
        let Some(bounds) = self.bounds() else {
            return;
        };
        if !needs_normals {
            return;
        }

        // Weld positions on a grid far finer than any real feature.
        let diagonal = (bounds.max - bounds.min).length();
        let cell = if diagonal > 0.0 { diagonal * 1e-6 } else { 1.0 };
        let mut weld_ids: HashMap<(i64, i64, i64), usize> = HashMap::new();
        let welded: Vec<usize> = self
            .vertices
            .iter()
            .map(|v| {
                let key = (
                    (v.x / cell).round() as i64,
                    (v.y / cell).round() as i64,
                    (v.z / cell).round() as i64,
                );
                let next_id = weld_ids.len();
                *weld_ids.entry(key).or_insert(next_id)
            })
            .collect();

        // Unit normal per non-degenerate face, plus the faces (with their corner
        // angle) incident to each welded vertex.
        let mut face_normals: Vec<Option<Vec3>> = Vec::with_capacity(self.faces.len());
        let mut incident: Vec<Vec<(usize, f32)>> = vec![Vec::new(); weld_ids.len()];
        for (face_idx, face) in self.faces.iter().enumerate() {
            let p = face.map(|fv| self.vertices[fv.v]);
            // Same degeneracy test as `face_normal`, so the two always agree.
            let Some(normal) = (p[1] - p[0]).cross(p[2] - p[0]).try_normalize() else {
                face_normals.push(None);
                continue;
            };
            let angles: [f32; 3] = std::array::from_fn(|i| {
                let a = (p[(i + 1) % 3] - p[i]).normalize_or_zero();
                let b = (p[(i + 2) % 3] - p[i]).normalize_or_zero();
                a.dot(b).clamp(-1.0, 1.0).acos()
            });
            for (corner, fv) in face.iter().enumerate() {
                incident[welded[fv.v]].push((face_idx, angles[corner]));
            }
            face_normals.push(Some(normal));
        }

        // Each weld-vertex/normal pair is stored once.
        let min_dot = crease_angle_deg.clamp(0.0, 180.0).to_radians().cos();
        let mut generated: HashMap<(usize, [u32; 3]), usize> = HashMap::new();
        for face_idx in 0..self.faces.len() {
            let Some(face_normal) = face_normals[face_idx] else {
                continue;
            };
            for corner in 0..3 {
                if self.faces[face_idx][corner].vn.is_some() {
                    continue;
                }
                let vertex = welded[self.faces[face_idx][corner].v];
                let sum: Vec3 = incident[vertex]
                    .iter()
                    .filter_map(|&(other, angle)| {
                        let other_normal = face_normals[other]?;
                        (other_normal.dot(face_normal) >= min_dot).then_some(other_normal * angle)
                    })
                    .sum();
                let normal = safe_normal(sum, face_normal);
                let key = (vertex, normal.to_array().map(f32::to_bits));
                let index = *generated.entry(key).or_insert_with(|| {
                    self.normals.push(normal);
                    self.normals.len() - 1
                });
                self.faces[face_idx][corner].vn = Some(index);
            }
        }
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
    let mut mtllibs = Vec::new();
    let mut current_material: Option<String> = None;
    let mut face_material_names = Vec::new();

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
            "mtllib" => {
                let mtl = parts.next().ok_or_else(|| {
                    format!(
                        "{}:{}: mtllib requires a file path",
                        path.display(),
                        line_num
                    )
                })?;
                let base = path.parent().unwrap_or_else(|| Path::new(""));
                mtllibs.push(base.join(mtl));
            }
            "usemtl" => {
                let name = parts.next().ok_or_else(|| {
                    format!(
                        "{}:{}: usemtl requires a material name",
                        path.display(),
                        line_num
                    )
                })?;
                current_material = Some(name.to_string());
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
                    face_material_names.push(current_material.clone());
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

    let materials = load_mtl_libraries(&mtllibs)?;
    let material_indices = materials
        .iter()
        .enumerate()
        .map(|(idx, material)| (material.name.clone(), idx))
        .collect::<HashMap<_, _>>();
    let face_materials = face_material_names
        .into_iter()
        .map(|name| name.and_then(|name| material_indices.get(&name).copied()))
        .collect();

    Ok(Model {
        vertices,
        texcoords,
        normals,
        faces,
        materials,
        face_materials,
    })
}

fn load_mtl_libraries(paths: &[PathBuf]) -> Result<Vec<ObjMaterial>, String> {
    let mut materials = Vec::new();
    for path in paths {
        materials.extend(load_mtl(path)?);
    }
    Ok(materials)
}

fn load_mtl(path: &Path) -> Result<Vec<ObjMaterial>, String> {
    let file = File::open(path)
        .map_err(|err| format!("failed to open MTL '{}': {}", path.display(), err))?;
    let reader = BufReader::new(file);
    let mut materials = Vec::new();
    let mut current: Option<ObjMaterial> = None;
    let base = path.parent().unwrap_or_else(|| Path::new(""));

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
            "newmtl" => {
                if let Some(material) = current.take() {
                    materials.push(material);
                }
                let name = parts.next().ok_or_else(|| {
                    format!(
                        "{}:{}: newmtl requires a material name",
                        path.display(),
                        line_num
                    )
                })?;
                current = Some(ObjMaterial {
                    name: name.to_string(),
                    material: Material::default(),
                    diffuse_map: None,
                    normal_map: None,
                });
            }
            "Kd" => {
                let rgb = parse_mtl_rgb(&mut parts, path, line_num, "Kd")?;
                if let Some(material) = &mut current {
                    material.material.base_color = rgb;
                }
            }
            "Ka" => {
                let ambient = parse_mtl_scalar_or_rgb(&mut parts, path, line_num, "Ka")?;
                if let Some(material) = &mut current {
                    material.material.ambient = ambient;
                }
            }
            "Ks" => {
                let specular = parse_mtl_scalar_or_rgb(&mut parts, path, line_num, "Ks")?;
                if let Some(material) = &mut current {
                    material.material.specular = specular;
                }
            }
            "Ns" => {
                let shininess = parse_f32(parts.next(), path, line_num, "Ns")?;
                if let Some(material) = &mut current {
                    material.material.shininess = shininess.max(1.0);
                }
            }
            "d" => {
                let alpha = parse_f32(parts.next(), path, line_num, "d")?;
                if let Some(material) = &mut current {
                    material.material.alpha = alpha.clamp(0.0, 1.0);
                }
            }
            "Tr" => {
                let transparency = parse_f32(parts.next(), path, line_num, "Tr")?;
                if let Some(material) = &mut current {
                    material.material.alpha = (1.0 - transparency).clamp(0.0, 1.0);
                }
            }
            "map_Kd" => {
                if let Some(material) = &mut current {
                    material.diffuse_map = Some(base.join(parse_texture_map_path(parts)));
                }
            }
            "map_Bump" | "map_bump" | "bump" | "norm" | "map_Kn" => {
                if let Some(material) = &mut current {
                    material.normal_map = Some(base.join(parse_texture_map_path(parts)));
                }
            }
            _ => {}
        }
    }

    if let Some(material) = current {
        materials.push(material);
    }

    Ok(materials)
}

fn parse_mtl_rgb<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    path: &Path,
    line_num: usize,
    label: &str,
) -> Result<Rgb<u8>, String> {
    let r = parse_f32(parts.next(), path, line_num, label)?;
    let g = parse_f32(parts.next(), path, line_num, label)?;
    let b = parse_f32(parts.next(), path, line_num, label)?;
    Ok(Rgb([
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]))
}

fn parse_mtl_scalar_or_rgb<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    path: &Path,
    line_num: usize,
    label: &str,
) -> Result<f32, String> {
    let first = parse_f32(parts.next(), path, line_num, label)?;
    let rest = parts
        .map(|part| {
            part.parse::<f32>().map_err(|err| {
                format!(
                    "{}:{}: invalid {} value '{}': {}",
                    path.display(),
                    line_num,
                    label,
                    part,
                    err
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if rest.is_empty() {
        Ok(first.max(0.0))
    } else {
        let sum = first + rest.iter().copied().sum::<f32>();
        Ok((sum / (rest.len() as f32 + 1.0)).max(0.0))
    }
}

fn parse_texture_map_path<'a>(parts: impl Iterator<Item = &'a str>) -> PathBuf {
    let tokens = parts.collect::<Vec<_>>();
    let path = tokens
        .iter()
        .rev()
        .find(|token| !token.starts_with('-'))
        .copied()
        .unwrap_or("");
    PathBuf::from(path)
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

fn write_fragment_checked(
    image: &mut RgbImage,
    x: i32,
    y: i32,
    fragment: Fragment,
    render_state: RenderState,
) {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        return;
    }
    let dst = *image.get_pixel(x as u32, y as u32);
    *image.get_pixel_mut(x as u32, y as u32) =
        blend_fragment(dst, fragment, render_state.blend_mode);
}

pub fn blend_fragment(dst: Rgb<u8>, src: Fragment, blend_mode: BlendMode) -> Rgb<u8> {
    match blend_mode {
        BlendMode::Off => src.colour,
        BlendMode::Alpha => {
            let a = src.alpha.clamp(0.0, 1.0);
            Rgb([
                (src.colour[0] as f32 * a + dst[0] as f32 * (1.0 - a)).round() as u8,
                (src.colour[1] as f32 * a + dst[1] as f32 * (1.0 - a)).round() as u8,
                (src.colour[2] as f32 * a + dst[2] as f32 * (1.0 - a)).round() as u8,
            ])
        }
        BlendMode::Additive => {
            let a = src.alpha.clamp(0.0, 1.0);
            Rgb([
                (dst[0] as f32 + src.colour[0] as f32 * a).clamp(0.0, 255.0) as u8,
                (dst[1] as f32 + src.colour[1] as f32 * a).clamp(0.0, 255.0) as u8,
                (dst[2] as f32 + src.colour[2] as f32 * a).clamp(0.0, 255.0) as u8,
            ])
        }
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
    let Some(coverage) = TriangleCoverage::new(v0, v1, v2) else {
        return;
    };

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if coverage.barycentric(p).is_some() {
                put_pixel_checked(image, x, y, colour);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TriangleCoverage {
    a: Vec2,
    b: Vec2,
    c: Vec2,
    sign: f32,
    inv_area: f32,
    top_left: [bool; 3],
}

impl TriangleCoverage {
    fn new(a: Vec2, b: Vec2, c: Vec2) -> Option<Self> {
        let area = edge_function(a, b, c);
        if area.abs() < EPSILON {
            return None;
        }
        Some(Self {
            a,
            b,
            c,
            sign: if area > 0.0 { 1.0 } else { -1.0 },
            inv_area: 1.0 / area,
            top_left: [
                is_top_left_edge(b, c),
                is_top_left_edge(c, a),
                is_top_left_edge(a, b),
            ],
        })
    }

    fn barycentric(&self, p: Vec2) -> Option<Vec3> {
        let e0 = edge_function(self.b, self.c, p);
        let e1 = edge_function(self.c, self.a, p);
        let e2 = edge_function(self.a, self.b, p);
        if edge_is_inside(e0 * self.sign, self.top_left[0])
            && edge_is_inside(e1 * self.sign, self.top_left[1])
            && edge_is_inside(e2 * self.sign, self.top_left[2])
        {
            Some(Vec3::new(
                e0 * self.inv_area,
                e1 * self.inv_area,
                e2 * self.inv_area,
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
fn point_in_triangle_top_left(a: Vec2, b: Vec2, c: Vec2, p: Vec2) -> bool {
    TriangleCoverage::new(a, b, c)
        .and_then(|coverage| coverage.barycentric(p))
        .is_some()
}

fn edge_function(a: Vec2, b: Vec2, p: Vec2) -> f32 {
    (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x)
}

fn is_top_left_edge(a: Vec2, b: Vec2) -> bool {
    let edge = b - a;
    edge.y > 0.0 || (edge.y.abs() < EPSILON && edge.x < 0.0)
}

fn edge_is_inside(value: f32, top_left: bool) -> bool {
    value > EPSILON || (top_left && value.abs() <= EPSILON)
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fragment {
    pub colour: Rgb<u8>,
    pub alpha: f32,
}

impl Fragment {
    pub fn new(colour: Rgb<u8>, alpha: f32) -> Self {
        Fragment {
            colour,
            alpha: alpha.clamp(0.0, 1.0),
        }
    }

    pub fn opaque(colour: Rgb<u8>) -> Self {
        Fragment { colour, alpha: 1.0 }
    }
}

/// Shader trait: vertex shader transforms vertices, fragment shader computes pixel colors.
pub trait Shader {
    /// Process vertex `vert_idx` (0..3) of face `face_idx`. Returns screen-space position.
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3;
    /// Compute fragment color given barycentric coordinates. Return None to discard.
    fn fragment(&self, bc: Vec3) -> Option<Fragment>;
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
    let Some(coverage) = TriangleCoverage::new(v0.truncate(), v1.truncate(), v2.truncate()) else {
        return;
    };

    let Some((min_x, max_x, min_y, max_y)) =
        raster_bounds(screen_coords, width, height, render_state.scissor)
    else {
        return;
    };

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = coverage.barycentric(p) {
                let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                let idx = x as usize + y as usize * width as usize;
                if depth_passes(z, zbuffer[idx], render_state.depth) {
                    let source_bc = source_barycentric[0] * bc.x
                        + source_barycentric[1] * bc.y
                        + source_barycentric[2] * bc.z;
                    let corrected = perspective_correct_barycentric(source_bc, reciprocal_w);
                    if let Some(fragment) = shader.fragment(corrected) {
                        if render_state.depth.write_enabled {
                            zbuffer[idx] = z;
                        }
                        if render_state.color_write {
                            write_fragment_checked(image, x, y, fragment, render_state);
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
    triangle_zbuf_with_render_state(v0, v1, v2, zbuffer, image, Fragment::opaque(colour), state);
}

fn triangle_zbuf_with_render_state(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    zbuffer: &mut [f32],
    image: &mut RgbImage,
    fragment: Fragment,
    render_state: RenderState,
) {
    let (width, height) = image.dimensions();
    let coords = [v0, v1, v2];
    let Some(coverage) = TriangleCoverage::new(v0.truncate(), v1.truncate(), v2.truncate()) else {
        return;
    };
    let Some((min_x, max_x, min_y, max_y)) =
        raster_bounds(&coords, width, height, render_state.scissor)
    else {
        return;
    };

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(bc) = coverage.barycentric(p) {
                let z = bc.x * v0.z + bc.y * v1.z + bc.z * v2.z;
                let idx = x as usize + y as usize * width as usize;
                if depth_passes(z, zbuffer[idx], render_state.depth) {
                    if render_state.depth.write_enabled {
                        zbuffer[idx] = z;
                    }
                    if render_state.color_write {
                        write_fragment_checked(image, x, y, fragment, render_state);
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
///
/// `light_dir` is the eye-space direction toward the light. `normal_matrix` is
/// derived from `model_view` in [`GouraudShader::new`].
pub struct GouraudShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub normal_matrix: Mat4,
    pub light_dir: Vec3,
    pub material: Material,
    pub varying_intensity: [f32; 3],
    pub varying_specular: [f32; 3],
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
            normal_matrix: normal_matrix(model_view),
            light_dir: safe_normal(light_dir, DEFAULT_LIGHT_DIR.normalize()),
            material,
            varying_intensity: [0.0; 3],
            varying_specular: [0.0; 3],
        }
    }
}

impl Shader for GouraudShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        let normal = self.model.vertex_normal(face_idx, vert_idx);

        let n = safe_normal(transform_vector(&self.normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);
        self.varying_specular[vert_idx] = specular_highlight(
            &self.material,
            n,
            self.light_dir,
            transform_point(&self.model_view, v),
        );

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Fragment> {
        let intensity = interpolate_scalar(self.varying_intensity, bc);
        let specular = interpolate_scalar(self.varying_specular, bc);
        // Unlit fragments still shade (to ambient or black) so they keep writing
        // depth and occluding geometry behind them.
        let colour = shade_material(self.material.base_color, self.material, intensity, specular);
        Some(Fragment::new(colour, self.material.alpha))
    }
}

/// Texture-mapped shader with Gouraud lighting and optional diffuse texture.
pub struct TextureShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub normal_matrix: Mat4,
    pub light_dir: Vec3,
    pub diffuse: Option<&'a RgbImage>,
    pub diffuse_mipmaps: Option<&'a [RgbImage]>,
    pub sampler: TextureSampler,
    pub material: Material,
    pub varying_intensity: [f32; 3],
    pub varying_specular: [f32; 3],
    pub varying_uv: [Vec2; 3],
    pub varying_screen_pos: [Vec2; 3],
}

impl<'a> TextureShader<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: &'a Model,
        transform: Mat4,
        model_view: Mat4,
        light_dir: Vec3,
        diffuse: Option<&'a RgbImage>,
        diffuse_mipmaps: Option<&'a [RgbImage]>,
        sampler: TextureSampler,
        material: Material,
    ) -> Self {
        TextureShader {
            model,
            transform,
            model_view,
            normal_matrix: normal_matrix(model_view),
            light_dir: safe_normal(light_dir, DEFAULT_LIGHT_DIR.normalize()),
            diffuse,
            diffuse_mipmaps,
            sampler,
            material,
            varying_intensity: [0.0; 3],
            varying_specular: [0.0; 3],
            varying_uv: [Vec2::ZERO; 3],
            varying_screen_pos: [Vec2::ZERO; 3],
        }
    }
}

impl Shader for TextureShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];

        self.varying_uv[vert_idx] = face_vertex_uv(self.model, face[vert_idx]);

        let normal = self.model.vertex_normal(face_idx, vert_idx);
        let n = safe_normal(transform_vector(&self.normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);
        self.varying_specular[vert_idx] = specular_highlight(
            &self.material,
            n,
            self.light_dir,
            transform_point(&self.model_view, v),
        );

        let screen = transform_point(&self.transform, v);
        self.varying_screen_pos[vert_idx] = screen.truncate();
        screen
    }

    fn fragment(&self, bc: Vec3) -> Option<Fragment> {
        let intensity = interpolate_scalar(self.varying_intensity, bc);
        let specular = interpolate_scalar(self.varying_specular, bc);

        let base = if let Some(tex) = self.diffuse {
            let uv =
                self.varying_uv[0] * bc.x + self.varying_uv[1] * bc.y + self.varying_uv[2] * bc.z;
            tint_rgb(
                sample_texture_with_mipmaps(
                    tex,
                    self.diffuse_mipmaps,
                    uv,
                    self.sampler,
                    triangle_lod(
                        self.varying_uv,
                        self.varying_screen_pos,
                        tex.width(),
                        tex.height(),
                    ),
                ),
                self.material.base_color,
            )
        } else {
            self.material.base_color
        };
        Some(Fragment::new(
            shade_material(base, self.material, intensity, specular),
            self.material.alpha,
        ))
    }
}

/// Normal mapping shader: uses tangent-space normal map for per-pixel lighting.
pub struct NormalMapShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub normal_matrix: Mat4,
    pub light_dir: Vec3,
    pub diffuse: Option<&'a RgbImage>,
    pub normal_map: &'a RgbImage,
    pub diffuse_mipmaps: Option<&'a [RgbImage]>,
    pub normal_mipmaps: Option<&'a [RgbImage]>,
    pub sampler: TextureSampler,
    pub material: Material,
    pub varying_uv: [Vec2; 3],
    pub varying_pos: [Vec3; 3],
    pub varying_screen_pos: [Vec2; 3],
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
        diffuse_mipmaps: Option<&'a [RgbImage]>,
        normal_mipmaps: Option<&'a [RgbImage]>,
        sampler: TextureSampler,
        material: Material,
    ) -> Self {
        NormalMapShader {
            model,
            transform,
            model_view,
            normal_matrix: normal_matrix(model_view),
            light_dir: safe_normal(light_dir, DEFAULT_LIGHT_DIR.normalize()),
            diffuse,
            normal_map,
            diffuse_mipmaps,
            normal_mipmaps,
            sampler,
            material,
            varying_uv: [Vec2::ZERO; 3],
            varying_pos: [Vec3::ZERO; 3],
            varying_screen_pos: [Vec2::ZERO; 3],
        }
    }
}

impl Shader for NormalMapShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        self.varying_pos[vert_idx] = v;
        self.varying_uv[vert_idx] = face_vertex_uv(self.model, face[vert_idx]);

        let screen = transform_point(&self.transform, v);
        self.varying_screen_pos[vert_idx] = screen.truncate();
        screen
    }

    fn fragment(&self, bc: Vec3) -> Option<Fragment> {
        let uv = self.varying_uv[0] * bc.x + self.varying_uv[1] * bc.y + self.varying_uv[2] * bc.z;

        let normal_lod = triangle_lod(
            self.varying_uv,
            self.varying_screen_pos,
            self.normal_map.width(),
            self.normal_map.height(),
        );
        let np = sample_texture_with_mipmaps(
            self.normal_map,
            self.normal_mipmaps,
            uv,
            self.sampler,
            normal_lod,
        );
        let tangent_normal = Vec3::new(
            np[0] as f32 / 255.0 * 2.0 - 1.0,
            np[1] as f32 / 255.0 * 2.0 - 1.0,
            np[2] as f32 / 255.0 * 2.0 - 1.0,
        );

        let dp1 = self.varying_pos[1] - self.varying_pos[0];
        let dp2 = self.varying_pos[2] - self.varying_pos[0];
        let duv1 = self.varying_uv[1] - self.varying_uv[0];
        let duv2 = self.varying_uv[2] - self.varying_uv[0];
        let face_normal = dp1.cross(dp2).try_normalize().unwrap_or(Vec3::Z);
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

        let n = safe_normal(transform_vector(&self.normal_matrix, world_normal), Vec3::Z);
        let intensity = n.dot(self.light_dir).max(0.0);
        let model_pos =
            self.varying_pos[0] * bc.x + self.varying_pos[1] * bc.y + self.varying_pos[2] * bc.z;
        let specular = specular_highlight(
            &self.material,
            n,
            self.light_dir,
            transform_point(&self.model_view, model_pos),
        );

        let base = self
            .diffuse
            .map(|tex| {
                tint_rgb(
                    sample_texture_with_mipmaps(
                        tex,
                        self.diffuse_mipmaps,
                        uv,
                        self.sampler,
                        triangle_lod(
                            self.varying_uv,
                            self.varying_screen_pos,
                            tex.width(),
                            tex.height(),
                        ),
                    ),
                    self.material.base_color,
                )
            })
            .unwrap_or(self.material.base_color);
        Some(Fragment::new(
            shade_material(base, self.material, intensity, specular),
            self.material.alpha,
        ))
    }
}

/// Normal visualization shader for debug output and no-texture assets.
pub struct NormalDebugShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub normal_matrix: Mat4,
    pub varying_normal: [Vec3; 3],
}

impl<'a> NormalDebugShader<'a> {
    pub fn new(model: &'a Model, transform: Mat4, model_view: Mat4) -> Self {
        NormalDebugShader {
            model,
            transform,
            model_view,
            normal_matrix: normal_matrix(model_view),
            varying_normal: [Vec3::Z; 3],
        }
    }
}

impl Shader for NormalDebugShader<'_> {
    fn vertex(&mut self, face_idx: usize, vert_idx: usize) -> Vec3 {
        let face = &self.model.faces[face_idx];
        let v = self.model.vertices[face[vert_idx].v];
        let normal = self.model.vertex_normal(face_idx, vert_idx);
        self.varying_normal[vert_idx] =
            safe_normal(transform_vector(&self.normal_matrix, normal), Vec3::Z);
        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Fragment> {
        let normal = safe_normal(
            self.varying_normal[0] * bc.x
                + self.varying_normal[1] * bc.y
                + self.varying_normal[2] * bc.z,
            Vec3::Z,
        );
        Some(Fragment::opaque(Rgb([
            ((normal.x * 0.5 + 0.5) * 255.0) as u8,
            ((normal.y * 0.5 + 0.5) * 255.0) as u8,
            ((normal.z * 0.5 + 0.5) * 255.0) as u8,
        ])))
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

    fn fragment(&self, _bc: Vec3) -> Option<Fragment> {
        Some(Fragment::opaque(Rgb([255, 255, 255])))
    }
}

/// Shadow-mapped Gouraud shader: combines Gouraud shading with shadow map lookup.
pub struct ShadowShader<'a> {
    pub model: &'a Model,
    pub transform: Mat4,
    pub model_view: Mat4,
    pub normal_matrix: Mat4,
    pub shadow_transform: Mat4,
    pub light_dir: Vec3,
    pub shadow_buffer: &'a [f32],
    pub shadow_width: u32,
    pub shadow_height: u32,
    pub shadow_options: ShadowOptions,
    pub material: Material,
    pub varying_intensity: [f32; 3],
    pub varying_specular: [f32; 3],
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
            normal_matrix: normal_matrix(model_view),
            shadow_transform,
            light_dir: safe_normal(light_dir, DEFAULT_LIGHT_DIR.normalize()),
            shadow_buffer,
            shadow_width,
            shadow_height,
            shadow_options,
            material,
            varying_intensity: [0.0; 3],
            varying_specular: [0.0; 3],
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
        let n = safe_normal(transform_vector(&self.normal_matrix, normal), Vec3::Z);
        self.varying_intensity[vert_idx] = n.dot(self.light_dir).max(0.0);
        self.varying_specular[vert_idx] = specular_highlight(
            &self.material,
            n,
            self.light_dir,
            transform_point(&self.model_view, v),
        );

        transform_point(&self.transform, v)
    }

    fn fragment(&self, bc: Vec3) -> Option<Fragment> {
        let intensity = interpolate_scalar(self.varying_intensity, bc);
        let specular = interpolate_scalar(self.varying_specular, bc);

        let world_pos = self.varying_world_pos[0] * bc.x
            + self.varying_world_pos[1] * bc.y
            + self.varying_world_pos[2] * bc.z;
        let shadow_coord = transform_point(&self.shadow_transform, world_pos);

        let visibility = sample_shadow(
            self.shadow_buffer,
            self.shadow_width,
            self.shadow_height,
            shadow_coord,
            self.shadow_options,
        );
        let diffuse_light = SHADOWED_DIFFUSE + (1.0 - SHADOWED_DIFFUSE) * visibility;

        Some(Fragment::new(
            shade_material(
                self.material.base_color,
                self.material,
                intensity * diffuse_light,
                // Occluded points see no highlight from the shadowing light.
                specular * visibility,
            ),
            self.material.alpha,
        ))
    }
}

/// Light visibility at `shadow_coord`: 1.0 fully lit, 0.0 fully occluded.
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
        0.0
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
    Mipmap,
}

impl TextureFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            TextureFilter::Nearest => "nearest",
            TextureFilter::Linear => "linear",
            TextureFilter::Mipmap => "mipmap",
        }
    }
}

impl FromStr for TextureFilter {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nearest" => Ok(TextureFilter::Nearest),
            "linear" => Ok(TextureFilter::Linear),
            // Not "trilinear": the sampler picks one mip level and filters it
            // bilinearly; it does not blend between levels.
            "mipmap" | "mip" => Ok(TextureFilter::Mipmap),
            _ => Err(format!(
                "unknown texture filter '{}'; expected one of: nearest, linear, mipmap",
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
    pub alpha: f32,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            base_color: Rgb([220, 220, 220]),
            ambient: 0.0,
            diffuse: 1.0,
            specular: 0.0,
            shininess: 32.0,
            alpha: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Off,
    Alpha,
    Additive,
}

impl BlendMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Off => "off",
            BlendMode::Alpha => "alpha",
            BlendMode::Additive => "additive",
        }
    }
}

impl FromStr for BlendMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(BlendMode::Off),
            "alpha" => Ok(BlendMode::Alpha),
            "additive" | "add" => Ok(BlendMode::Additive),
            _ => Err(format!(
                "unknown blend mode '{}'; expected one of: off, alpha, additive",
                value
            )),
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
    pub blend_mode: BlendMode,
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
            blend_mode: BlendMode::Off,
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
    /// Direction toward the light in normalized scene space. Shading and the
    /// shadow map both derive their light from this one vector.
    pub light_dir: Vec3,
    pub debug_outputs: bool,
    pub render_state: RenderState,
    pub texture: Option<RgbImage>,
    pub texture_mipmaps: Vec<RgbImage>,
    pub normal_map: Option<RgbImage>,
    pub normal_map_mipmaps: Vec<RgbImage>,
    pub texture_sampler: TextureSampler,
    pub material: Material,
    pub shadow: ShadowOptions,
    pub near_plane: f32,
    pub far_plane: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 1024,
            height: 1024,
            mode: RenderMode::Shadow,
            camera_eye: None,
            light_dir: DEFAULT_LIGHT_DIR.normalize(),
            debug_outputs: false,
            render_state: RenderState::default(),
            texture: None,
            texture_mipmaps: Vec::new(),
            normal_map: None,
            normal_map_mipmaps: Vec::new(),
            texture_sampler: TextureSampler::default(),
            material: Material::default(),
            shadow: ShadowOptions::default(),
            near_plane: 0.01,
            far_plane: 1000.0,
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
    view_to_clip: Mat4,
    clip_to_screen: Mat4,
    light_transform: Mat4,
    light_model_view: Mat4,
    light_view_to_clip: Mat4,
    light_clip_to_screen: Mat4,
    /// Eye-space direction toward the light, used by every lit shader.
    light_dir_view: Vec3,
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
            RenderMode::Flat => render_flat(model, &transforms, options, &mut zbuffer, &mut image),
            RenderMode::Gouraud => {
                let mut shader = GouraudShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    transforms.light_dir_view,
                    options.material,
                );
                render_shader(
                    model,
                    &transforms,
                    &mut shader,
                    &mut zbuffer,
                    &mut image,
                    options,
                    options.render_state,
                );
            }
            RenderMode::Texture => {
                let checker = checker_texture(128);
                let diffuse = options
                    .texture
                    .as_ref()
                    .or_else(|| (!model.texcoords.is_empty()).then_some(&checker));
                let generated_mipmaps = if options.texture_sampler.filter == TextureFilter::Mipmap
                    && options.texture_mipmaps.is_empty()
                {
                    diffuse.map(build_mipmaps)
                } else {
                    None
                };
                let diffuse_mipmaps = if !options.texture_mipmaps.is_empty() {
                    Some(options.texture_mipmaps.as_slice())
                } else {
                    generated_mipmaps.as_deref()
                };
                let mut shader = TextureShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    transforms.light_dir_view,
                    diffuse,
                    diffuse_mipmaps,
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
                    options.render_state,
                );
            }
            RenderMode::NormalMap => {
                let neutral_normal_map = neutral_normal_map(2);
                let normal_map = options.normal_map.as_ref().unwrap_or(&neutral_normal_map);
                let diffuse = options.texture.as_ref();
                let generated_diffuse_mipmaps = if options.texture_sampler.filter
                    == TextureFilter::Mipmap
                    && options.texture_mipmaps.is_empty()
                {
                    diffuse.map(build_mipmaps)
                } else {
                    None
                };
                let generated_normal_mipmaps = if options.texture_sampler.filter
                    == TextureFilter::Mipmap
                    && options.normal_map_mipmaps.is_empty()
                {
                    Some(build_mipmaps(normal_map))
                } else {
                    None
                };
                let diffuse_mipmaps = if !options.texture_mipmaps.is_empty() {
                    Some(options.texture_mipmaps.as_slice())
                } else {
                    generated_diffuse_mipmaps.as_deref()
                };
                let normal_mipmaps = if !options.normal_map_mipmaps.is_empty() {
                    Some(options.normal_map_mipmaps.as_slice())
                } else {
                    generated_normal_mipmaps.as_deref()
                };
                let mut shader = NormalMapShader::new(
                    model,
                    transforms.transform,
                    transforms.model_view,
                    transforms.light_dir_view,
                    diffuse,
                    normal_map,
                    diffuse_mipmaps,
                    normal_mipmaps,
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
                    options.render_state,
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
                    options.render_state,
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
                    transforms.light_dir_view,
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
                    options.render_state,
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
    let view_to_clip = proj;
    let clip_to_screen = vp;
    let transform = clip_to_screen * view_to_clip * view * model_matrix;
    let model_view = view * model_matrix;

    let light_dir = safe_normal(options.light_dir, DEFAULT_LIGHT_DIR.normalize());
    let light_dir_view = safe_normal(transform_vector(&view, light_dir), Vec3::Z);
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
    let light_view_to_clip = light_proj;
    let light_clip_to_screen = shadow_vp;
    let light_transform = light_clip_to_screen * light_view_to_clip * light_view * model_matrix;
    let light_model_view = light_view * model_matrix;
    RenderTransforms {
        transform,
        model_view,
        view_to_clip,
        clip_to_screen,
        light_transform,
        light_model_view,
        light_view_to_clip,
        light_clip_to_screen,
        light_dir_view,
    }
}

fn render_wireframe(
    model: &Model,
    transforms: &RenderTransforms,
    options: &RenderOptions,
    image: &mut RgbImage,
) {
    let colour = Rgb([235, 238, 245]);
    let mut primitives = Vec::new();
    for face_idx in 0..model.faces.len() {
        if !prepare_face(
            model,
            transforms,
            face_idx,
            options.near_plane,
            options.far_plane,
            options.render_state,
            &mut primitives,
        ) {
            continue;
        }
        for primitive in &primitives {
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
    zbuffer: &mut [f32],
    image: &mut RgbImage,
) {
    let normal_matrix = normal_matrix(transforms.model_view);
    let light_dir = transforms.light_dir_view;
    let mut primitives = Vec::new();
    for face_idx in 0..model.faces.len() {
        // Cull and clip before lighting, so culled faces cost no shading work.
        if !prepare_face(
            model,
            transforms,
            face_idx,
            options.near_plane,
            options.far_plane,
            options.render_state,
            &mut primitives,
        ) {
            continue;
        }
        let normal = safe_normal(
            transform_vector(&normal_matrix, model.face_normal(face_idx)),
            Vec3::Z,
        );
        let intensity = normal.dot(light_dir).max(0.0);
        let specular = if options.material.specular > 0.0 {
            let face = model.faces[face_idx];
            let centroid =
                (model.vertices[face[0].v] + model.vertices[face[1].v] + model.vertices[face[2].v])
                    / 3.0;
            specular_highlight(
                &options.material,
                normal,
                light_dir,
                transform_point(&transforms.model_view, centroid),
            )
        } else {
            0.0
        };
        // Unlit faces still shade (to ambient or black) and write depth.
        let shaded = shade_material(
            options.material.base_color,
            options.material,
            intensity,
            specular,
        );
        for primitive in &primitives {
            triangle_zbuf_with_render_state(
                primitive.screen_coords[0],
                primitive.screen_coords[1],
                primitive.screen_coords[2],
                zbuffer,
                image,
                Fragment::new(shaded, options.material.alpha),
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
    render_state: RenderState,
) {
    let mut primitives = Vec::new();
    for face_idx in 0..model.faces.len() {
        // Cull and clip first: culled or fully clipped faces skip vertex shading.
        // Shaders keep no state across faces, so skipping one is safe.
        if !prepare_face(
            model,
            transforms,
            face_idx,
            options.near_plane,
            options.far_plane,
            render_state,
            &mut primitives,
        ) {
            continue;
        }
        let _: [Vec3; 3] = std::array::from_fn(|j| shader.vertex(face_idx, j));
        let reciprocal_w = reciprocal_w_for_face(model, transforms.transform, face_idx);
        for primitive in &primitives {
            triangle_shader_perspective_with_render_state(
                &primitive.screen_coords,
                primitive.source_barycentric,
                reciprocal_w,
                shader,
                zbuffer,
                image,
                render_state,
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

/// Screen-space winding test, kept as the reference the clip-space classifier
/// ([`should_cull_winding`]) is checked against.
#[cfg(test)]
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
    clip_pos: Vec4,
    source_bc: Vec3,
}

#[derive(Clone, Copy, Debug)]
struct ClippedPrimitive {
    screen_coords: [Vec3; 3],
    source_barycentric: [Vec3; 3],
}

/// Cull a face, then clip it against the frustum and project the surviving
/// pieces into `out` (cleared first). Returns whether anything is left to
/// rasterize.
///
/// Culling runs before clipping, so culled faces never pay for the clipper. The
/// winding comes from the homogeneous determinant ([`homogeneous_winding`]),
/// which matches screen-space winding whenever every vertex is in front of the
/// centre of projection, and stays meaningful for triangles that cross it,
/// where screen-space winding is undefined.
fn prepare_face(
    model: &Model,
    transforms: &RenderTransforms,
    face_idx: usize,
    near_plane: f32,
    far_plane: f32,
    render_state: RenderState,
    out: &mut Vec<ClippedPrimitive>,
) -> bool {
    out.clear();
    let triangle = face_clip_vertices(model, transforms, face_idx);
    if should_cull_winding(
        homogeneous_winding(&triangle),
        render_state.cull_mode,
        render_state.front_face,
    ) {
        return false;
    }
    clip_triangle_into(triangle, transforms, near_plane, far_plane, out);
    !out.is_empty()
}

fn face_clip_vertices(
    model: &Model,
    transforms: &RenderTransforms,
    face_idx: usize,
) -> [ClippedVertex; 3] {
    let face = model.faces[face_idx];
    [
        clipped_vertex_for_source(model.vertices[face[0].v], transforms, Vec3::X),
        clipped_vertex_for_source(model.vertices[face[1].v], transforms, Vec3::Y),
        clipped_vertex_for_source(model.vertices[face[2].v], transforms, Vec3::Z),
    ]
}

/// Signed orientation of a triangle as seen from the centre of projection:
/// the determinant of its clip-space `(x, y, w)` corners, which equals
/// `w0 * w1 * w2` times twice its signed NDC area.
fn homogeneous_winding(triangle: &[ClippedVertex; 3]) -> f32 {
    let column = |v: &ClippedVertex| Vec3::new(v.clip_pos.x, v.clip_pos.y, v.clip_pos.w);
    Mat3::from_cols(
        column(&triangle[0]),
        column(&triangle[1]),
        column(&triangle[2]),
    )
    .determinant()
}

/// A positive winding is counter-clockwise in the final image: the viewport
/// keeps axis directions, and the vertical flip happens after rasterization.
/// Zero winding counts as not front-facing, like the screen-space test.
fn should_cull_winding(winding: f32, cull_mode: CullMode, front_face: FrontFace) -> bool {
    let front_facing = match front_face {
        FrontFace::Ccw => winding > 0.0,
        FrontFace::Cw => winding < 0.0,
    };
    match cull_mode {
        CullMode::Off => false,
        CullMode::Front => front_facing,
        CullMode::Back => !front_facing,
    }
}

/// Clip a triangle against the frustum and append its projected pieces.
///
/// A triangle inside every plane skips the clipper, which would return it
/// unchanged: each fully-inside pass rotates the vertex list by one, and six
/// passes restore the original order. A triangle entirely outside one plane is
/// dropped without clipping.
fn clip_triangle_into(
    triangle: [ClippedVertex; 3],
    transforms: &RenderTransforms,
    near_plane: f32,
    far_plane: f32,
    out: &mut Vec<ClippedPrimitive>,
) {
    let mut fully_inside = true;
    for plane in FrustumPlane::ALL {
        let inside = triangle
            .map(|vertex| frustum_plane_distance(vertex, plane, near_plane, far_plane) >= -EPSILON);
        if inside == [false; 3] {
            return;
        }
        fully_inside &= inside == [true; 3];
    }
    if fully_inside {
        out.push(project_clipped_triangle(&triangle, transforms));
        return;
    }
    out.extend(
        clip_varying_triangle_against_frustum(triangle, near_plane, far_plane)
            .iter()
            .map(|piece| project_clipped_triangle(piece, transforms)),
    );
}

fn project_clipped_triangle(
    triangle: &[ClippedVertex; 3],
    transforms: &RenderTransforms,
) -> ClippedPrimitive {
    ClippedPrimitive {
        screen_coords: std::array::from_fn(|i| {
            clip_position_to_screen(triangle[i].clip_pos, transforms.clip_to_screen)
        }),
        source_barycentric: std::array::from_fn(|i| triangle[i].source_bc),
    }
}

fn clipped_vertex_for_source(
    model_pos: Vec3,
    transforms: &RenderTransforms,
    source_bc: Vec3,
) -> ClippedVertex {
    let view_pos = transform_point(&transforms.model_view, model_pos);
    let clip_pos = transform_homogeneous(&transforms.view_to_clip, view_pos);
    ClippedVertex {
        view_pos,
        clip_pos,
        source_bc,
    }
}

fn clip_position_to_screen(clip_pos: Vec4, clip_to_screen: Mat4) -> Vec3 {
    let w = if clip_pos.w.abs() > EPSILON {
        clip_pos.w
    } else {
        EPSILON
    };
    transform_point(
        &clip_to_screen,
        Vec3::new(clip_pos.x / w, clip_pos.y / w, clip_pos.z / w),
    )
}

fn clip_varying_triangle_against_frustum(
    triangle: [ClippedVertex; 3],
    near: f32,
    far: f32,
) -> Vec<[ClippedVertex; 3]> {
    let mut polygon = triangle.to_vec();
    for plane in FrustumPlane::ALL {
        polygon = clip_polygon_against_frustum_plane(&polygon, plane, near, far);
        if polygon.len() < 3 {
            return Vec::new();
        }
    }
    triangulate_clipped_polygon(&polygon)
}

#[derive(Clone, Copy, Debug)]
enum FrustumPlane {
    Left,
    Right,
    Bottom,
    Top,
    Near,
    Far,
}

impl FrustumPlane {
    const ALL: [FrustumPlane; 6] = [
        FrustumPlane::Left,
        FrustumPlane::Right,
        FrustumPlane::Bottom,
        FrustumPlane::Top,
        FrustumPlane::Near,
        FrustumPlane::Far,
    ];
}

fn frustum_plane_distance(vertex: ClippedVertex, plane: FrustumPlane, near: f32, far: f32) -> f32 {
    match plane {
        FrustumPlane::Left => vertex.clip_pos.x + vertex.clip_pos.w,
        FrustumPlane::Right => vertex.clip_pos.w - vertex.clip_pos.x,
        FrustumPlane::Bottom => vertex.clip_pos.y + vertex.clip_pos.w,
        FrustumPlane::Top => vertex.clip_pos.w - vertex.clip_pos.y,
        FrustumPlane::Near => -vertex.view_pos.z - near.max(EPSILON),
        FrustumPlane::Far => far.max(near.max(EPSILON)) + vertex.view_pos.z,
    }
}

fn clip_polygon_against_frustum_plane(
    polygon: &[ClippedVertex],
    plane: FrustumPlane,
    near: f32,
    far: f32,
) -> Vec<ClippedVertex> {
    let mut output = Vec::with_capacity(polygon.len() + 1);
    for i in 0..polygon.len() {
        let current = polygon[i];
        let next = polygon[(i + 1) % polygon.len()];
        let current_distance = frustum_plane_distance(current, plane, near, far);
        let next_distance = frustum_plane_distance(next, plane, near, far);
        let current_inside = current_distance >= -EPSILON;
        let next_inside = next_distance >= -EPSILON;

        match (current_inside, next_inside) {
            (true, true) => output.push(next),
            (true, false) => output.push(intersect_frustum_vertex(
                current,
                next,
                current_distance,
                next_distance,
            )),
            (false, true) => {
                output.push(intersect_frustum_vertex(
                    current,
                    next,
                    current_distance,
                    next_distance,
                ));
                output.push(next);
            }
            (false, false) => {}
        }
    }
    output
}

fn intersect_frustum_vertex(
    a: ClippedVertex,
    b: ClippedVertex,
    distance_a: f32,
    distance_b: f32,
) -> ClippedVertex {
    let denom = distance_a - distance_b;
    let t = if denom.abs() > EPSILON {
        (distance_a / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ClippedVertex {
        view_pos: a.view_pos + (b.view_pos - a.view_pos) * t,
        clip_pos: a.clip_pos + (b.clip_pos - a.clip_pos) * t,
        source_bc: a.source_bc + (b.source_bc - a.source_bc) * t,
    }
}

fn triangulate_clipped_polygon(polygon: &[ClippedVertex]) -> Vec<[ClippedVertex; 3]> {
    if polygon.len() < 3 {
        return Vec::new();
    }
    let mut triangles = Vec::with_capacity(polygon.len() - 2);
    for i in 1..polygon.len() - 1 {
        triangles.push([polygon[0], polygon[i], polygon[i + 1]]);
    }
    triangles
}

#[cfg(test)]
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

#[cfg(test)]
fn intersect_clipped_vertex(a: ClippedVertex, b: ClippedVertex, near_z: f32) -> ClippedVertex {
    let t = (near_z - a.view_pos.z) / (b.view_pos.z - a.view_pos.z);
    ClippedVertex {
        view_pos: a.view_pos + (b.view_pos - a.view_pos) * t,
        clip_pos: a.clip_pos + (b.clip_pos - a.clip_pos) * t,
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
    let shadow_state = shadow_pass_state();
    let mut shadow_image = ImageBuffer::new(width, height);
    let mut shadow_zbuffer = vec![shadow_state.clear_depth; (width * height) as usize];
    let mut depth_shader = DepthShader {
        model,
        transform: transforms.light_transform,
    };
    let shadow_transforms = RenderTransforms {
        transform: transforms.light_transform,
        model_view: transforms.light_model_view,
        view_to_clip: transforms.light_view_to_clip,
        clip_to_screen: transforms.light_clip_to_screen,
        light_transform: transforms.light_transform,
        light_model_view: transforms.light_model_view,
        light_view_to_clip: transforms.light_view_to_clip,
        light_clip_to_screen: transforms.light_clip_to_screen,
        light_dir_view: transforms.light_dir_view,
    };
    render_shader(
        model,
        &shadow_transforms,
        &mut depth_shader,
        &mut shadow_zbuffer,
        &mut shadow_image,
        options,
        shadow_state,
    );
    shadow_zbuffer
}

/// Fixed raster state for the light-space depth pass. The shadow map must not
/// inherit user state such as scissor, depth writes, culling, or blending, or a
/// flag meant for the camera view would silently crop or empty the shadow map.
fn shadow_pass_state() -> RenderState {
    RenderState {
        cull_mode: CullMode::Off,
        front_face: FrontFace::Ccw,
        depth: DepthState {
            test_enabled: true,
            write_enabled: true,
        },
        fill_mode: FillMode::Solid,
        scissor: None,
        clear_color: Rgb([0, 0, 0]),
        clear_depth: f32::NEG_INFINITY,
        color_write: false,
        blend_mode: BlendMode::Off,
    }
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
        options.render_state,
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
    sample_texture_with_mipmaps(texture, None, uv, sampler, 0.0)
}

pub fn sample_texture_with_mipmaps(
    texture: &RgbImage,
    mipmaps: Option<&[RgbImage]>,
    uv: Vec2,
    sampler: TextureSampler,
    lod: f32,
) -> Rgb<u8> {
    if sampler.filter == TextureFilter::Mipmap {
        if let Some(levels) = mipmaps {
            if !levels.is_empty() {
                let level = lod
                    .round()
                    .clamp(0.0, (levels.len().saturating_sub(1)) as f32)
                    as usize;
                let mut sampler = sampler;
                sampler.filter = TextureFilter::Linear;
                return sample_texture_level(&levels[level], uv, sampler);
            }
        }
    }
    sample_texture_level(texture, uv, sampler)
}

fn sample_texture_level(texture: &RgbImage, uv: Vec2, sampler: TextureSampler) -> Rgb<u8> {
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
        TextureFilter::Linear | TextureFilter::Mipmap => {
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

pub fn build_mipmaps(texture: &RgbImage) -> Vec<RgbImage> {
    let mut levels = vec![texture.clone()];
    while levels.last().unwrap().width() > 1 || levels.last().unwrap().height() > 1 {
        let previous = levels.last().unwrap();
        let next_width = (previous.width() / 2).max(1);
        let next_height = (previous.height() / 2).max(1);
        let next = ImageBuffer::from_fn(next_width, next_height, |x, y| {
            let sx = x * 2;
            let sy = y * 2;
            let samples = [
                *previous.get_pixel(sx.min(previous.width() - 1), sy.min(previous.height() - 1)),
                *previous.get_pixel(
                    (sx + 1).min(previous.width() - 1),
                    sy.min(previous.height() - 1),
                ),
                *previous.get_pixel(
                    sx.min(previous.width() - 1),
                    (sy + 1).min(previous.height() - 1),
                ),
                *previous.get_pixel(
                    (sx + 1).min(previous.width() - 1),
                    (sy + 1).min(previous.height() - 1),
                ),
            ];
            average_rgb(&samples)
        });
        levels.push(next);
    }
    levels
}

fn average_rgb(samples: &[Rgb<u8>; 4]) -> Rgb<u8> {
    Rgb([
        (samples.iter().map(|p| p[0] as u16).sum::<u16>() / 4) as u8,
        (samples.iter().map(|p| p[1] as u16).sum::<u16>() / 4) as u8,
        (samples.iter().map(|p| p[2] as u16).sum::<u16>() / 4) as u8,
    ])
}

fn triangle_lod(uv: [Vec2; 3], screen: [Vec2; 3], texture_width: u32, texture_height: u32) -> f32 {
    let screen_area = edge_function(screen[0], screen[1], screen[2])
        .abs()
        .max(1.0);
    let uv_area = edge_function(uv[0], uv[1], uv[2]).abs().max(EPSILON);
    let texel_area = uv_area * texture_width.max(1) as f32 * texture_height.max(1) as f32;
    (texel_area / screen_area).max(1.0).log2() * 0.5
}

fn lerp_rgb(a: Rgb<u8>, b: Rgb<u8>, t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    Rgb([
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ])
}

/// Inverse-transpose of the model-view matrix, used to move normals to eye space.
fn normal_matrix(model_view: Mat4) -> Mat4 {
    model_view.inverse().transpose()
}

/// Blinn-Phong specular term `max(dot(n, h), 0)^shininess` with half-vector
/// `h = normalize(l + v)`. All vectors are unit length and in the same space;
/// `n` is the outward normal, `l` points toward the light, `v` toward the viewer.
fn blinn_phong(n: Vec3, l: Vec3, v: Vec3, shininess: f32) -> f32 {
    if n.dot(l) <= 0.0 {
        return 0.0;
    }
    let h = safe_normal(l + v, n);
    n.dot(h).max(0.0).powf(shininess.max(1.0))
}

/// Specular highlight for an eye-space normal and position. The eye sits at the
/// eye-space origin, so the view vector is `-eye_pos`. Skips the work entirely
/// for non-specular materials.
fn specular_highlight(material: &Material, n: Vec3, light_dir: Vec3, eye_pos: Vec3) -> f32 {
    if material.specular <= 0.0 {
        return 0.0;
    }
    blinn_phong(
        n,
        light_dir,
        safe_normal(-eye_pos, Vec3::Z),
        material.shininess,
    )
}

fn interpolate_scalar(values: [f32; 3], bc: Vec3) -> f32 {
    values[0] * bc.x + values[1] * bc.y + values[2] * bc.z
}

/// Texture coordinate of a face vertex, or the origin when the vertex has none.
/// Resetting (rather than keeping the previous face's value) keeps the vertex
/// stage free of cross-face state.
fn face_vertex_uv(model: &Model, fv: FaceVertex) -> Vec2 {
    fv.vt.map_or(Vec2::ZERO, |ti| model.texcoords[ti])
}

fn tint_rgb(pixel: Rgb<u8>, tint: Rgb<u8>) -> Rgb<u8> {
    Rgb([
        (pixel[0] as u16 * tint[0] as u16 / 255) as u8,
        (pixel[1] as u16 * tint[1] as u16 / 255) as u8,
        (pixel[2] as u16 * tint[2] as u16 / 255) as u8,
    ])
}

/// Combine ambient, diffuse, and specular terms for a material.
///
/// `diffuse_intensity` is the Lambert term and `specular_highlight` the
/// already-exponentiated Blinn-Phong term (see [`specular_highlight`]); the
/// material's `diffuse`/`specular` factors scale them.
fn shade_material(
    base: Rgb<u8>,
    material: Material,
    diffuse_intensity: f32,
    specular_highlight: f32,
) -> Rgb<u8> {
    let diffuse = diffuse_intensity.max(0.0) * material.diffuse.max(0.0);
    let specular = specular_highlight.max(0.0) * material.specular.max(0.0);
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

    fn temp_asset_pair(obj_contents: &str, mtl_contents: &str) -> String {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir();
        let obj_path = dir.join(format!("octo-render-test-{id}.obj"));
        let mtl_path = dir.join(format!("octo-render-test-{id}.mtl"));
        let obj_contents =
            obj_contents.replace("{MTL}", mtl_path.file_name().unwrap().to_str().unwrap());
        fs::write(&obj_path, obj_contents).unwrap();
        fs::write(&mtl_path, mtl_contents).unwrap();
        obj_path.to_string_lossy().into_owned()
    }

    fn tiny_model() -> Model {
        Model {
            vertices: vec![
                Vec3::new(-0.6, -0.6, 0.0),
                Vec3::new(0.6, -0.6, 0.0),
                Vec3::new(0.0, 0.6, 0.0),
            ],
            texcoords: Vec::new(),
            normals: vec![Vec3::new(0.0, 0.0, 1.0)],
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
            materials: Vec::new(),
            face_materials: vec![None],
        }
    }

    /// A large receiver triangle at z = 0 with a smaller occluder floating in
    /// front of it at z = 0.4. Both face +Z (counter-clockwise from the camera).
    fn occluder_model() -> Model {
        let corner = |v: usize| FaceVertex {
            v,
            vt: None,
            vn: None,
        };
        Model {
            vertices: vec![
                Vec3::new(-1.0, -1.0, 0.0),
                Vec3::new(1.0, -1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(-0.3, -0.3, 0.4),
                Vec3::new(0.3, -0.3, 0.4),
                Vec3::new(0.0, 0.3, 0.4),
            ],
            texcoords: Vec::new(),
            normals: Vec::new(),
            faces: vec![
                [corner(0), corner(1), corner(2)],
                [corner(3), corner(4), corner(5)],
            ],
            materials: Vec::new(),
            face_materials: vec![None, None],
        }
    }

    /// Axis-aligned cube from -1 to 1 with outward, counter-clockwise faces,
    /// each quad fan-triangulated. With `shared_corners` the six faces share
    /// eight vertices; otherwise every face has its own four copies, as many
    /// exporters write them.
    fn cube_model(shared_corners: bool) -> Model {
        let corner = |i: usize| {
            Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            )
        };
        let quads: [[usize; 4]; 6] = [
            [4, 5, 7, 6], // +Z
            [0, 2, 3, 1], // -Z
            [1, 3, 7, 5], // +X
            [0, 4, 6, 2], // -X
            [2, 6, 7, 3], // +Y
            [0, 1, 5, 4], // -Y
        ];
        let mut vertices: Vec<Vec3> = if shared_corners {
            (0..8).map(corner).collect()
        } else {
            Vec::new()
        };
        let mut faces = Vec::new();
        for quad in quads {
            let ids: [usize; 4] = if shared_corners {
                quad
            } else {
                let base = vertices.len();
                vertices.extend(quad.iter().map(|&i| corner(i)));
                [base, base + 1, base + 2, base + 3]
            };
            let fv = |v: usize| FaceVertex {
                v,
                vt: None,
                vn: None,
            };
            faces.push([fv(ids[0]), fv(ids[1]), fv(ids[2])]);
            faces.push([fv(ids[0]), fv(ids[2]), fv(ids[3])]);
        }
        let face_count = faces.len();
        Model {
            vertices,
            texcoords: Vec::new(),
            normals: Vec::new(),
            faces,
            materials: Vec::new(),
            face_materials: vec![None; face_count],
        }
    }

    /// Flat 4x4-vertex grid in the z = 0 plane, facing +Z.
    fn grid_model() -> Model {
        let vertices: Vec<Vec3> = (0..16)
            .map(|i| Vec3::new((i % 4) as f32, (i / 4) as f32, 0.0))
            .collect();
        let fv = |v: usize| FaceVertex {
            v,
            vt: None,
            vn: None,
        };
        let mut faces = Vec::new();
        for y in 0..3 {
            for x in 0..3 {
                let i = y * 4 + x;
                faces.push([fv(i), fv(i + 1), fv(i + 5)]);
                faces.push([fv(i), fv(i + 5), fv(i + 4)]);
            }
        }
        let face_count = faces.len();
        Model {
            vertices,
            texcoords: Vec::new(),
            normals: Vec::new(),
            faces,
            materials: Vec::new(),
            face_materials: vec![None; face_count],
        }
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3, context: &str) {
        assert!(
            (actual - expected).length() < 1e-5,
            "{context}: expected {expected:?}, got {actual:?}"
        );
    }

    fn clip_test_vertex(clip_pos: Vec4, source_bc: Vec3) -> ClippedVertex {
        ClippedVertex {
            view_pos: Vec3::new(clip_pos.x, clip_pos.y, -1.0),
            clip_pos,
            source_bc,
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
    fn obj_loader_parses_mtl_materials_and_face_assignments() {
        let path = temp_asset_pair(
            "\
mtllib {MTL}
v 0 0 0
v 1 0 0
v 0 1 0
vt 0 0
vt 1 0
vt 0 1
vn 0 0 -1
usemtl painted
f 1/1/1 2/2/1 3/3/1
",
            "\
newmtl painted
Kd 0.25 0.50 0.75
Ka 0.10 0.10 0.10
Ks 0.20 0.20 0.20
Ns 64
d 0.5
map_Kd checker.ppm
map_Bump normal-checker.ppm
",
        );
        let model = Model::load(path).unwrap();
        assert_eq!(model.materials.len(), 1);
        assert_eq!(model.materials[0].name, "painted");
        assert_eq!(model.materials[0].material.base_color, Rgb([64, 128, 191]));
        assert_eq!(model.materials[0].material.alpha, 0.5);
        assert!(model.materials[0].diffuse_map.is_some());
        assert!(model.materials[0].normal_map.is_some());
        assert_eq!(model.face_materials, vec![Some(0)]);
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
    fn top_left_rule_assigns_shared_edge_to_one_triangle() {
        let p = Vec2::new(1.5, 1.5);
        let first = point_in_triangle_top_left(
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 2.0),
            p,
        );
        let second = point_in_triangle_top_left(
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(0.0, 2.0),
            p,
        );
        assert_ne!(first, second);
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
        assert_eq!(ao, compute_ssao(&zbuffer, 3, 3, 1, 4));

        let options = RenderOptions {
            width: 48,
            height: 48,
            mode: RenderMode::Ssao,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(1.0, 0.6, 1.0),
            ..RenderOptions::default()
        };
        let first = render_model(&occluder_model(), &options);
        let second = render_model(&occluder_model(), &options);
        assert_eq!(first.image.as_raw(), second.image.as_raw());
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
    fn mipmap_generation_and_sampling_are_deterministic() {
        let tex = ImageBuffer::from_fn(4, 4, |x, y| {
            Rgb([(x * 40) as u8, (y * 40) as u8, ((x + y) * 20) as u8])
        });
        let mipmaps = build_mipmaps(&tex);
        assert_eq!(mipmaps.len(), 3);
        assert_eq!(mipmaps[0].dimensions(), (4, 4));
        assert_eq!(mipmaps[1].dimensions(), (2, 2));
        assert_eq!(mipmaps[2].dimensions(), (1, 1));
        let sampled = sample_texture_with_mipmaps(
            &tex,
            Some(&mipmaps),
            Vec2::new(0.25, 0.75),
            TextureSampler {
                wrap: TextureWrap::Clamp,
                filter: TextureFilter::Mipmap,
            },
            1.0,
        );
        assert_eq!(sampled, Rgb([40, 40, 40]));
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
                    clip_pos: Vec4::new(-1.0, -1.0, -2.0, 1.0),
                    source_bc: Vec3::X,
                },
                ClippedVertex {
                    view_pos: Vec3::new(1.0, -1.0, -2.0),
                    clip_pos: Vec4::new(1.0, -1.0, -2.0, 1.0),
                    source_bc: Vec3::Y,
                },
                ClippedVertex {
                    view_pos: Vec3::new(0.0, 1.0, 0.5),
                    clip_pos: Vec4::new(0.0, 1.0, 0.5, 1.0),
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
    fn frustum_clip_rejects_fully_outside_triangle() {
        let clipped = clip_varying_triangle_against_frustum(
            [
                clip_test_vertex(Vec4::new(-3.0, 0.0, 0.0, 1.0), Vec3::X),
                clip_test_vertex(Vec4::new(-2.5, 0.5, 0.0, 1.0), Vec3::Y),
                clip_test_vertex(Vec4::new(-2.5, -0.5, 0.0, 1.0), Vec3::Z),
            ],
            0.1,
            10.0,
        );
        assert!(clipped.is_empty());
    }

    #[test]
    fn frustum_clip_preserves_varyings_when_crossing_multiple_planes() {
        let clipped = clip_varying_triangle_against_frustum(
            [
                clip_test_vertex(Vec4::new(-2.0, 0.0, 0.0, 1.0), Vec3::X),
                clip_test_vertex(Vec4::new(0.0, 2.0, 0.0, 1.0), Vec3::Y),
                clip_test_vertex(Vec4::new(0.4, 0.4, 0.0, 1.0), Vec3::Z),
            ],
            0.1,
            10.0,
        );
        assert!(!clipped.is_empty());
        for vertex in clipped.iter().flatten() {
            assert!(vertex.clip_pos.x + vertex.clip_pos.w >= -EPSILON);
            assert!(vertex.clip_pos.w - vertex.clip_pos.x >= -EPSILON);
            assert!(vertex.clip_pos.y + vertex.clip_pos.w >= -EPSILON);
            assert!(vertex.clip_pos.w - vertex.clip_pos.y >= -EPSILON);
            assert!(
                (vertex.source_bc.x + vertex.source_bc.y + vertex.source_bc.z - 1.0).abs()
                    < EPSILON
            );
        }
    }

    #[test]
    fn blend_modes_apply_alpha_and_additive_math() {
        let dst = Rgb([100, 20, 10]);
        let src = Fragment::new(Rgb([200, 100, 50]), 0.5);
        assert_eq!(
            blend_fragment(dst, src, BlendMode::Alpha),
            Rgb([150, 60, 30])
        );
        assert_eq!(
            blend_fragment(dst, src, BlendMode::Additive),
            Rgb([200, 70, 35])
        );
        assert_eq!(blend_fragment(dst, src, BlendMode::Off), src.colour);
    }

    #[test]
    fn scissor_limits_raster_output() {
        let clear = Rgb([3, 4, 5]);
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, 1.0),
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
            light_dir: Vec3::new(0.0, 0.0, 1.0),
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
            light_dir: Vec3::new(0.0, 0.0, 1.0),
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
    fn occluder_casts_shadow_and_pcf_softens_its_edge() {
        let render = |filter: ShadowFilter| {
            let options = RenderOptions {
                width: 64,
                height: 64,
                mode: RenderMode::Shadow,
                camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
                light_dir: Vec3::new(1.0, 0.6, 1.0),
                shadow: ShadowOptions {
                    size: 64,
                    filter,
                    ..ShadowOptions::default()
                },
                ..RenderOptions::default()
            };
            render_model(&occluder_model(), &options).image
        };
        let hard = render(ShadowFilter::None);
        let soft = render(ShadowFilter::Pcf);

        // Both quads face the camera and the light equally, so without shadows
        // every covered pixel would have the same colour. A shadow adds a second,
        // darker shade on the receiver.
        let lit = hard.pixels().map(|p| p[0]).max().unwrap();
        assert!(hard
            .pixels()
            .any(|p| *p != DEFAULT_BACKGROUND && p[0] > 0 && p[0] < lit));
        // PCF blends taps along the shadow edge, so it must change the result.
        assert_ne!(hard.as_raw(), soft.as_raw());
    }

    #[test]
    fn shadow_map_ignores_camera_raster_state() {
        let render = |render_state: RenderState| {
            let options = RenderOptions {
                width: 64,
                height: 64,
                mode: RenderMode::Shadow,
                camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
                light_dir: Vec3::new(1.0, 0.6, 1.0),
                debug_outputs: true,
                render_state,
                shadow: ShadowOptions {
                    size: 32,
                    ..ShadowOptions::default()
                },
                ..RenderOptions::default()
            };
            render_model(&occluder_model(), &options)
                .debug
                .shadow
                .unwrap()
        };
        let reference = render(RenderState::default());
        let restricted = render(RenderState {
            cull_mode: CullMode::Front,
            depth: DepthState {
                test_enabled: true,
                write_enabled: false,
            },
            scissor: Some(ScissorRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }),
            color_write: false,
            blend_mode: BlendMode::Additive,
            ..RenderState::default()
        });
        assert!(reference.pixels().any(|p| p[0] > 0));
        assert_eq!(reference.as_raw(), restricted.as_raw());
    }

    #[test]
    fn specular_material_adds_highlight_where_light_and_view_align() {
        let render = |specular: f32| {
            let options = RenderOptions {
                width: 32,
                height: 32,
                mode: RenderMode::Gouraud,
                camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
                light_dir: Vec3::new(0.0, 0.0, 1.0),
                material: Material {
                    base_color: Rgb([100, 100, 100]),
                    specular,
                    shininess: 16.0,
                    ..Material::default()
                },
                ..RenderOptions::default()
            };
            render_model(&tiny_model(), &options).image
        };
        let matte = render(0.0);
        let shiny = render(1.0);
        assert_eq!(matte.get_pixel(16, 16), &Rgb([100, 100, 100]));
        assert!(shiny.get_pixel(16, 16)[0] > 200);
        assert!(shiny
            .pixels()
            .zip(matte.pixels())
            .all(|(s, m)| s[0] >= m[0]));
    }

    #[test]
    fn blinn_phong_peaks_on_the_half_vector_and_ignores_back_lighting() {
        let n = Vec3::Z;
        assert!((blinn_phong(n, Vec3::Z, Vec3::Z, 32.0) - 1.0).abs() < EPSILON);
        let off_axis = Vec3::new(0.6, 0.0, 0.8);
        let mirrored = Vec3::new(-0.6, 0.0, 0.8);
        assert!((blinn_phong(n, off_axis, mirrored, 32.0) - 1.0).abs() < 1e-5);
        assert!(blinn_phong(n, off_axis, Vec3::Z, 32.0) < blinn_phong(n, off_axis, Vec3::Z, 2.0));
        assert_eq!(blinn_phong(n, -Vec3::Z, Vec3::Z, 32.0), 0.0);
    }

    #[test]
    fn unlit_surfaces_still_write_depth_and_occlude() {
        // The light sits behind the triangle, so its camera-facing side is unlit.
        // It must still render (black, with no ambient) and fill the z-buffer
        // instead of leaving a hole that shows the clear colour.
        for mode in [
            RenderMode::Flat,
            RenderMode::Gouraud,
            RenderMode::Texture,
            RenderMode::NormalMap,
            RenderMode::Shadow,
        ] {
            let options = RenderOptions {
                width: 32,
                height: 32,
                mode,
                camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
                light_dir: Vec3::new(0.0, 0.0, -1.0),
                ..RenderOptions::default()
            };
            let output = render_model(&tiny_model(), &options);
            assert_eq!(
                output.image.get_pixel(16, 16),
                &Rgb([0, 0, 0]),
                "mode {}",
                mode.as_str()
            );
            let covered = output.zbuffer.iter().filter(|z| z.is_finite()).count();
            assert!(
                covered > 100,
                "mode {} wrote {covered} depths",
                mode.as_str()
            );
        }
    }

    #[test]
    fn cube_fixture_faces_point_outward() {
        let cube = cube_model(true);
        for face_idx in 0..cube.faces.len() {
            let centroid = cube.faces[face_idx]
                .iter()
                .map(|fv| cube.vertices[fv.v])
                .sum::<Vec3>()
                / 3.0;
            assert!(cube.face_normal(face_idx).dot(centroid) > 0.0);
        }
    }

    #[test]
    fn generated_normals_average_every_face_without_a_crease_limit() {
        for shared_corners in [true, false] {
            let mut cube = cube_model(shared_corners);
            cube.generate_missing_normals(180.0);
            for face_idx in 0..cube.faces.len() {
                for corner in 0..3 {
                    let position = cube.vertices[cube.faces[face_idx][corner].v];
                    assert_vec3_close(
                        cube.vertex_normal(face_idx, corner),
                        position.normalize(),
                        &format!("shared={shared_corners} face {face_idx} corner {corner}"),
                    );
                }
            }
        }
    }

    #[test]
    fn generated_normals_keep_edges_sharper_than_the_crease_angle_hard() {
        let mut cube = cube_model(false);
        cube.generate_missing_normals(DEFAULT_CREASE_ANGLE_DEG);
        for face_idx in 0..cube.faces.len() {
            for corner in 0..3 {
                assert_vec3_close(
                    cube.vertex_normal(face_idx, corner),
                    cube.face_normal(face_idx),
                    &format!("face {face_idx} corner {corner}"),
                );
            }
        }
    }

    #[test]
    fn generated_normals_on_a_flat_grid_point_along_its_face() {
        let mut grid = grid_model();
        grid.generate_missing_normals(DEFAULT_CREASE_ANGLE_DEG);
        for face_idx in 0..grid.faces.len() {
            for corner in 0..3 {
                assert_vec3_close(grid.vertex_normal(face_idx, corner), Vec3::Z, "grid");
            }
        }
        // Every corner of a vertex shares one stored normal.
        assert_eq!(grid.normals.len(), grid.vertices.len());
    }

    #[test]
    fn generated_normals_leave_authored_normals_alone() {
        let mut model = tiny_model();
        let mut untextured = model.faces[0];
        for fv in &mut untextured {
            fv.vn = None;
        }
        model.faces.push(untextured);
        model.face_materials.push(None);
        model.generate_missing_normals(DEFAULT_CREASE_ANGLE_DEG);
        assert_eq!(model.faces[0].map(|fv| fv.vn), [Some(0); 3]);
        assert_eq!(model.normals[0], Vec3::new(0.0, 0.0, 1.0));
        assert!(model.faces[1].iter().all(|fv| fv.vn.is_some()));
    }

    #[test]
    fn teapot_loads_with_smooth_unit_normals_facing_out() {
        let faceted = Model::load_with_options(
            "resources/teapot.obj",
            ObjLoadOptions {
                crease_angle_deg: None,
            },
        )
        .unwrap();
        assert!(faceted.normals.is_empty());

        let model = Model::load("resources/teapot.obj").unwrap();
        let mut smoothed_corners = 0;
        for face_idx in 0..model.faces.len() {
            for corner in 0..3 {
                let Some(index) = model.faces[face_idx][corner].vn else {
                    continue;
                };
                smoothed_corners += 1;
                let normal = model.normals[index];
                assert!((normal.length() - 1.0).abs() < 1e-4);
                assert!(normal.dot(model.face_normal(face_idx)) > 0.0);
            }
        }
        // Only degenerate faces (at the lid knob and base poles) keep the
        // per-face fallback.
        assert!(smoothed_corners > model.faces.len() * 3 * 9 / 10);
        assert!(model.normals.len() < smoothed_corners);
    }

    #[test]
    fn faces_without_texcoords_do_not_inherit_previous_uvs() {
        let mut model = tiny_model();
        model.texcoords = vec![Vec2::new(0.25, 0.75)];
        let untextured = model.faces[0];
        for fv in &mut model.faces[0] {
            fv.vt = Some(0);
        }
        model.faces.push(untextured);
        model.face_materials.push(None);

        let mut texture_shader = TextureShader::new(
            &model,
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            Vec3::Z,
            None,
            None,
            TextureSampler::default(),
            Material::default(),
        );
        let normal_map = neutral_normal_map(2);
        let mut normal_shader = NormalMapShader::new(
            &model,
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            Vec3::Z,
            None,
            &normal_map,
            None,
            None,
            TextureSampler::default(),
            Material::default(),
        );
        for j in 0..3 {
            texture_shader.vertex(0, j);
            normal_shader.vertex(0, j);
        }
        assert_eq!(texture_shader.varying_uv, [Vec2::new(0.25, 0.75); 3]);
        assert_eq!(normal_shader.varying_uv, [Vec2::new(0.25, 0.75); 3]);
        for j in 0..3 {
            texture_shader.vertex(1, j);
            normal_shader.vertex(1, j);
        }
        assert_eq!(texture_shader.varying_uv, [Vec2::ZERO; 3]);
        assert_eq!(normal_shader.varying_uv, [Vec2::ZERO; 3]);
    }

    #[test]
    fn debug_outputs_include_depth_normals_shadow_and_ao() {
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Ssao,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, 1.0),
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
            light_dir: Vec3::new(0.0, 0.0, 1.0),
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
    fn clip_space_winding_matches_screen_winding() {
        use rand::{rngs::StdRng, Rng, SeedableRng};

        let cube = cube_model(true);
        let options = RenderOptions {
            width: 256,
            height: 256,
            camera_eye: Some(Vec3::new(0.0, 0.25, 3.0)),
            ..RenderOptions::default()
        };
        let transforms = build_transforms(&cube, &options, 256, 256, 256, 256);
        let mut rng = StdRng::seed_from_u64(0x5eed);
        let (mut checked, mut crossing) = (0, 0);
        for _ in 0..4000 {
            // Scene points reach well behind the eye, so some triangles have
            // corners behind the centre of projection (w <= 0).
            let corners: [Vec3; 3] = std::array::from_fn(|_| {
                Vec3::new(
                    rng.gen_range(-4.0..4.0),
                    rng.gen_range(-4.0..4.0),
                    rng.gen_range(-4.0..8.0),
                )
            });
            let triangle: [ClippedVertex; 3] = std::array::from_fn(|i| {
                clipped_vertex_for_source(corners[i], &transforms, [Vec3::X, Vec3::Y, Vec3::Z][i])
            });
            let winding = homogeneous_winding(&triangle);
            let pieces = clip_varying_triangle_against_frustum(
                triangle,
                options.near_plane,
                options.far_plane,
            );
            if triangle.iter().any(|v| v.clip_pos.w <= 0.0) && !pieces.is_empty() {
                crossing += 1;
            }
            for piece in &pieces {
                let screen = project_clipped_triangle(piece, &transforms).screen_coords;
                let [a, b, c] = screen.map(Vec3::truncate);
                let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                if area.abs() < 1.0 {
                    // Slivers: their winding is numerically meaningless.
                    continue;
                }
                checked += 1;
                for front_face in [FrontFace::Ccw, FrontFace::Cw] {
                    for cull_mode in [CullMode::Back, CullMode::Front] {
                        assert_eq!(
                            should_cull_winding(winding, cull_mode, front_face),
                            should_cull_screen_triangle_with_front_face(
                                &screen, cull_mode, front_face
                            ),
                            "corners {corners:?}, winding {winding}, piece area {area}"
                        );
                    }
                }
            }
        }
        assert!(checked > 1000, "only {checked} pieces checked");
        assert!(
            crossing > 20,
            "only {crossing} triangles crossed behind the eye"
        );
    }

    #[test]
    fn floor_crossing_behind_the_eye_keeps_its_winding() {
        let cube = cube_model(true);
        let options = RenderOptions {
            width: 128,
            height: 128,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            ..RenderOptions::default()
        };
        let transforms = build_transforms(&cube, &options, 128, 128, 128, 128);
        // An upward-facing floor below the eye, running from in front of the
        // camera to far behind it.
        let corner = |v: usize| FaceVertex {
            v,
            vt: None,
            vn: None,
        };
        let mut floor = Model {
            vertices: vec![
                Vec3::new(-1.0, -1.0, 0.0),
                Vec3::new(0.0, -1.0, 6.0),
                Vec3::new(1.0, -1.0, 0.0),
            ],
            texcoords: Vec::new(),
            normals: Vec::new(),
            faces: vec![[corner(0), corner(1), corner(2)]],
            materials: Vec::new(),
            face_materials: vec![None],
        };
        assert!(floor.face_normal(0).y > 0.0);
        assert!(face_clip_vertices(&floor, &transforms, 0)
            .iter()
            .any(|v| v.clip_pos.w < 0.0));

        let back_culling = RenderState {
            cull_mode: CullMode::Back,
            ..RenderState::default()
        };
        let mut pieces = Vec::new();
        assert!(prepare_face(
            &floor,
            &transforms,
            0,
            options.near_plane,
            options.far_plane,
            back_culling,
            &mut pieces
        ));
        for piece in &pieces {
            let [a, b, c] = piece.screen_coords.map(Vec3::truncate);
            assert!((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) > 0.0);
        }

        floor.faces[0].swap(1, 2);
        assert!(!prepare_face(
            &floor,
            &transforms,
            0,
            options.near_plane,
            options.far_plane,
            back_culling,
            &mut pieces
        ));
    }

    /// Pins culled output so the culling stage can be moved without changing
    /// a single pixel or depth value.
    #[test]
    fn culled_teapot_renders_stay_stable() {
        let model = Model::load("resources/teapot.obj").unwrap();
        let mut checksums = Vec::new();
        for mode in [RenderMode::Flat, RenderMode::Gouraud] {
            for cull_mode in [CullMode::Back, CullMode::Front] {
                let options = RenderOptions {
                    width: 64,
                    height: 64,
                    mode,
                    camera_eye: Some(Vec3::new(0.0, 0.25, 3.0)),
                    render_state: RenderState {
                        cull_mode,
                        ..RenderState::default()
                    },
                    ..RenderOptions::default()
                };
                let output = render_model(&model, &options);
                checksums.push((
                    image_checksum(&output.image),
                    depth_bits_hash(&output.zbuffer),
                ));
            }
        }
        assert_eq!(checksums, CULLED_TEAPOT_CHECKSUMS);
    }

    const CULLED_TEAPOT_CHECKSUMS: [(u64, u64); 4] = [
        (1_506_840, 9_950_000_386_150_388_727),
        (937_821, 14_738_515_683_572_321_133),
        (1_508_325, 9_950_000_386_150_388_727),
        (939_097, 14_738_515_683_572_321_133),
    ];

    /// Order-sensitive hash of the exact depth bits.
    fn depth_bits_hash(zbuffer: &[f32]) -> u64 {
        zbuffer.iter().fold(0u64, |hash, z| {
            hash.wrapping_mul(1_099_511_628_211)
                .wrapping_add(z.to_bits() as u64)
        })
    }

    #[test]
    fn teapot_flat_render_matches_golden_image_with_tolerance() {
        let model = Model::load("resources/teapot.obj").unwrap();
        let options = RenderOptions {
            width: 64,
            height: 64,
            mode: RenderMode::Flat,
            camera_eye: Some(Vec3::new(0.0, 0.25, 3.0)),
            light_dir: DEFAULT_LIGHT_DIR,
            debug_outputs: false,
            ..RenderOptions::default()
        };
        let output = render_model(&model, &options);
        assert_matches_golden(&output.image, "flat-teapot-64.png", 1);
    }

    #[test]
    fn mtl_textured_quad_matches_golden_image_with_tolerance() {
        let output = render_material_quad(TextureFilter::Nearest, BlendMode::Off, None);
        assert_matches_golden(&output, "material-quad-32.png", 1);
    }

    #[test]
    fn alpha_blended_quad_matches_golden_image_with_tolerance() {
        let output = render_material_quad(TextureFilter::Nearest, BlendMode::Alpha, Some(0.45));
        assert_matches_golden(&output, "material-quad-alpha-32.png", 1);
    }

    #[test]
    fn mipmapped_quad_matches_golden_image_with_tolerance() {
        let output = render_material_quad(TextureFilter::Mipmap, BlendMode::Off, None);
        assert_matches_golden(&output, "material-quad-mipmap-32.png", 1);
    }

    fn render_material_quad(
        filter: TextureFilter,
        blend_mode: BlendMode,
        alpha_override: Option<f32>,
    ) -> RgbImage {
        let model = Model::load("resources/material-quad.obj").unwrap();
        let material = model.materials.first().unwrap();
        let texture = image::open(material.diffuse_map.as_ref().unwrap())
            .unwrap()
            .to_rgb8();
        let texture_mipmaps = if filter == TextureFilter::Mipmap {
            build_mipmaps(&texture)
        } else {
            Vec::new()
        };
        let mut material_state = material.material;
        if let Some(alpha) = alpha_override {
            material_state.alpha = alpha;
        }
        let mut render_state = RenderState {
            blend_mode,
            ..RenderState::default()
        };
        if blend_mode != BlendMode::Off {
            render_state.depth = DepthState {
                test_enabled: false,
                write_enabled: false,
            };
        }
        let options = RenderOptions {
            width: 32,
            height: 32,
            mode: RenderMode::Texture,
            light_dir: Vec3::new(0.0, 0.0, 1.0),
            texture: Some(texture),
            texture_mipmaps,
            texture_sampler: TextureSampler {
                wrap: TextureWrap::Clamp,
                filter,
            },
            material: material_state,
            render_state,
            ..RenderOptions::default()
        };
        render_model(&model, &options).image
    }

    fn image_checksum(image: &RgbImage) -> u64 {
        image
            .pixels()
            .map(|p| p[0] as u64 + p[1] as u64 * 3 + p[2] as u64 * 7)
            .sum()
    }

    /// Compare `actual` with `tests/golden/<name>`. Run with `UPDATE_GOLDENS=1` to
    /// rewrite the golden from the current output instead, then inspect the image
    /// diff before committing it.
    fn assert_matches_golden(actual: &RgbImage, name: &str, tolerance: u8) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            actual.save(&path).unwrap();
            return;
        }
        let golden = image::open(&path)
            .unwrap_or_else(|err| panic!("failed to load golden '{}': {err}", path.display()))
            .to_rgb8();
        assert_images_close(actual, &golden, tolerance);
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
