//! A vertical (9:16) demo reel rendered entirely by octo-render's CPU
//! rasterizer: no GPU, no screen capture.
//!
//! It writes numbered PNG frames plus a caption timeline that
//! `scripts/make-demo.sh` turns into an MP4 and a README GIF:
//!
//! ```text
//! cargo run --release --example demo -- --out target/demo
//! cargo run --release --example demo -- --preview
//! ```
//!
//! Flags: `--out DIR`, `--width`, `--height`, `--fps`, `--ssaa N` (N x N
//! supersampling), `--threads N`, `--only SEGMENT`, `--preview` (270x480,
//! 15 fps, no supersampling, written to `target/demo-preview`).

use glam::{Mat4, Vec3};
use image::{imageops, Rgb, RgbImage};
use renderer::procedural::{self, Superformula, TwistedTorus};
use renderer::{
    normalized_transform, render_scene, CullMode, Material, Model, RenderMode, RenderOptions,
    RenderState, SceneObject, ShadowFilter, ShadowOptions,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// Every object stays inside this radius of the origin; the shadow camera is
/// sized to it.
const SCENE_RADIUS: f32 = 3.2;
const FLOOR_Y: f32 = -0.9;
const BACKGROUND: Rgb<u8> = Rgb([14, 16, 28]);
/// When the closing card appears; `scripts/make-demo.sh` draws its text.
const END_CARD_START: f32 = 18.6;

// --- Settings -----------------------------------------------------------------

struct Settings {
    out: PathBuf,
    width: u32,
    height: u32,
    fps: f32,
    ssaa: u32,
    threads: usize,
    shadow_size: u32,
    /// Scales procedural mesh resolution (lower in preview).
    detail: f32,
    only: Option<String>,
}

fn parse_settings() -> Result<Settings, String> {
    let mut settings = Settings {
        out: PathBuf::from("target/demo"),
        width: 1080,
        height: 1920,
        fps: 30.0,
        ssaa: 2,
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        shadow_size: 2048,
        detail: 1.0,
        only: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("missing value for {name}"))
        };
        let number = |text: String, name: &str| {
            text.parse::<f32>()
                .map_err(|err| format!("invalid {name} '{text}': {err}"))
        };
        match arg.as_str() {
            "--out" => settings.out = PathBuf::from(value("--out")?),
            "--width" => settings.width = number(value("--width")?, "--width")? as u32,
            "--height" => settings.height = number(value("--height")?, "--height")? as u32,
            "--fps" => settings.fps = number(value("--fps")?, "--fps")?,
            "--ssaa" => settings.ssaa = (number(value("--ssaa")?, "--ssaa")? as u32).max(1),
            "--threads" => {
                settings.threads = (number(value("--threads")?, "--threads")? as usize).max(1)
            }
            "--only" => settings.only = Some(value("--only")?),
            "--preview" => {
                settings.out = PathBuf::from("target/demo-preview");
                settings.width = 270;
                settings.height = 480;
                settings.fps = 15.0;
                settings.ssaa = 1;
                settings.shadow_size = 512;
                settings.detail = 0.5;
            }
            "--help" | "-h" => {
                println!("usage: demo [--out DIR] [--width W] [--height H] [--fps F] [--ssaa N] [--threads N] [--only SEGMENT] [--preview]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if settings.width < 16 || settings.height < 16 || settings.fps <= 0.0 {
        return Err("--width/--height must be at least 16 and --fps positive".into());
    }
    Ok(settings)
}

// --- Timeline -------------------------------------------------------------------

/// What a segment shows: a render mode, or one of the debug maps.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Mode(RenderMode),
    Depth,
    ShadowMap,
    AmbientOcclusion,
}

/// Which objects are on stage.
#[derive(Clone, Copy, PartialEq)]
enum Cast {
    /// Teapot, floating knot, sphere, and morphing supershape.
    Hero,
    /// Shapes built from math: knot, twisted torus, supershape.
    Generative,
    /// Everything, plus the cow, teddy, and pumpkin.
    Finale,
}

/// Camera on an orbit around `target`: angles in degrees.
#[derive(Clone, Copy)]
struct Orbit {
    azimuth: f32,
    elevation: f32,
    distance: f32,
    target: Vec3,
}

impl Orbit {
    const fn new(azimuth: f32, elevation: f32, distance: f32, target_y: f32) -> Orbit {
        Orbit {
            azimuth,
            elevation,
            distance,
            target: Vec3::new(0.0, target_y, 0.0),
        }
    }

    fn lerp(self, to: Orbit, k: f32) -> Orbit {
        let mix = |a: f32, b: f32| a + (b - a) * k;
        Orbit {
            azimuth: mix(self.azimuth, to.azimuth),
            elevation: mix(self.elevation, to.elevation),
            distance: mix(self.distance, to.distance),
            target: self.target.lerp(to.target, k),
        }
    }

    fn eye(self) -> Vec3 {
        self.target + self.distance * direction(self.azimuth, self.elevation)
    }
}

/// Unit vector at an azimuth (around +Y, from +Z toward +X) and elevation, in
/// degrees.
fn direction(azimuth: f32, elevation: f32) -> Vec3 {
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    Vec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos())
}

