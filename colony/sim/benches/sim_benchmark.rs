//! Tick-time benchmarks for the M1 exit criteria (design document, section 14).

use colony_sim::scenario;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn tick_benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("tick");
    for size in [128usize, 256, 512] {
        group.throughput(Throughput::Elements((size * size) as u64));
        group.bench_with_input(
            BenchmarkId::new("gas-mixing-sequential", size),
            &size,
            |b, &size| {
                let mut sim = scenario::gas_mixing(size, size);
                sim.set_parallel(false);
                b.iter(|| sim.step());
            },
        );
    }
    #[cfg(feature = "parallel")]
    group.bench_function(BenchmarkId::new("gas-mixing-parallel", 512), |b| {
        let mut sim = scenario::gas_mixing(512, 512);
        sim.set_parallel(true);
        b.iter(|| sim.step());
    });
    group.bench_function(BenchmarkId::new("noise-sequential", 512), |b| {
        let mut sim = scenario::noise(512, 512, 1);
        sim.set_parallel(false);
        b.iter(|| sim.step());
    });
    #[cfg(feature = "parallel")]
    group.bench_function(BenchmarkId::new("noise-parallel", 512), |b| {
        let mut sim = scenario::noise(512, 512, 1);
        sim.set_parallel(true);
        b.iter(|| sim.step());
    });
    group.bench_function(BenchmarkId::new("settled-sequential", 512), |b| {
        let mut sim = scenario::settled(512, 512);
        sim.set_parallel(false);
        sim.step();
        b.iter(|| sim.step());
    });
    group.bench_function(BenchmarkId::new("hot-rock-sequential", 512), |b| {
        let mut sim = scenario::hot_rock(512, 512);
        sim.set_parallel(false);
        b.iter(|| sim.step());
    });
    group.finish();

    let mut group = c.benchmark_group("hash");
    group.bench_function("state-hash-512", |b| {
        let sim = scenario::gas_mixing(512, 512);
        b.iter(|| sim.state_hash());
    });
    group.finish();
}

criterion_group!(benches, tick_benches);
criterion_main!(benches);
