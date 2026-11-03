//! `colony-game`: the windowed viewer for the colony-sim kernel, plus offscreen
//! screenshot and benchmark modes that need no window (but do need a GPU adapter).

mod app;
mod input;
mod offscreen;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use colony_render::Overlay;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub scenario: String,
    pub width: usize,
    pub height: usize,
    pub seed: u64,
    pub ticks: usize,
    pub zoom: u32,
    pub overlay: Overlay,
    pub speed: u32,
    pub screenshot: Option<PathBuf>,
    pub benchmark: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            scenario: "cave".to_string(),
            width: 256,
            height: 192,
            seed: 1,
            ticks: 0,
            zoom: 3,
            overlay: Overlay::None,
            speed: 1,
            screenshot: None,
            benchmark: None,
        }
    }
}

const USAGE: &str =
    "usage: colony-game [--scenario NAME] [--width N] [--height N] [--seed N] [--zoom 1..8]
                   [--overlay none|temperature|pressure] [--speed 1..3]
       colony-game --screenshot FILE.png [--ticks N] [other flags]
       colony-game --benchmark FRAMES [--ticks N] [other flags]

  --scenario    gas-mixing, settled, pressure-burst, hot-rock, noise, pool, cave, crew (default cave)
  --width/--height   world size in cells (default 256 x 192)
  --seed        world seed for the noise and cave scenarios (default 1)
  --ticks       steps to run before a screenshot or benchmark (default 0)
  --zoom        integer zoom, 1 to 8 (default 3)
  --overlay     none, temperature, or pressure (default none)
  --speed       initial sim speed multiplier, 1 to 3 (default 1)
  --screenshot  render one 1920x1080 frame offscreen to a PNG and exit (needs a GPU adapter)
  --benchmark   render FRAMES frames offscreen at 1920x1080 and report ms per frame

Window controls: WASD or arrows pan, + and - or the wheel zoom, space pauses, 1/2/3 set
the speed, Q/E/C/N/P/O pick the dig, build, cancel, spawn, pour, or oxygen tool, left
click applies it, right click pours water, Esc quits. Hover a colonist for its state.";

fn parse_num<T: std::str::FromStr>(s: &str, flag: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("{flag}: expected a number, got {s:?}"))
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--scenario" => o.scenario = value(&mut i, flag)?,
            "--width" => o.width = parse_num(&value(&mut i, flag)?, flag)?,
            "--height" => o.height = parse_num(&value(&mut i, flag)?, flag)?,
            "--seed" => o.seed = parse_num(&value(&mut i, flag)?, flag)?,
            "--ticks" => o.ticks = parse_num(&value(&mut i, flag)?, flag)?,
            "--zoom" => {
                o.zoom = parse_num(&value(&mut i, flag)?, flag)?;
                if !(1..=8).contains(&o.zoom) {
                    return Err("--zoom: expected 1 to 8".to_string());
                }
            }
            "--speed" => {
                o.speed = parse_num(&value(&mut i, flag)?, flag)?;
                if !(1..=3).contains(&o.speed) {
                    return Err("--speed: expected 1 to 3".to_string());
                }
            }
            "--overlay" => {
                let name = value(&mut i, flag)?;
                o.overlay = Overlay::from_name(&name)
                    .ok_or_else(|| format!("--overlay: unknown overlay {name:?}"))?;
            }
            "--screenshot" => o.screenshot = Some(PathBuf::from(value(&mut i, flag)?)),
            "--benchmark" => o.benchmark = Some(parse_num(&value(&mut i, flag)?, flag)?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown flag {other}\n{USAGE}")),
        }
        i += 1;
    }
    if o.width < 3 || o.height < 3 {
        return Err("world must be at least 3x3".to_string());
    }
    Ok(o)
}

fn main() -> ExitCode {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse(&args) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    if opts.screenshot.is_some() || opts.benchmark.is_some() {
        offscreen::run(&opts)
    } else {
        app::run(opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn defaults_and_every_flag_parse() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        let o = parse(&args("--scenario pool --width 64 --height 48 --seed 9 --ticks 20 --zoom 2 --overlay pressure --speed 3 --screenshot out.png --benchmark 50")).unwrap();
        assert_eq!(o.scenario, "pool");
        assert_eq!(
            (o.width, o.height, o.seed, o.ticks, o.zoom, o.speed),
            (64, 48, 9, 20, 2, 3)
        );
        assert_eq!(o.overlay, Overlay::Pressure);
        assert_eq!(o.screenshot, Some(PathBuf::from("out.png")));
        assert_eq!(o.benchmark, Some(50));
    }

    #[test]
    fn bad_input_is_rejected_with_a_message() {
        assert!(parse(&args("--benchmark"))
            .unwrap_err()
            .contains("needs a value"));
        assert!(parse(&args("--benchmark ten"))
            .unwrap_err()
            .contains("expected a number"));
        assert!(parse(&args("--zoom 9")).unwrap_err().contains("1 to 8"));
        assert!(parse(&args("--overlay heat"))
            .unwrap_err()
            .contains("unknown overlay"));
        assert!(parse(&args("--bogus"))
            .unwrap_err()
            .contains("unknown flag"));
        assert!(parse(&args("-h")).unwrap_err().starts_with("usage:"));
        assert!(parse(&args("--width 2")).unwrap_err().contains("3x3"));
    }
}