#[derive(Clone, Copy)]
struct Segment {
    name: &'static str,
    start: f32,
    duration: f32,
    view: View,
    cast: Cast,
    camera: (Orbit, Orbit),
    /// Light (azimuth, elevation) in degrees at the start and end.
    light: ((f32, f32), (f32, f32)),
    specular: bool,
    cull: CullMode,
    near: f32,
    fov: f32,
}

impl Segment {
    fn end(&self) -> f32 {
        self.start + self.duration
    }
}

/// The opening pose; the finale returns to it so the clip loops.
const OPENING: Orbit = Orbit::new(30.0, 14.0, 6.6, 0.75);
const OPENING_LIGHT: (f32, f32) = (130.0, 48.0);

fn timeline() -> Vec<Segment> {
    let segment = |name, start, duration, view, cast, camera, light| Segment {
        name,
        start,
        duration,
        view,
        cast,
        camera,
        light,
        specular: false,
        cull: CullMode::Off,
        near: 0.05,
        fov: 50.0,
    };
    let mode = View::Mode;
    let mut segments = vec![
        Segment {
            specular: true,
            ..segment(
                "hook",
                0.0,
                2.0,
                mode(RenderMode::Shadow),
                Cast::Hero,
                (OPENING, Orbit::new(12.0, 12.0, 4.9, 0.7)),
                (OPENING_LIGHT, (60.0, 48.0)),
            )
        },
        Segment {
            cull: CullMode::Back,
            ..segment(
                "wireframe",
                2.0,
                2.0,
                mode(RenderMode::Wireframe),
                Cast::Hero,
                (
                    Orbit::new(-40.0, 38.0, 5.6, 0.75),
                    Orbit::new(-15.0, 34.0, 5.4, 0.75),
                ),
                ((40.0, 55.0), (40.0, 55.0)),
            )
        },
        segment(
            "flat",
            4.0,
            1.5,
            mode(RenderMode::Flat),
            Cast::Hero,
            (
                Orbit::new(55.0, 8.0, 5.2, 0.7),
                Orbit::new(75.0, 10.0, 5.0, 0.7),
            ),
            ((40.0, 55.0), (40.0, 55.0)),
        ),
        segment(
            "gouraud",
            5.5,
            1.5,
            mode(RenderMode::Gouraud),
            Cast::Hero,
            (
                Orbit::new(75.0, 10.0, 5.0, 0.7),
                Orbit::new(110.0, 15.0, 5.0, 0.7),
            ),
            ((40.0, 55.0), (40.0, 55.0)),
        ),
        Segment {
            specular: true,
            ..segment(
                "specular",
                7.0,
                1.5,
                mode(RenderMode::Gouraud),
                Cast::Hero,
                (
                    Orbit::new(110.0, 15.0, 5.0, 0.7),
                    Orbit::new(125.0, 16.0, 3.8, 0.5),
                ),
                ((20.0, 40.0), (80.0, 40.0)),
            )
        },
        Segment {
            specular: true,
            ..segment(
                "shadows",
                8.5,
                2.5,
                mode(RenderMode::Shadow),
                Cast::Hero,
                (
                    Orbit::new(-20.0, 30.0, 6.4, 0.8),
                    Orbit::new(8.0, 32.0, 6.2, 0.8),
                ),
                ((-70.0, 36.0), (70.0, 36.0)),
            )
        },
        Segment {
            specular: true,
            ..segment(
                "generative",
                11.0,
                1.5,
                mode(RenderMode::Shadow),
                Cast::Generative,
                (
                    Orbit::new(30.0, 22.0, 5.6, 0.75),
                    Orbit::new(58.0, 20.0, 5.2, 0.75),
                ),
                ((50.0, 55.0), (50.0, 55.0)),
            )
        },
        segment(
            "normals",
            12.5,
            1.5,
            mode(RenderMode::NormalDebug),
            Cast::Generative,
            (
                Orbit::new(58.0, 20.0, 5.2, 0.75),
                Orbit::new(92.0, 25.0, 5.0, 0.75),
            ),
            ((50.0, 55.0), (50.0, 55.0)),
        ),
    ];
    // Three quick debug-view cuts; the orbit continues across them.
    let debug_cuts = [
        ("depth", View::Depth),
        ("light-view", View::ShadowMap),
        ("ambient-occlusion", View::AmbientOcclusion),
    ];
    let cut = 2.0 / 3.0;
    for (i, (name, view)) in debug_cuts.into_iter().enumerate() {
        let at = |k: f32| Orbit::new(95.0 + 30.0 * k, 25.0, 5.6, 0.75);
        let (k0, k1) = (i as f32 / 3.0, (i + 1) as f32 / 3.0);
        segments.push(segment(
            name,
            14.0 + i as f32 * cut,
            cut,
            view,
            Cast::Hero,
            (at(k0), at(k1)),
            ((60.0, 50.0), (60.0, 50.0)),
        ));
    }
    let knot = |distance: f32| Orbit {
        target: KNOT_GENERATIVE,
        ..Orbit::new(35.0, 12.0, distance, 0.0)
    };
    segments.push(Segment {
        near: 1.0,
        ..segment(
            "clipping",
            16.0,
            2.0,
            mode(RenderMode::Gouraud),
            Cast::Generative,
            (knot(4.0), knot(1.15)),
            ((40.0, 50.0), (40.0, 50.0)),
        )
    });
    segments.push(Segment {
        specular: true,
        ..segment(
            "finale",
            18.0,
            3.0,
            mode(RenderMode::Shadow),
            Cast::Finale,
            (Orbit::new(150.0, 20.0, 5.6, 0.7), OPENING),
            ((60.0, 48.0), OPENING_LIGHT),
        )
    });
    segments
}

