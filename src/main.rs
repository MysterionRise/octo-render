use glam::Vec3;
use renderer::{
    render_model, CullMode, DepthState, FillMode, Model, RenderMode, RenderOptions, RenderState,
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
    depth_test: bool,
    depth_write: bool,
    fill_mode: FillMode,
    texture: Option<PathBuf>,
    texture_wrap: TextureWrap,
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
            depth_test: true,
            depth_write: true,
            fill_mode: FillMode::Solid,
            texture: None,
            texture_wrap: TextureWrap::Repeat,
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
    let options = RenderOptions {
        width: cli.width,
        height: cli.height,
        mode: cli.shader,
        camera_eye: cli.camera,
        light_dir: cli.light,
        debug_outputs: cli.debug_dir.is_some(),
        render_state: RenderState {
            cull_mode: cli.cull_mode,
            depth: DepthState {
                test_enabled: cli.depth_test,
                write_enabled: cli.depth_write,
            },
            fill_mode: cli.fill_mode,
        },
        texture,
        texture_sampler: TextureSampler {
            wrap: cli.texture_wrap,
        },
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
            "--depth-test" => {
                cli.depth_test = parse_on_off(&value("--depth-test")?, "--depth-test")?
            }
            "--depth-write" => {
                cli.depth_write = parse_on_off(&value("--depth-write")?, "--depth-write")?
            }
            "--fill" => cli.fill_mode = FillMode::from_str(&value("--fill")?)?,
            "--texture" => cli.texture = Some(PathBuf::from(value("--texture")?)),
            "--texture-wrap" => {
                cli.texture_wrap = TextureWrap::from_str(&value("--texture-wrap")?)?
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
  --depth-test <on|off>  Enable/disable z-buffer testing [default: on]
  --depth-write <on|off> Enable/disable z-buffer writes [default: on]
  --fill <mode>        Fill mode: solid, wireframe [default: solid]
  --texture <path>     Texture image used by --shader texture
  --texture-wrap <mode> Texture wrap: repeat, clamp [default: repeat]
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
                "--depth-test",
                "off",
                "--depth-write",
                "off",
                "--fill",
                "wireframe",
                "--texture-wrap",
                "clamp",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();

        assert_eq!(cli.shader, RenderMode::Texture);
        assert_eq!(cli.cull_mode, CullMode::Back);
        assert!(!cli.depth_test);
        assert!(!cli.depth_write);
        assert_eq!(cli.fill_mode, FillMode::Wireframe);
        assert_eq!(cli.texture_wrap, TextureWrap::Clamp);
    }
}
