use glam::Vec3;
use image::Rgb;
use renderer::{
    render_model, CullMode, DepthState, FillMode, FrontFace, Material, Model, RenderMode,
    RenderOptions, RenderState, ScissorRect, ShadowFilter, ShadowOptions, TextureFilter,
    TextureSampler, TextureWrap,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;
use std::time::Instant;

#[derive(Debug)]
struct Cli {
    model: PathBuf,
    width: u32,
    height: u32,
    shader: RenderMode,
    output: PathBuf,
    camera: Option<Vec3>,
    light: Vec3,
    debug_dir: Option<PathBuf>,
    cull_mode: CullMode,
    front_face: FrontFace,
    depth_test: bool,
    depth_write: bool,
    fill_mode: FillMode,
    scissor: Option<ScissorRect>,
    clear_color: Rgb<u8>,
    clear_depth: f32,
    color_write: bool,
    texture: Option<PathBuf>,
    normal_map: Option<PathBuf>,
    texture_wrap: TextureWrap,
    texture_filter: TextureFilter,
    material: Material,
    shadow: ShadowOptions,
}

impl Default for Cli {
    fn default() -> Self {
        Cli {
            model: PathBuf::from("resources/teapot.obj"),
            width: 1024,
            height: 1024,
            shader: RenderMode::Shadow,
            output: PathBuf::from("renders/teapot-shadow.png"),
            camera: None,
            light: Vec3::new(1.0, 1.0, -1.0),
            debug_dir: None,
            cull_mode: CullMode::Off,
            front_face: FrontFace::Ccw,
            depth_test: true,
            depth_write: true,
            fill_mode: FillMode::Solid,
            scissor: None,
            clear_color: Rgb([18, 20, 24]),
            clear_depth: f32::NEG_INFINITY,
            color_write: true,
            texture: None,
            normal_map: None,
            texture_wrap: TextureWrap::Repeat,
            texture_filter: TextureFilter::Nearest,
            material: Material::default(),
            shadow: ShadowOptions::default(),
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let cli = parse_args(env::args().skip(1))?;

    let model = Model::load(&cli.model)?;
    let texture = match &cli.texture {
        Some(path) => Some(load_texture(path)?),
        None => None,
    };
    let normal_map = match &cli.normal_map {
        Some(path) => Some(load_texture(path)?),
        None => None,
    };
    let options = RenderOptions {
        width: cli.width,
        height: cli.height,
        mode: cli.shader,
        camera_eye: cli.camera,
        light_dir: cli.light,
        debug_outputs: cli.debug_dir.is_some(),
        render_state: RenderState {
            cull_mode: cli.cull_mode,
            front_face: cli.front_face,
            depth: DepthState {
                test_enabled: cli.depth_test,
                write_enabled: cli.depth_write,
            },
            fill_mode: cli.fill_mode,
            scissor: cli.scissor,
            clear_color: cli.clear_color,
            clear_depth: cli.clear_depth,
            color_write: cli.color_write,
        },
        texture,
        normal_map,
        texture_sampler: TextureSampler {
            wrap: cli.texture_wrap,
            filter: cli.texture_filter,
        },
        material: cli.material,
        shadow: cli.shadow,
        near_plane: 0.01,
    };

    let started = Instant::now();
    let output = render_model(&model, &options);
    let elapsed = started.elapsed();

    save_image(&output.image, &cli.output)?;

    if let Some(debug_dir) = &cli.debug_dir {
        fs::create_dir_all(debug_dir).map_err(|err| {
            format!(
                "failed to create debug directory '{}': {}",
                debug_dir.display(),
                err
            )
        })?;
        if let Some(image) = &output.debug.depth {
            save_image(image, &debug_dir.join("depth.png"))?;
        }
        if let Some(image) = &output.debug.shadow {
            save_image(image, &debug_dir.join("shadow-map.png"))?;
        }
        if let Some(image) = &output.debug.normals {
            save_image(image, &debug_dir.join("normals.png"))?;
        }
        if let Some(image) = &output.debug.ao {
            save_image(image, &debug_dir.join("ao.png"))?;
        }
    }

    let seconds = elapsed.as_secs_f64().max(1e-9);
    let tri_per_sec = output.stats.triangles as f64 / seconds;
    println!(
        "rendered {} triangles at {}x{} with '{}' in {:.2?} ({:.0} triangles/sec) -> {}",
        output.stats.triangles,
        output.stats.width,
        output.stats.height,
        output.stats.mode.as_str(),
        elapsed,
        tri_per_sec,
        cli.output.display()
    );

    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            print_help();
            process::exit(0);
        }

        let (key, inline_value) = match arg.split_once('=') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (arg, None),
        };

        let mut value = |name: &str| -> Result<String, String> {
            if let Some(value) = inline_value.clone() {
                Ok(value)
            } else {
                args.next()
                    .ok_or_else(|| format!("missing value for {name}"))
            }
        };

        match key.as_str() {
            "--model" => cli.model = PathBuf::from(value("--model")?),
            "--width" => cli.width = parse_u32(&value("--width")?, "--width")?,
            "--height" => cli.height = parse_u32(&value("--height")?, "--height")?,
            "--shader" => cli.shader = RenderMode::from_str(&value("--shader")?)?,
            "--output" => cli.output = PathBuf::from(value("--output")?),
            "--camera" => cli.camera = Some(parse_vec3(&value("--camera")?, "--camera")?),
            "--light" => cli.light = parse_vec3(&value("--light")?, "--light")?,
            "--debug-dir" => cli.debug_dir = Some(PathBuf::from(value("--debug-dir")?)),
            "--cull" => cli.cull_mode = CullMode::from_str(&value("--cull")?)?,
            "--front-face" => cli.front_face = FrontFace::from_str(&value("--front-face")?)?,
            "--depth-test" => {
                cli.depth_test = parse_on_off(&value("--depth-test")?, "--depth-test")?
            }
            "--depth-write" => {
                cli.depth_write = parse_on_off(&value("--depth-write")?, "--depth-write")?
            }
            "--fill" => cli.fill_mode = FillMode::from_str(&value("--fill")?)?,
            "--scissor" => cli.scissor = Some(parse_scissor(&value("--scissor")?)?),
            "--clear-color" => {
                cli.clear_color = parse_rgb(&value("--clear-color")?, "--clear-color")?
            }
            "--clear-depth" => {
                cli.clear_depth = parse_f32_arg(&value("--clear-depth")?, "--clear-depth")?
            }
            "--color-write" => {
                cli.color_write = parse_on_off(&value("--color-write")?, "--color-write")?
            }
            "--texture" => cli.texture = Some(PathBuf::from(value("--texture")?)),
            "--normal-map" => cli.normal_map = Some(PathBuf::from(value("--normal-map")?)),
            "--texture-wrap" => {
                cli.texture_wrap = TextureWrap::from_str(&value("--texture-wrap")?)?
            }
            "--texture-filter" => {
                cli.texture_filter = TextureFilter::from_str(&value("--texture-filter")?)?
            }
            "--base-color" => {
                cli.material.base_color = parse_rgb(&value("--base-color")?, "--base-color")?
            }
            "--ambient" => cli.material.ambient = parse_f32_arg(&value("--ambient")?, "--ambient")?,
            "--diffuse" => cli.material.diffuse = parse_f32_arg(&value("--diffuse")?, "--diffuse")?,
            "--specular" => {
                cli.material.specular = parse_f32_arg(&value("--specular")?, "--specular")?
            }
            "--shininess" => {
                cli.material.shininess = parse_f32_arg(&value("--shininess")?, "--shininess")?
            }
            "--shadow-size" => {
                cli.shadow.size = parse_positive_u32(&value("--shadow-size")?, "--shadow-size")?
            }
            "--shadow-bias" => {
                cli.shadow.bias = parse_f32_arg(&value("--shadow-bias")?, "--shadow-bias")?
            }
            "--shadow-filter" => {
                cli.shadow.filter = ShadowFilter::from_str(&value("--shadow-filter")?)?
            }
            _ => {
                return Err(format!(
                    "unknown argument '{}'; run with --help for usage",
                    key
                ));
            }
        }
    }

    if cli.width == 0 || cli.height == 0 {
        return Err("--width and --height must be greater than zero".to_string());
    }
    if cli.light.length_squared() < 1e-6 {
        return Err("--light must not be the zero vector".to_string());
    }
    if cli.material.ambient < 0.0
        || cli.material.diffuse < 0.0
        || cli.material.specular < 0.0
        || cli.material.shininess <= 0.0
    {
        return Err(
            "--ambient/--diffuse/--specular must be >= 0 and --shininess must be > 0".to_string(),
        );
    }
    if cli.shadow.bias < 0.0 {
        return Err("--shadow-bias must be >= 0".to_string());
    }

    Ok(cli)
}