/// On-screen captions: (start, end, text). `\n` breaks lines.
const CAPTIONS: &[(f32, f32, &str)] = &[
    (0.1, 1.0, "No GPU.\nNo OpenGL."),
    (1.0, 1.95, "Just Rust\non a CPU"),
    (2.05, 3.95, "1 · Triangles"),
    (4.05, 5.45, "2 · Flat shading\n+ z-buffer"),
    (5.55, 6.95, "3 · Smooth\nshading"),
    (7.05, 8.45, "4 · Specular\nhighlights"),
    (8.55, 10.95, "5 · Shadow\nmapping"),
    (11.05, 12.45, "6 · Geometry\nfrom math"),
    (12.55, 13.95, "Normals\nas color"),
    (14.0, 14.62, "Depth buffer"),
    (14.67, 15.28, "The light's\nview"),
    (15.33, 15.95, "Ambient\nocclusion"),
    (16.05, 17.95, "7 · Near-plane\nclipping"),
];

// --- Assets and staging -----------------------------------------------------------

struct Assets {
    teapot: Model,
    cow: Model,
    teddy: Model,
    pumpkin: Model,
    floor: Model,
    sphere: Model,
    knot: Model,
}

impl Assets {
    fn load(detail: f32) -> Result<Assets, String> {
        let n = |count: f32| ((count * detail) as usize).max(8);
        Ok(Assets {
            teapot: Model::load("resources/teapot.obj")?,
            cow: Model::load("resources/cow-nonormals.obj")?,
            teddy: Model::load("resources/teddy.obj")?,
            pumpkin: Model::load("resources/pumpkin_tall_10k.obj")?,
            floor: procedural::ground_disk(2.9, n(24.0), n(160.0)),
            sphere: procedural::uv_sphere(1.0, n(96.0), n(48.0)),
            knot: procedural::torus_knot(2, 3, 0.9, 0.35, 0.16, n(480.0), n(28.0)),
        })
    }
}

