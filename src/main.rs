use image::{ImageBuffer, Rgb, RgbImage};
use image::imageops::{flip_vertical_in_place};
use renderer::{draw_line};


fn main() {
    let tga_red = Rgb([255, 0, 0]);
    let tga_white = Rgb([255, 255, 255]);
    let mut image: RgbImage = ImageBuffer::new(100, 100);
    // *image.get_pixel_mut(52, 41) = tga_red;
    // draw_line(0, 0, 100, 100, &mut image, tga_red);
    draw_line(13, 20, 80, 40, &mut image, tga_white);
    draw_line(20, 13, 40, 80, &mut image, tga_red);
    draw_line(80, 40, 13, 20, &mut image, tga_red);
    flip_vertical_in_place(&mut image);
    image.save("output.tga").unwrap();
}
