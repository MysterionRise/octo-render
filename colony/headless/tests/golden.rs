//! Pins the debug render of a scenario pixel for pixel. The kernel is integer-exact, so
//! the comparison is exact too. `UPDATE_GOLDENS=1 cargo test -p colony-headless golden`
//! rewrites the image; inspect the diff before committing it.

use std::path::PathBuf;

use colony_headless::render_elements;
use colony_sim::scenario;

#[test]
fn gas_mixing_64_elements_match_golden() {
    let mut sim = scenario::gas_mixing(64, 64);
    for _ in 0..120 {
        sim.step();
    }
    let img = render_elements(&sim);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/gas-mixing-64-120.png");
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        img.save(&path).unwrap();
        return;
    }
    let golden = image::open(&path)
        .unwrap_or_else(|_| {
            panic!(
                "missing {}: run with UPDATE_GOLDENS=1 to create it",
                path.display()
            )
        })
        .to_rgb8();
    assert_eq!(golden.dimensions(), img.dimensions());
    assert!(golden.as_raw() == img.as_raw(), "render differs from the golden image; re-bless with UPDATE_GOLDENS=1 only for an intentional rule change");
}