/// Meshes that change every frame.
struct Generated {
    supershape: Model,
    twisted: Option<Model>,
}

/// The supershape cycles sphere -> star -> flower -> sphere.
fn supershape_at(t: f32, detail: f32) -> Model {
    let keys = [
        (Superformula::CIRCLE, Superformula::CIRCLE),
        (
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
        ),
        (
            Superformula {
                m: 7.0,
                n1: 0.35,
                n2: 1.6,
                n3: 1.6,
            },
            Superformula {
                m: 2.0,
                n1: 1.0,
                n2: 1.4,
                n3: 1.4,
            },
        ),
    ];
    let phase = (t * 0.45).rem_euclid(keys.len() as f32);
    let index = phase as usize % keys.len();
    let next = (index + 1) % keys.len();
    let k = smoothstep(phase.fract());
    let n = |count: f32| ((count * detail) as usize).max(8);
    procedural::supershape(
        keys[index].0.lerp(keys[next].0, k),
        keys[index].1.lerp(keys[next].1, k),
        1.0,
        n(160.0),
        n(80.0),
    )
}

impl Generated {
    fn at(cast: Cast, t: f32, detail: f32) -> Generated {
        let n = |count: f32| ((count * detail) as usize).max(8);
        Generated {
            supershape: supershape_at(t, detail),
            twisted: (cast == Cast::Generative).then(|| {
                procedural::twisted_torus(
                    TwistedTorus {
                        major: 0.9,
                        minor: 0.32,
                        lobes: 5,
                        twist: 2,
                        amplitude: 0.25,
                        phase: t * 3.0,
                    },
                    n(220.0),
                    n(110.0),
                )
            }),
        }
    }
}

fn material(rgb: [u8; 3], specular: bool) -> Material {
    Material {
        base_color: Rgb(rgb),
        ambient: 0.14,
        diffuse: 0.95,
        specular: if specular { 0.45 } else { 0.0 },
        shininess: 40.0,
        alpha: 1.0,
    }
}

/// Scale `model` to `radius`, spin it about its own vertical axis, and move it
/// to `position`; with `on_floor`, its lowest point rests on the floor instead.
fn placed(model: &Model, radius: f32, position: Vec3, spin: f32, on_floor: bool) -> Mat4 {
    let normalize = normalized_transform(model, radius);
    let mut position = position;
    if on_floor {
        let lowest = model
            .vertices
            .iter()
            .map(|v| normalize.transform_point3(*v).y)
            .fold(f32::INFINITY, f32::min);
        position.y = FLOOR_Y - lowest;
    }
    Mat4::from_translation(position) * Mat4::from_rotation_y(spin) * normalize
}

/// Where the knot floats in the generative cast; the clipping dolly aims here.
const KNOT_GENERATIVE: Vec3 = Vec3::new(0.0, 0.8, 0.0);

