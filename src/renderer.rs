use image::{Rgb, RgbImage};

pub fn draw_line(mut x1: i32, mut y1: i32, mut x2: i32, mut y2: i32, image: &mut RgbImage, colour: Rgb<u8>) {
    let mut steep: bool = false;
    if i32::abs(x1 - x2) < i32::abs(y1 - y2) {
        steep = true;
        std::mem::swap(&mut x1, &mut y1);
        std::mem::swap(&mut x2, &mut y2);
    }
    if x1 > x2 {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut y1, &mut y2);
    }

    for x in x1..x2 {
        let t = (x - x1) as f32 / (x2 - x1) as f32;
        let y = y1 as f32 * (1.0 - t) + y2 as f32 * t;
        if steep {
            *image.get_pixel_mut(y as u32, x as u32) = colour;
        } else {
            *image.get_pixel_mut(x as u32, y as u32) = colour;
        }
    }
}




