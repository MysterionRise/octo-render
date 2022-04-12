use image::{ImageBuffer, Rgb, RgbImage};
use image::imageops::{flip_vertical_in_place};

fn main() {
    let tga_red = Rgb([255, 0, 0]);
    let mut image: RgbImage = ImageBuffer::new(100, 100);
    *image.get_pixel_mut(52, 41) = tga_red;
    flip_vertical_in_place(&mut image);
    image.save("output.tga").unwrap();
}
