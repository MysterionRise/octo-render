//! `colony-headless`: run a scenario without a window, print tick timings and chunk
//! statistics, write per-tick hashes, dump PNGs, or compare two hash logs.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use colony_headless::{render_elements, render_temperature};
use colony_sim::scenario;

struct Options {
    scenario: String,
    width: usize,
    height: usize,
    ticks: usize,
    threads: usize,
    hash_out: Option<PathBuf>,
    png_dir: Option<PathBuf>,
    png_every: usize,
    compare: Option<(PathBuf, PathBuf)>,
    profile: bool,
    seed: u64,
}

const USAGE: &str = "usage: colony-headless [--scenario NAME] [--width N] [--height N] [--ticks N]
                       [--threads N] [--hash-out FILE] [--png-dir DIR] [--png-every N] [--profile] [--seed N]
       colony-headless --compare A.hashes B.hashes

  --scenario   gas-mixing, settled, pressure-burst, hot-rock, noise, pool, cave, crew (default gas-mixing)
  --width/--height   world size in cells (default 512 x 512)
  --ticks      ticks to run (default 300)
  --threads    0 = all cores, 1 = sequential (default 1)
  --hash-out   write one 'tick hash' line per tick
  --png-dir    write element and temperature PNGs every --png-every ticks (default 100)
  --profile    also report the time spent in each pass (sort, flux, heat)
  --seed       world seed for the noise and cave scenarios (default 1)
  --compare    print the first tick where two hash logs differ";

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        scenario: "gas-mixing".to_string(),
        width: 512,
        height: 512,
        ticks: 300,
        threads: 1,
        hash_out: None,
        png_dir: None,
        png_every: 100,
        compare: None,
        profile: false,
        seed: 1,
    };
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
            "--ticks" => o.ticks = parse_num(&value(&mut i, flag)?, flag)?,
            "--threads" => o.threads = parse_num(&value(&mut i, flag)?, flag)?,
            "--png-every" => o.png_every = parse_num(&value(&mut i, flag)?, flag)?.max(1),
            "--hash-out" => o.hash_out = Some(PathBuf::from(value(&mut i, flag)?)),
            "--png-dir" => o.png_dir = Some(PathBuf::from(value(&mut i, flag)?)),
            "--compare" => {
                let a = value(&mut i, flag)?;
                let b = value(&mut i, flag)?;
                o.compare = Some((PathBuf::from(a), PathBuf::from(b)));
            }
            "--profile" => o.profile = true,
            "--seed" => o.seed = parse_num(&value(&mut i, flag)?, flag)? as u64,
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown flag {other}\n{USAGE}")),
        }
        i += 1;
    }
    Ok(o)
}

fn parse_num(s: &str, flag: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("{flag}: expected a number, got {s:?}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse(&args) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    if let Some((a, b)) = &opts.compare {
        return compare(a, b);
    }
    run(&opts)
}