fn scene<'a>(
    cast: Cast,
    t: f32,
    specular: bool,
    with_floor: bool,
    assets: &'a Assets,
    generated: &'a Generated,
) -> Vec<SceneObject<'a>> {
    let object = |model, transform, rgb, shiny: bool| SceneObject {
        model,
        transform,
        material: material(rgb, specular && shiny),
    };
    let floor = SceneObject {
        model: &assets.floor,
        transform: Mat4::from_translation(Vec3::new(0.0, FLOOR_Y, 0.0)),
        material: Material {
            base_color: Rgb([58, 64, 96]),
            ambient: 0.22,
            diffuse: 0.9,
            specular: 0.0,
            shininess: 1.0,
            alpha: 1.0,
        },
    };
    let knot_spin = Mat4::from_rotation_y(0.9 * t) * Mat4::from_rotation_x(0.45);
    let bob = 0.08 * (2.0 * t).sin();
    let supershape = |position, size| {
        object(
            &generated.supershape,
            placed(&generated.supershape, size, position, 0.6 * t, true),
            [150, 110, 245],
            true,
        )
    };
    let mut objects: Vec<SceneObject> = with_floor.then_some(floor).into_iter().collect();
    match cast {
        Cast::Hero | Cast::Finale => {
            objects.push(object(
                &assets.teapot,
                placed(
                    &assets.teapot,
                    0.75,
                    Vec3::new(-0.45, 0.0, 0.45),
                    0.5 + 0.2 * t,
                    true,
                ),
                [240, 120, 90],
                true,
            ));
            objects.push(object(
                &assets.knot,
                Mat4::from_translation(Vec3::new(0.0, 0.95 + bob, -0.15))
                    * knot_spin
                    * Mat4::from_scale(Vec3::splat(0.45)),
                [60, 205, 190],
                true,
            ));
            objects.push(object(
                &assets.sphere,
                Mat4::from_translation(Vec3::new(0.55, FLOOR_Y + 0.34, 0.65))
                    * Mat4::from_scale(Vec3::splat(0.34)),
                [245, 190, 80],
                true,
            ));
            objects.push(supershape(Vec3::new(0.5, 0.0, -0.5), 0.5));
            if cast == Cast::Finale {
                objects.push(object(
                    &assets.cow,
                    placed(&assets.cow, 0.6, Vec3::new(-0.85, 0.0, -0.65), 0.9, true),
                    [232, 232, 238],
                    true,
                ));
                objects.push(object(
                    &assets.teddy,
                    placed(&assets.teddy, 0.42, Vec3::new(0.95, 0.0, -0.15), -0.6, true),
                    [205, 145, 85],
                    true,
                ));
                objects.push(object(
                    &assets.pumpkin,
                    placed(&assets.pumpkin, 0.35, Vec3::new(0.05, 0.0, 1.05), 0.3, true),
                    [245, 140, 40],
                    true,
                ));
            }
        }
        Cast::Generative => {
            objects.push(object(
                &assets.knot,
                Mat4::from_translation(KNOT_GENERATIVE + Vec3::new(0.0, bob, 0.0))
                    * knot_spin
                    * Mat4::from_scale(Vec3::splat(0.6)),
                [60, 205, 190],
                true,
            ));
            if let Some(twisted) = &generated.twisted {
                objects.push(object(
                    twisted,
                    placed(twisted, 0.5, Vec3::new(-0.55, 0.0, 0.55), 0.4 * t, true),
                    [245, 90, 165],
                    true,
                ));
            }
            objects.push(supershape(Vec3::new(0.55, 0.0, 0.45), 0.5));
        }
    }
    objects
}

// --- Rendering ------------------------------------------------------------------

fn ease_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 {
        4.0 * x * x * x
    } else {
        1.0 - (-2.0 * x + 2.0).powi(3) / 2.0
    }
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn segment_at(segments: &[Segment], t: f32) -> &Segment {
    segments
        .iter()
        .find(|segment| t < segment.end())
        .unwrap_or_else(|| segments.last().unwrap())
}