fn parse_on_off(value: &str, label: &str) -> Result<bool, String> {
    match value {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("{label} must be one of: on, off")),
    }
}

fn parse_u32(value: &str, label: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|err| format!("invalid {label} value '{value}': {err}"))
}

fn parse_positive_u32(value: &str, label: &str) -> Result<u32, String> {
    let parsed = parse_u32(value, label)?;
    if parsed == 0 {
        Err(format!("{label} must be greater than zero"))
    } else {
        Ok(parsed)
    }
}

fn parse_f32_arg(value: &str, label: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .map_err(|err| format!("invalid {label} value '{value}': {err}"))
}

fn parse_rgb(value: &str, label: &str) -> Result<Rgb<u8>, String> {
    let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(format!("{label} must be formatted as r,g,b"));
    }
    Ok(Rgb([
        parse_u8_channel(parts[0], label, "red")?,
        parse_u8_channel(parts[1], label, "green")?,
        parse_u8_channel(parts[2], label, "blue")?,
    ]))
}

fn parse_u8_channel(value: &str, label: &str, channel: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|err| format!("invalid {label} {channel} channel '{value}': {err}"))
}

fn parse_scissor(value: &str) -> Result<ScissorRect, String> {
    let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err("--scissor must be formatted as x,y,width,height".to_string());
    }
    Ok(ScissorRect {
        x: parse_u32(parts[0], "--scissor x")?,
        y: parse_u32(parts[1], "--scissor y")?,
        width: parse_u32(parts[2], "--scissor width")?,
        height: parse_u32(parts[3], "--scissor height")?,
    })
}