fn compare(a: &PathBuf, b: &PathBuf) -> ExitCode {
    let read = |p: &PathBuf| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let (ta, tb) = match (read(a), read(b)) {
        (Ok(ta), Ok(tb)) => (ta, tb),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    for (n, (la, lb)) in ta.lines().zip(tb.lines()).enumerate() {
        if la != lb {
            println!(
                "first divergence at line {} (tick {}):\n  {}\n  {}",
                n + 1,
                la.split(' ').next().unwrap_or("?"),
                la,
                lb
            );
            return ExitCode::FAILURE;
        }
    }
    let (na, nb) = (ta.lines().count(), tb.lines().count());
    if na != nb {
        println!(
            "logs agree for {} ticks but have different lengths ({na} vs {nb})",
            na.min(nb)
        );
        return ExitCode::FAILURE;
    }
    println!("identical for {na} ticks");
    ExitCode::SUCCESS
}

fn run(opts: &Options) -> ExitCode {
    let Some(mut sim) =
        scenario::by_name_seeded(&opts.scenario, opts.width, opts.height, opts.seed)
    else {
        eprintln!(
            "unknown scenario {:?} or world under 3x3; known: {}",
            opts.scenario,
            scenario::NAMES.join(", ")
        );
        return ExitCode::FAILURE;
    };
    sim.set_parallel(opts.threads != 1);
    let pool = match opts.threads {
        1 => None,
        n => Some(rayon_pool(n).unwrap_or_else(|e| {
            eprintln!("thread pool: {e}");
            std::process::exit(1)
        })),
    };
    if let Some(dir) = &opts.png_dir {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("{}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    }
    let mut hashes = String::new();
    let mut times = Vec::with_capacity(opts.ticks);
    let mut pass_times = [std::time::Duration::ZERO; 4];
    let mut processed_total = 0usize;
    let mut processed_mass_total = 0usize;
    let mut path_solves_total = 0usize;
    let start_mass = sim.total_mass();
    let start_energy = sim.total_energy();
    let chunks = sim.chunks().len();
    println!(
        "scenario {} {}x{} ({} chunks), {} ticks, threads {}",
        opts.scenario,
        opts.width,
        opts.height,
        chunks,
        opts.ticks,
        if opts.threads == 0 {
            "all".to_string()
        } else {
            opts.threads.to_string()
        }
    );
    for t in 0..opts.ticks {
        let t0 = Instant::now();
        let stats = if opts.profile {
            let mut timed = || {
                sim.begin_tick();
                let mut mark = Instant::now();
                sim.pass_sort();
                pass_times[0] += mark.elapsed();
                mark = Instant::now();
                sim.pass_flux();
                pass_times[1] += mark.elapsed();
                mark = Instant::now();
                sim.pass_heat();
                pass_times[2] += mark.elapsed();
                mark = Instant::now();
                sim.pass_agents();
                pass_times[3] += mark.elapsed();
                sim.end_tick()
            };
            match &pool {
                Some(pool) => pool.install(timed),
                None => timed(),
            }
        } else {
            match &pool {
                Some(pool) => pool.install(|| sim.step()),
                None => sim.step(),
            }
        };
        times.push(t0.elapsed());
        processed_total += stats.chunks_processed;
        processed_mass_total += stats.chunks_processed_mass;
        path_solves_total += stats.path_solves as usize;
        if opts.hash_out.is_some() {
            hashes.push_str(&format!("{} {:016x}\n", stats.tick, sim.state_hash()));
        }
        if let Some(dir) = &opts.png_dir {
            if t % opts.png_every == 0 || t + 1 == opts.ticks {
                let tag = format!("{}-{:05}", opts.scenario, stats.tick);
                if let Err(e) = render_elements(&sim).save(dir.join(format!("{tag}-elements.png")))
                {
                    eprintln!("png: {e}");
                }
                if let Err(e) =
                    render_temperature(&sim).save(dir.join(format!("{tag}-temperature.png")))
                {
                    eprintln!("png: {e}");
                }
            }
        }
    }
    if let Some(path) = &opts.hash_out {
        if let Err(e) = fs::write(path, hashes) {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    let total_ms =
        |d: &[std::time::Duration]| d.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>();
    let mut sorted = times.clone();
    sorted.sort();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    let n = times.len().max(1);
    println!(
        "tick time: avg {:.3} ms, min {:.3}, p50 {:.3}, p95 {:.3}, max {:.3}",
        total_ms(&times) / n as f64,
        ms(sorted[0]),
        ms(sorted[n / 2]),
        ms(sorted[(n * 95 / 100).min(n - 1)]),
        ms(sorted[n - 1])
    );
    if opts.profile {
        let per = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / n as f64;
        println!(
            "passes: sort {:.3} ms, flux {:.3} ms, heat {:.3} ms, agents {:.3} ms per tick",
            per(pass_times[0]),
            per(pass_times[1]),
            per(pass_times[2]),
            per(pass_times[3])
        );
    }
    if !sim.colonists().is_empty() {
        println!(
            "agents: {} colonists, {:.2} path solves per tick, jobs {:?}",
            sim.colonists().len(),
            path_solves_total as f64 / n as f64,
            sim.jobs().counts()
        );
    }
    let pct = |count: usize| 100.0 * count as f64 / (chunks * n) as f64;
    println!(
        "chunks: heat pass processed {:.1}%, mass passes processed {:.1}%, fully skipped {:.1}%",
        pct(processed_total),
        pct(processed_mass_total),
        100.0 - pct(processed_total)
    );
    let end_mass = sim.total_mass();
    println!(
        "mass: start {} raw, end {} raw, delta {} ({})",
        start_mass,
        end_mass,
        end_mass as i128 - start_mass as i128,
        if start_mass == end_mass {
            "conserved exactly"
        } else {
            "NOT CONSERVED"
        }
    );
    let drift = sim.total_energy() - start_energy;
    let bound = 2 * n as i128 * sim.total_capacity();
    println!(
        "energy: drift {} of bound {} ({})",
        drift,
        bound,
        if drift.abs() <= bound {
            "within rounding bound"
        } else {
            "EXCEEDS BOUND"
        }
    );
    println!("hash: {:016x}", sim.state_hash());
    if start_mass != end_mass || drift.abs() > bound {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "parallel")]
fn rayon_pool(threads: usize) -> Result<rayon::ThreadPool, String> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "parallel"))]
fn rayon_pool(_threads: usize) -> Result<NoPool, String> {
    Err("built without the parallel feature; use --threads 1".to_string())
}

#[cfg(not(feature = "parallel"))]
struct NoPool;

#[cfg(not(feature = "parallel"))]
impl NoPool {
    fn install<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
}
