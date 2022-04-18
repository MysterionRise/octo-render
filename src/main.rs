use image::{ImageBuffer, Rgb, RgbImage};
use image::imageops::{flip_vertical_in_place};
use renderer::{draw_line, read_waveform_obj_file};


fn main() {
    let (vertices, faces) = read_waveform_obj_file("resources/teapot.obj");
    let tga_red = Rgb([255, 0, 0]);
    let tga_green = Rgb([0, 255, 0]);
    let tga_white = Rgb([255, 255, 255]);
    let width = 1024.0;
    let height = 1024.0;
    let coeff = 100.0;
    let mut image: RgbImage = ImageBuffer::new(width as u32, height as u32);
    for face in faces {
        let v1 = vertices[face.0 as usize - 1];
        let v2 = vertices[face.1 as usize - 1];
        let v3 = vertices[face.2 as usize - 1];
        draw_line((v1.0 * coeff + width / 2.0) as i32,
                  (v1.1 * coeff + height / 2.0) as i32,
                  (v2.0 * coeff + width / 2.0) as i32,
                  (v2.1 * coeff + height / 2.0) as i32, &mut image, tga_white);
        draw_line((v2.0 * coeff + width / 2.0) as i32,
                  (v2.1 * coeff + height / 2.0) as i32,
                  (v3.0 * coeff + width / 2.0) as i32,
                  (v3.1 * coeff + height / 2.0) as i32, &mut image, tga_red);
        draw_line((v1.0 * coeff + width / 2.0) as i32,
                  (v1.1 * coeff + height / 2.0) as i32,
                  (v3.0 * coeff + width / 2.0) as i32,
                  (v3.1 * coeff + height / 2.0) as i32, &mut image, tga_green);
    }
    flip_vertical_in_place(&mut image);
    image.save("output.tga").unwrap();
}
