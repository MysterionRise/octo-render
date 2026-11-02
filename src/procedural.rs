//! Procedural meshes: shapes built from math instead of loaded from OBJ files.
//!
//! Every generator returns a [`Model`] with unit vertex normals and outward,
//! counter-clockwise faces, so it shades and culls like an imported mesh.
//! Closed shapes close their seams and poles bit for bit, so welding vertices
//! by exact position yields a watertight, consistently wound mesh.

use crate::{FaceVertex, Model};
use glam::{Vec2, Vec3};
use std::f32::consts::{PI, TAU};

/// Sine and cosine of `t` full turns, exact at `t = 0` and `t = 1`, so a
/// seam's first and last columns land on identical positions.
fn turn(t: f32) -> (f32, f32) {
    if t <= 0.0 || t >= 1.0 {
        (0.0, 1.0)
    } else {
        (t * TAU).sin_cos()
    }
}

/// Sine and cosine of `t` half turns, exact at `t = 0` and `t = 1` (poles).
fn half_turn(t: f32) -> (f32, f32) {
    if t <= 0.0 {
        (0.0, 1.0)
    } else if t >= 1.0 {
        (0.0, -1.0)
    } else {
        (t * PI).sin_cos()
    }
}

/// `t` full turns in radians, wrapped so that `t = 1` gives exactly the same
/// angle as `t = 0`.
fn turn_angle(t: f32) -> f32 {
    if t >= 1.0 {
        0.0
    } else {
        t * TAU
    }
}

/// Build a mesh from a parametric surface sampled on an `nu` by `nv` grid over
/// `u, v` in `[0, 1]`. `sample` returns a position and its normal.
///
/// Seam vertices are duplicated so texture coordinates stay continuous. Each
/// grid cell becomes the triangles `[a, b, c]` and `[a, c, d]`, so faces are
/// counter-clockwise around `du x dv`: parameterize the surface so that points
/// outward. Degenerate triangles, such as those at a sphere's poles, are
/// dropped.
pub fn grid_surface(
    nu: usize,
    nv: usize,
    mut sample: impl FnMut(f32, f32) -> (Vec3, Vec3),
) -> Model {
    let (nu, nv) = (nu.max(1), nv.max(1));
    let mut vertices = Vec::with_capacity((nu + 1) * (nv + 1));
    let mut normals = Vec::with_capacity(vertices.capacity());
    let mut texcoords = Vec::with_capacity(vertices.capacity());
    for j in 0..=nv {
        for i in 0..=nu {
            let (u, v) = (i as f32 / nu as f32, j as f32 / nv as f32);
            let (position, normal) = sample(u, v);
            vertices.push(position);
            normals.push(normal.try_normalize().unwrap_or(Vec3::Y));
            texcoords.push(Vec2::new(u, v));
        }
    }

    let index = |i: usize, j: usize| j * (nu + 1) + i;
    let corner = |k: usize| FaceVertex {
        v: k,
        vt: Some(k),
        vn: Some(k),
    };
    let mut faces = Vec::with_capacity(2 * nu * nv);
    for j in 0..nv {
        for i in 0..nu {
            let (a, b, c, d) = (
                index(i, j),
                index(i + 1, j),
                index(i + 1, j + 1),
                index(i, j + 1),
            );
            for triangle in [[a, b, c], [a, c, d]] {
                let [p0, p1, p2] = triangle.map(|k| vertices[k]);
                if (p1 - p0).cross(p2 - p0).try_normalize().is_some() {
                    faces.push(triangle.map(corner));
                }
            }
        }
    }

    let face_count = faces.len();
    Model {
        vertices,
        texcoords,
        normals,
        faces,
        materials: Vec::new(),
        face_materials: vec![None; face_count],
    }
}

/// A square floor in the `y = 0` plane spanning `[-half, half]` in x and z,
/// split into `divisions` cells per side, facing +Y.
pub fn ground_grid(half: f32, divisions: usize) -> Model {
    let n = divisions.max(1);
    grid_surface(n, n, |u, v| {
        let x = -half + 2.0 * half * u;
        let z = half - 2.0 * half * v;
        (Vec3::new(x, 0.0, z), Vec3::Y)
    })
}