fn parse_vec3(value: &str, label: &str) -> Result<Vec3, String> {
    let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(format!(
            "{label} must be formatted as x,y,z, for example 0,0,3"
        ));
    }
    let x = parts[0]
        .parse::<f32>()
        .map_err(|err| format!("invalid {label} x value '{}': {}", parts[0], err))?;
    let y = parts[1]
        .parse::<f32>()
        .map_err(|err| format!("invalid {label} y value '{}': {}", parts[1], err))?;
    let z = parts[2]
        .parse::<f32>()
        .map_err(|err| format!("invalid {label} z value '{}': {}", parts[2], err))?;
    Ok(Vec3::new(x, y, z))
}

fn save_image(image: &image::RgbImage, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create '{}': {}", parent.display(), err))?;
        }
    }
    image
        .save(path)
        .map_err(|err| format!("failed to save '{}': {}", path.display(), err))
}

fn load_texture(path: &Path) -> Result<image::RgbImage, String> {
    image::open(path)
        .map_err(|err| format!("failed to load texture '{}': {}", path.display(), err))
        .map(|image| image.to_rgb8())
}

fn print_help() {
    println!(
        "\
Octo Render CPU software renderer

Usage:
  cargo run --release --bin app -- [options]

Options:
  --model <path>       OBJ model to render [default: resources/teapot.obj]
  --width <px>         Output width [default: 1024]
  --height <px>        Output height [default: 1024]
  --shader <mode>      One of: {}
  --output <path>      Output image path [default: renders/teapot-shadow.png]
  --camera <x,y,z>     Camera eye in normalized scene space [default: 0,0.25,3]
  --light <x,y,z>      Directional light vector [default: 1,1,-1]
  --debug-dir <path>   Write depth, shadow-map, normals, and AO debug PNGs
  --cull <mode>        Face culling: off, front, back [default: off]
  --front-face <mode>  Front-face winding: ccw, cw [default: ccw]
  --depth-test <on|off>  Enable/disable z-buffer testing [default: on]
  --depth-write <on|off> Enable/disable z-buffer writes [default: on]
  --fill <mode>        Fill mode: solid, wireframe [default: solid]
  --scissor <x,y,w,h>  Restrict rasterization to a screen rectangle
  --clear-color <r,g,b> Clear color [default: 18,20,24]
  --clear-depth <z>    Clear depth [default: -inf]
  --color-write <on|off> Enable/disable color writes [default: on]
  --texture <path>     Texture image used by --shader texture
  --normal-map <path>  Normal-map image used by --shader normal-map
  --texture-wrap <mode> Texture wrap: repeat, clamp [default: repeat]
  --texture-filter <mode> Texture filter: nearest, linear [default: nearest]
  --base-color <r,g,b> Material base color [default: 220,220,220]
  --ambient <value>    Ambient material factor [default: 0]
  --diffuse <value>    Diffuse material factor [default: 1]
  --specular <value>   Specular material factor [default: 0]
  --shininess <value>  Specular exponent control [default: 32]
  --shadow-size <px>   Square shadow-map size [default: output dimensions]
  --shadow-bias <z>    Shadow-map depth bias [default: 4]
  --shadow-filter <mode> Shadow filter: none, pcf [default: none]
  --help               Show this help
",
        RenderMode::all().join(", ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_render_state_flags() {
        let cli = parse_args(
            [
                "--shader",
                "texture",
                "--cull",
                "back",
                "--front-face",
                "cw",
                "--depth-test",
                "off",
                "--depth-write",
                "off",
                "--fill",
                "wireframe",
                "--texture-wrap",
                "clamp",
                "--texture-filter",
                "linear",
                "--scissor",
                "1,2,3,4",
                "--clear-color",
                "10,20,30",
                "--clear-depth",
                "-inf",
                "--color-write",
                "off",
                "--base-color",
                "200,180,160",
                "--ambient",
                "0.1",
                "--diffuse",
                "0.8",
                "--specular",
                "0.2",
                "--shininess",
                "64",
                "--shadow-size",
                "256",
                "--shadow-bias",
                "2",
                "--shadow-filter",
                "pcf",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();

        assert_eq!(cli.shader, RenderMode::Texture);
        assert_eq!(cli.cull_mode, CullMode::Back);
        assert_eq!(cli.front_face, FrontFace::Cw);
        assert!(!cli.depth_test);
        assert!(!cli.depth_write);
        assert_eq!(cli.fill_mode, FillMode::Wireframe);
        assert_eq!(cli.texture_wrap, TextureWrap::Clamp);
        assert_eq!(cli.texture_filter, TextureFilter::Linear);
        assert_eq!(
            cli.scissor,
            Some(ScissorRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4
            })
        );
        assert_eq!(cli.clear_color, Rgb([10, 20, 30]));
        assert!(cli.clear_depth.is_infinite() && cli.clear_depth.is_sign_negative());
        assert!(!cli.color_write);
        assert_eq!(cli.material.base_color, Rgb([200, 180, 160]));
        assert_eq!(cli.material.ambient, 0.1);
        assert_eq!(cli.material.diffuse, 0.8);
        assert_eq!(cli.material.specular, 0.2);
        assert_eq!(cli.material.shininess, 64.0);
        assert_eq!(cli.shadow.size, 256);
        assert_eq!(cli.shadow.bias, 2.0);
        assert_eq!(cli.shadow.filter, ShadowFilter::Pcf);
    }
}
