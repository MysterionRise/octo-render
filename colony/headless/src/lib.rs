//! Debug renders of kernel state. The simulation's row 0 is the bottom of the world,
//! so images are flipped vertically.

use colony_sim::{Mass, Phase, Sim};
use image::{Rgb, RgbImage};

/// Element colour shaded by mass: gases brighten up to 2 kg per cell, liquids up to
/// 1,000 kg, solids are drawn at full colour, vacuum is near black.
pub fn render_elements(sim: &Sim) -> RgbImage {
    let world = sim.world();
    let table = sim.table();
    let mut img = RgbImage::new(world.width as u32, world.height as u32);
    for y in 0..world.height {
        for x in 0..world.width {
            let (element, mass, _) = world.get(x, y);
            let def = table.get(element);
            let brightness = match def.phase {
                Phase::Vacuum => 256,
                Phase::Gas => 64 + shade(mass, Mass::from_kg(2)),
                Phase::Liquid => 64 + shade(mass, Mass::from_kg(1000)),
                Phase::Solid => 256,
            };
            let px = Rgb(def
                .colour
                .map(|c| ((c as u32 * brightness) >> 8).min(255) as u8));
            img.put_pixel(x as u32, (world.height - 1 - y) as u32, px);
        }
    }
    img
}

/// 0..=192 proportional to `mass / full`, saturating.
fn shade(mass: Mass, full: Mass) -> u32 {
    let m = mass.raw() as u64;
    let f = full.raw() as u64;
    ((m.min(f) * 192) / f) as u32
}

/// Temperature from 250 K (blue) to 350 K (red); vacuum is black.
pub fn render_temperature(sim: &Sim) -> RgbImage {
    let world = sim.world();
    let mut img = RgbImage::new(world.width as u32, world.height as u32);
    for y in 0..world.height {
        for x in 0..world.width {
            let (element, _, temp) = world.get(x, y);
            let px = if element.0 == 0 {
                Rgb([0, 0, 0])
            } else {
                let mk = temp.to_millikelvin().clamp(250_000, 350_000) - 250_000;
                let t = (mk * 255 / 100_000) as u8;
                Rgb([t, 60, 255 - t])
            };
            img.put_pixel(x as u32, (world.height - 1 - y) as u32, px);
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_sim::scenario;

    #[test]
    fn renders_flip_rows_and_shade_by_mass() {
        let sim = scenario::gas_mixing(8, 6);
        let img = render_elements(&sim);
        assert_eq!(img.dimensions(), (8, 6));
        // Bottom row of the world is sandstone at full colour, drawn at the image bottom.
        assert_eq!(img.get_pixel(0, 5).0, [160, 140, 100]);
        // Oxygen at 2 kg is at full brightness; carbon dioxide at 0.5 kg is dimmer.
        assert_eq!(img.get_pixel(1, 2).0, [120, 200, 255]);
        let co2 = img.get_pixel(6, 2).0;
        assert!(co2[0] < 200 && co2[0] > 0);
        let heat = render_temperature(&sim);
        assert!(heat.get_pixel(1, 2).0[2] > heat.get_pixel(6, 2).0[2]);
    }
}