/// A round floor in the `y = 0` plane, facing +Y.
pub fn ground_disk(radius: f32, rings: usize, segments: usize) -> Model {
    grid_surface(segments, rings, |u, v| {
        let (sin, cos) = turn(u);
        let rho = radius * v;
        (Vec3::new(rho * cos, 0.0, rho * sin), Vec3::Y)
    })
}

/// A sphere centred on the origin, with `segments` slices around Y and
/// `rings` stacks from pole to pole.
pub fn uv_sphere(radius: f32, segments: usize, rings: usize) -> Model {
    grid_surface(segments, rings, |u, v| {
        let (sin_theta, cos_theta) = turn(u);
        let (sin_phi, cos_phi) = half_turn(v);
        let normal = Vec3::new(sin_phi * cos_theta, cos_phi, sin_phi * sin_theta);
        (normal * radius, normal)
    })
}

/// A torus around the Y axis: `major` is the distance from the centre to the
/// tube's centre line and `minor` the tube radius.
pub fn torus(major: f32, minor: f32, ring_segments: usize, tube_segments: usize) -> Model {
    grid_surface(tube_segments, ring_segments, |u, v| {
        let (sin_phi, cos_phi) = turn(u);
        let (sin_theta, cos_theta) = turn(v);
        let normal = Vec3::new(cos_phi * cos_theta, sin_phi, cos_phi * sin_theta);
        let ring = Vec3::new(major * cos_theta, 0.0, major * sin_theta);
        (ring + normal * minor, normal)
    })
}

/// Point on a `(p, q)` torus knot around the Y axis at `s` radians, and its
/// derivative: it winds `p` times around the axis and `q` times around a core
/// circle of radius `major`, with amplitude `amplitude`.
fn torus_knot_point(p: f32, q: f32, major: f32, amplitude: f32, s: f32) -> (Vec3, Vec3) {
    let (sin_ps, cos_ps) = (p * s).sin_cos();
    let (sin_qs, cos_qs) = (q * s).sin_cos();
    let radius = major + amplitude * cos_qs;
    let point = Vec3::new(radius * cos_ps, amplitude * sin_qs, radius * sin_ps);
    let tangent = Vec3::new(
        -amplitude * q * sin_qs * cos_ps - radius * p * sin_ps,
        amplitude * q * cos_qs,
        -amplitude * q * sin_qs * sin_ps + radius * p * cos_ps,
    );
    (point, tangent)
}

/// A tube of radius `tube` along a `(p, q)` torus knot (see
/// [`torus_knot_point`]). The tube's frame is parallel-transported along the
/// curve, and the leftover twist at the end is spread evenly, so the tube
/// neither spins nor shows a seam where it closes.
pub fn torus_knot(
    p: u32,
    q: u32,
    major: f32,
    amplitude: f32,
    tube: f32,
    curve_segments: usize,
    tube_segments: usize,
) -> Model {
    let n = curve_segments.max(3);
    let (p, q) = (p as f32, q as f32);
    let samples: Vec<(Vec3, Vec3)> = (0..n)
        .map(|j| {
            let (point, tangent) =
                torus_knot_point(p, q, major, amplitude, turn_angle(j as f32 / n as f32));
            (point, tangent.normalize())
        })
        .collect();

    // Parallel transport: remove each step's tangential component.
    let start_normal = samples[0]
        .1
        .cross(Vec3::Y)
        .try_normalize()
        .unwrap_or(Vec3::X);
    let mut normals = vec![start_normal];
    for (_, tangent) in &samples[1..] {
        let previous = *normals.last().unwrap();
        normals.push((previous - previous.dot(*tangent) * *tangent).normalize());
    }
    // Carrying the last frame round to the start leaves a twist; undo it
    // gradually along the curve.
    let tangent0 = samples[0].1;
    let last = normals[n - 1];
    let arrived = (last - last.dot(tangent0) * tangent0).normalize();
    let twist = arrived
        .cross(start_normal)
        .dot(tangent0)
        .atan2(arrived.dot(start_normal));
    let frames: Vec<(Vec3, Vec3, Vec3)> = samples
        .iter()
        .zip(&normals)
        .enumerate()
        .map(|(j, (&(point, tangent), &normal))| {
            let (sin, cos) = (twist * j as f32 / n as f32).sin_cos();
            let normal = normal * cos + tangent.cross(normal) * sin;
            (point, normal, tangent.cross(normal))
        })
        .collect();

    grid_surface(tube_segments, n, |u, v| {
        let (point, normal, binormal) = frames[(v * n as f32).round() as usize % n];
        let (sin_phi, cos_phi) = turn(u);
        let direction = normal * cos_phi + binormal * sin_phi;
        (point + direction * tube, direction)
    })
}