fn render_frame(t: f32, segment: &Segment, settings: &Settings, assets: &Assets) -> RgbImage {
    let k = ease_in_out((t - segment.start) / segment.duration);
    let orbit = segment.camera.0.lerp(segment.camera.1, k);
    let ((az0, el0), (az1, el1)) = segment.light;
    let light = direction(az0 + (az1 - az0) * k, el0 + (el1 - el0) * k);

    // Wireframe lines and the SSAO kernel are sized in pixels, so those views
    // render at output resolution.
    let ssaa = match segment.view {
        View::Mode(RenderMode::Wireframe) | View::AmbientOcclusion => 1,
        _ => settings.ssaa,
    };
    let (width, height) = (settings.width * ssaa, settings.height * ssaa);
    let generated = Generated::at(segment.cast, t, settings.detail);
    // The dense floor mesh swamps wireframe, and its flat normal is a slab of
    // green in the normal view; both views show the objects alone.
    let with_floor = !matches!(
        segment.view,
        View::Mode(RenderMode::Wireframe) | View::Mode(RenderMode::NormalDebug)
    );
    let mut objects = scene(
        segment.cast,
        t,
        segment.specular,
        with_floor,
        assets,
        &generated,
    );
    if segment.near > 0.5 {
        // Let the clipped interior read: it faces away from the light.
        for object in &mut objects {
            object.material.ambient = 0.3;
        }
    }

    let mode = match segment.view {
        View::Mode(mode) => mode,
        View::Depth => RenderMode::Flat,
        View::ShadowMap => RenderMode::Shadow,
        View::AmbientOcclusion => RenderMode::Ssao,
    };
    let options = RenderOptions {
        width,
        height,
        mode,
        camera_eye: Some(orbit.eye()),
        camera_target: Some(orbit.target),
        fov_y_degrees: segment.fov,
        near_plane: segment.near,
        light_dir: light,
        debug_outputs: !matches!(segment.view, View::Mode(_)),
        shadow: ShadowOptions {
            size: settings.shadow_size,
            // Bias is in depth units: ~0.05 scene units at this scene radius.
            bias: 1.0,
            filter: ShadowFilter::Pcf,
        },
        render_state: RenderState {
            clear_color: BACKGROUND,
            cull_mode: segment.cull,
            ..RenderState::default()
        },
        ..RenderOptions::default()
    };
    let output = render_scene(&objects, &options, SCENE_RADIUS);
    let image = match segment.view {
        View::Mode(_) => output.image,
        View::Depth => output.debug.depth.unwrap(),
        View::AmbientOcclusion => {
            // The AO map is white wherever nothing was drawn; show the scene
            // background there instead. Debug maps are flipped like the image,
            // the z-buffer is not.
            let mut ao = output.debug.ao.unwrap();
            for (x, y, pixel) in ao.enumerate_pixels_mut() {
                let row = (height - 1 - y) as usize;
                if !output.zbuffer[row * width as usize + x as usize].is_finite() {
                    *pixel = BACKGROUND;
                }
            }
            ao
        }
        View::ShadowMap => letterbox(&output.debug.shadow.unwrap(), width, height),
    };
    downsample(&image, ssaa)
}

/// Fit a square debug map across the frame's width, centred vertically.
fn letterbox(square: &RgbImage, width: u32, height: u32) -> RgbImage {
    let side = width.min(height);
    let scaled = imageops::resize(square, side, side, imageops::FilterType::Triangle);
    let mut frame = RgbImage::from_pixel(width, height, Rgb([0, 0, 0]));
    imageops::overlay(
        &mut frame,
        &scaled,
        ((width - side) / 2) as i64,
        ((height - side) / 2) as i64,
    );
    frame
}

/// Average each `factor x factor` block (box-filter supersampling).
fn downsample(image: &RgbImage, factor: u32) -> RgbImage {
    if factor == 1 {
        return image.clone();
    }
    let (width, height) = (image.width() / factor, image.height() / factor);
    let samples = factor * factor;
    RgbImage::from_fn(width, height, |x, y| {
        let mut sum = [0u32; 3];
        for dy in 0..factor {
            for dx in 0..factor {
                let pixel = image.get_pixel(x * factor + dx, y * factor + dy);
                for (total, channel) in sum.iter_mut().zip(pixel.0) {
                    *total += channel as u32;
                }
            }
        }
        Rgb(sum.map(|total| ((total + samples / 2) / samples) as u8))
    })
}

// --- Output -------------------------------------------------------------------------