/// Build a mesh from any parametric surface `f(u, v)` over `[0, 1]^2`, with
/// normals from central differences of `f` (one-sided at the borders). Use it
/// for shapes whose normals have no simple closed form; `du x dv` must point
/// outward.
pub fn parametric(nu: usize, nv: usize, f: impl Fn(f32, f32) -> Vec3) -> Model {
    const H: f32 = 1e-3;
    let normal_at = |u: f32, v: f32| {
        let du = f((u + H).min(1.0), v) - f((u - H).max(0.0), v);
        let dv = f(u, (v + H).min(1.0)) - f(u, (v - H).max(0.0));
        du.cross(dv).try_normalize()
    };
    grid_surface(nu, nv, |u, v| {
        // At a pole du vanishes; take the normal from just inside instead.
        let inward = if v < 0.5 { v + 4.0 * H } else { v - 4.0 * H };
        let normal = normal_at(u, v)
            .or_else(|| normal_at(u, inward))
            .unwrap_or(Vec3::Y);
        (f(u, v), normal)
    })
}

/// Gielis' superformula, `r(a) = (|cos(m a / 4)|^n2 + |sin(m a / 4)|^n3)^(-1 / n1)`.
/// With every exponent at 2, or with `m = 0`, it is the unit circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Superformula {
    pub m: f32,
    pub n1: f32,
    pub n2: f32,
    pub n3: f32,
}

impl Superformula {
    /// The unit circle (and, as a [`supershape`], the unit sphere).
    pub const CIRCLE: Superformula = Superformula {
        m: 0.0,
        n1: 2.0,
        n2: 2.0,
        n3: 2.0,
    };

    pub fn radius(self, angle: f32) -> f32 {
        let a = self.m * angle / 4.0;
        let sum = a.cos().abs().powf(self.n2) + a.sin().abs().powf(self.n3);
        sum.max(1e-6).powf(-1.0 / self.n1)
    }

    /// Linear blend between two superformulas, for morphing.
    pub fn lerp(self, other: Superformula, t: f32) -> Superformula {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Superformula {
            m: mix(self.m, other.m),
            n1: mix(self.n1, other.n1),
            n2: mix(self.n2, other.n2),
            n3: mix(self.n3, other.n3),
        }
    }
}

/// A 3D supershape: `longitude` shapes the horizontal cross-section and
/// `latitude` the profile from pole to pole. Two [`Superformula::CIRCLE`]s give
/// a sphere of radius `scale`.
pub fn supershape(
    longitude: Superformula,
    latitude: Superformula,
    scale: f32,
    segments: usize,
    rings: usize,
) -> Model {
    parametric(segments, rings, |u, v| {
        // Longitude runs from -pi to pi; latitude from the north pole south.
        let (sin_u, cos_u) = turn(u);
        let (sin_theta, cos_theta) = (-sin_u, -cos_u);
        let theta = if u >= 1.0 { -PI } else { u * TAU - PI };
        let (cos_phi, sin_phi) = half_turn(v);
        let phi = PI / 2.0 - v.clamp(0.0, 1.0) * PI;
        let r1 = longitude.radius(theta);
        let r2 = latitude.radius(phi);
        scale
            * Vec3::new(
                r1 * cos_theta * r2 * cos_phi,
                r2 * sin_phi,
                r1 * sin_theta * r2 * cos_phi,
            )
    })
}

/// A torus whose tube radius ripples into `lobes` ridges that spiral `twist`
/// times around the ring; animating `phase` makes the ridges flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwistedTorus {
    /// Distance from the centre to the tube's centre line.
    pub major: f32,
    /// Mean tube radius.
    pub minor: f32,
    pub lobes: u32,
    pub twist: u32,
    /// Ripple height as a fraction of `minor`.
    pub amplitude: f32,
    /// Ripple phase in radians.
    pub phase: f32,
}

/// Mesh a [`TwistedTorus`] with `ring_segments` around the ring and
/// `tube_segments` around the tube.
pub fn twisted_torus(shape: TwistedTorus, ring_segments: usize, tube_segments: usize) -> Model {
    let TwistedTorus {
        major,
        minor,
        lobes,
        twist,
        amplitude,
        phase,
    } = shape;
    parametric(tube_segments, ring_segments, |u, v| {
        let (sin_phi, cos_phi) = turn(u);
        let (sin_theta, cos_theta) = turn(v);
        let ripple = (lobes as f32 * turn_angle(u) + twist as f32 * turn_angle(v) + phase).cos();
        let radius = minor * (1.0 + amplitude * ripple);
        let ring = Vec3::new(major * cos_theta, 0.0, major * sin_theta);
        ring + radius * Vec3::new(cos_phi * cos_theta, sin_phi, cos_phi * sin_theta)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        render_scene, CullMode, Material, RenderMode, RenderOptions, RenderState, SceneObject,
    };
    use glam::Mat4;
    use std::collections::{HashMap, HashSet};

    /// Unit normals that agree with the faces they belong to.
    fn assert_normals_valid(model: &Model, name: &str) {
        for normal in &model.normals {
            assert!(
                (normal.length() - 1.0).abs() < 1e-4,
                "{name}: normal {normal:?}"
            );
        }
        let mut dots = Vec::new();
        for (face_idx, face) in model.faces.iter().enumerate() {
            let face_normal = model.face_normal(face_idx);
            let mean: Vec3 = face.iter().map(|fv| model.normals[fv.vn.unwrap()]).sum();
            assert!(
                mean.dot(face_normal) > 0.0,
                "{name}: face {face_idx} disagrees"
            );
            dots.extend(
                face.iter()
                    .map(|fv| model.normals[fv.vn.unwrap()].dot(face_normal)),
            );
        }
        dots.sort_by(f32::total_cmp);
        let median = dots[dots.len() / 2];
        // A coarse tessellation leaves vertex normals ~12 degrees off the flat
        // faces; a flipped or wrong normal is far below this.
        assert!(median > 0.95, "{name}: median normal agreement {median}");
    }

    /// Divergence-theorem volume; positive when faces wind outward.
    fn signed_volume(model: &Model) -> f32 {
        model
            .faces
            .iter()
            .map(|face| {
                let [a, b, c] = face.map(|fv| model.vertices[fv.v]);
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    /// After welding vertices by exact position, every directed edge must
    /// appear exactly once and its reverse must exist: the surface is closed
    /// and consistently wound.
    fn assert_watertight(model: &Model, name: &str) {
        let mut ids: HashMap<[u32; 3], usize> = HashMap::new();
        let welded: Vec<usize> = model
            .vertices
            .iter()
            .map(|v| {
                let next = ids.len();
                // `+ 0.0` maps -0.0 to 0.0: same point, different bits.
                *ids.entry(v.to_array().map(|c| (c + 0.0).to_bits()))
                    .or_insert(next)
            })
            .collect();
        let mut edges = HashSet::new();
        for face in &model.faces {
            let [a, b, c] = face.map(|fv| welded[fv.v]);
            if a == b || b == c || a == c {
                continue;
            }
            for edge in [(a, b), (b, c), (c, a)] {
                assert!(edges.insert(edge), "{name}: directed edge {edge:?} repeats");
            }
        }
        for &(a, b) in &edges {
            assert!(edges.contains(&(b, a)), "{name}: edge {a}->{b} has no twin");
        }
    }

    fn closed_shapes() -> Vec<(&'static str, Model)> {
        vec![
            ("sphere", uv_sphere(1.0, 24, 12)),
            ("torus", torus(1.0, 0.3, 32, 16)),
            ("knot", torus_knot(2, 3, 1.0, 0.4, 0.15, 128, 12)),
            (
                "supershape",
                supershape(Superformula::CIRCLE, Superformula::CIRCLE, 1.0, 24, 12),
            ),
            (
                "star",
                supershape(
                    Superformula {
                        m: 5.0,
                        n1: 0.6,
                        n2: 1.2,
                        n3: 1.2,
                    },
                    Superformula {
                        m: 3.0,
                        n1: 1.0,
                        n2: 1.0,
                        n3: 1.0,
                    },
                    1.0,
                    48,
                    24,
                ),
            ),
            (
                "twisted torus",
                twisted_torus(
                    TwistedTorus {
                        major: 1.0,
                        minor: 0.35,
                        lobes: 5,
                        twist: 2,
                        amplitude: 0.25,
                        phase: 0.7,
                    },
                    96,
                    72,
                ),
            ),
        ]
    }

    #[test]
    fn closed_shapes_are_watertight_and_wind_outward() {
        for (name, model) in closed_shapes() {
            assert_watertight(&model, name);
            assert!(signed_volume(&model) > 0.0, "{name}: faces wind inward");
        }
    }

    #[test]
    fn generated_normals_are_unit_and_match_their_faces() {
        let mut shapes = closed_shapes();
        shapes.push(("grid", ground_grid(2.0, 4)));
        shapes.push(("disk", ground_disk(2.0, 4, 24)));
        for (name, model) in shapes {
            assert_normals_valid(&model, name);
        }
    }

    #[test]
    fn grids_have_the_expected_counts() {
        let grid = ground_grid(1.0, 4);
        assert_eq!((grid.vertices.len(), grid.faces.len()), (25, 32));
        let sphere = uv_sphere(1.0, 16, 8);
        // Each pole row loses one triangle per cell.
        assert_eq!(
            (sphere.vertices.len(), sphere.faces.len()),
            (17 * 9, 2 * 16 * 7)
        );
        let torus = torus(1.0, 0.3, 24, 12);
        assert_eq!(torus.faces.len(), 2 * 24 * 12);
        let disk = ground_disk(1.0, 3, 10);
        // The centre ring loses one triangle per cell.
        assert_eq!(disk.faces.len(), 2 * 10 * 3 - 10);
    }

    #[test]
    fn volumes_approach_the_analytic_shapes() {
        let sphere = signed_volume(&uv_sphere(1.0, 96, 48));
        let expected = 4.0 / 3.0 * PI;
        assert!(
            (sphere / expected - 1.0).abs() < 0.02,
            "sphere {sphere} vs {expected}"
        );
        let torus = signed_volume(&torus(1.0, 0.3, 96, 48));
        let expected = 2.0 * PI * PI * 1.0 * 0.3 * 0.3;
        assert!(
            (torus / expected - 1.0).abs() < 0.02,
            "torus {torus} vs {expected}"
        );
    }

    #[test]
    fn floors_face_up() {
        for model in [ground_grid(2.0, 3), ground_disk(2.0, 3, 16)] {
            for face_idx in 0..model.faces.len() {
                assert!((model.face_normal(face_idx) - Vec3::Y).length() < 1e-5);
            }
        }
    }

    #[test]
    fn supershape_of_two_circles_is_a_sphere() {
        let sphere = supershape(Superformula::CIRCLE, Superformula::CIRCLE, 1.5, 32, 16);
        for v in &sphere.vertices {
            assert!((v.length() - 1.5).abs() < 1e-5, "{v:?}");
        }
        let blended = Superformula::CIRCLE.lerp(
            Superformula {
                m: 6.0,
                n1: 1.0,
                n2: 1.0,
                n3: 1.0,
            },
            0.0,
        );
        assert_eq!(blended, Superformula::CIRCLE);
    }

    #[test]
    fn back_face_culling_does_not_change_closed_shapes() {
        for (name, model) in closed_shapes() {
            let render = |cull_mode: CullMode| {
                let options = RenderOptions {
                    width: 96,
                    height: 96,
                    mode: RenderMode::Flat,
                    camera_eye: Some(Vec3::new(1.5, 2.0, 4.5)),
                    render_state: RenderState {
                        cull_mode,
                        ..RenderState::default()
                    },
                    ..RenderOptions::default()
                };
                let object = SceneObject {
                    model: &model,
                    transform: Mat4::IDENTITY,
                    material: Material::default(),
                };
                render_scene(&[object], &options, 2.0).image
            };
            let off = render(CullMode::Off);
            let back = render(CullMode::Back);
            let front = render(CullMode::Front);
            let differing = off
                .pixels()
                .zip(back.pixels())
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                differing * 1000 <= (off.width() * off.height()) as usize,
                "{name}: {differing} pixels changed with back-face culling"
            );
            assert_ne!(
                off.as_raw(),
                front.as_raw(),
                "{name}: front culling kept the outside"
            );
        }
    }
}