fn write_timeline(settings: &Settings, duration: f32, frames: usize) -> Result<(), String> {
    let io = |err: std::io::Error| err.to_string();
    let captions_dir = settings.out.join("captions");
    fs::create_dir_all(&captions_dir).map_err(io)?;
    let mut tsv = String::new();
    for (i, (start, end, text)) in CAPTIONS.iter().enumerate() {
        let file = format!("captions/{i:02}.txt");
        fs::write(settings.out.join(&file), text).map_err(io)?;
        tsv.push_str(&format!("{start:.3}\t{end:.3}\t{file}\n"));
    }
    fs::write(settings.out.join("timeline.tsv"), tsv).map_err(io)?;
    let meta = format!(
        "WIDTH={}\nHEIGHT={}\nFPS={}\nFRAMES={}\nDURATION={:.3}\nEND_CARD_START={:.3}\n",
        settings.width, settings.height, settings.fps, frames, duration, END_CARD_START
    );
    fs::write(settings.out.join("meta.env"), meta).map_err(io)
}

fn run() -> Result<(), String> {
    let settings = parse_settings()?;
    let segments = timeline();
    let duration = segments.last().unwrap().end();
    let frames = (duration * settings.fps).round() as usize;
    let frames_dir = settings.out.join("frames");
    fs::create_dir_all(&frames_dir).map_err(|err| err.to_string())?;
    write_timeline(&settings, duration, frames)?;
    if settings.only.is_none() {
        // A full run owns the frame directory: clear frames from older runs.
        for entry in fs::read_dir(&frames_dir).map_err(|err| err.to_string())? {
            let path = entry.map_err(|err| err.to_string())?.path();
            if path.extension().is_some_and(|ext| ext == "png") {
                fs::remove_file(path).map_err(|err| err.to_string())?;
            }
        }
    }

    let assets = Assets::load(settings.detail)?;
    let work: Vec<usize> = (0..frames)
        .filter(|&frame| {
            let t = frame as f32 / settings.fps;
            settings
                .only
                .as_deref()
                .is_none_or(|name| segment_at(&segments, t).name == name)
        })
        .collect();
    if work.is_empty() {
        return Err("no frames selected; check --only".into());
    }
    eprintln!(
        "rendering {} frames at {}x{} (ssaa {}) on {} threads -> {}",
        work.len(),
        settings.width,
        settings.height,
        settings.ssaa,
        settings.threads,
        settings.out.display()
    );

    let next = AtomicUsize::new(0);
    let timings: Mutex<Vec<(&str, f32)>> = Mutex::new(Vec::new());
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..settings.threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&frame) = work.get(i) else { break };
                let t = frame as f32 / settings.fps;
                let segment = segment_at(&segments, t);
                let frame_started = Instant::now();
                let image = render_frame(t, segment, &settings, &assets);
                let path = frames_dir.join(format!("frame_{frame:05}.png"));
                if let Err(err) = image.save(&path) {
                    *failure.lock().unwrap() = Some(format!("{}: {err}", path.display()));
                    break;
                }
                let ms = frame_started.elapsed().as_secs_f32() * 1000.0;
                timings.lock().unwrap().push((segment.name, ms));
                if i.is_multiple_of(30) {
                    eprintln!("  frame {frame:>4} ({:<17}) {ms:>7.1} ms", segment.name);
                }
            });
        }
    });
    if let Some(err) = failure.into_inner().unwrap() {
        return Err(err);
    }

    let timings = timings.into_inner().unwrap();
    for segment in &segments {
        let times: Vec<f32> = timings
            .iter()
            .filter(|(name, _)| *name == segment.name)
            .map(|&(_, ms)| ms)
            .collect();
        if !times.is_empty() {
            eprintln!(
                "  {:<17} {:>4} frames, {:>7.1} ms/frame",
                segment.name,
                times.len(),
                times.iter().sum::<f32>() / times.len() as f32
            );
        }
    }
    eprintln!(
        "rendered {} frames in {:.1} s",
        work.len(),
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(2);
    }
}
