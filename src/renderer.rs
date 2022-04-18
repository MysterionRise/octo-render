use image::{Rgb, RgbImage};
use std::fs::File;
use std::io;
use std::io::{BufRead};
use io::BufReader;

pub fn read_waveform_obj_file(filename: &str) -> (Vec<(f32, f32, f32)>, Vec<(i32, i32, i32)>) {
    let file = File::open(filename).unwrap();
    let reader = BufReader::new(file);
    let lines = reader.lines();
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for line in lines {
        let line = line.unwrap();
        if line.starts_with("f") {
            // println!("{}", line);
            let mut parts = line.split_whitespace();
            parts.next();
            let v1 = parts.next().unwrap().parse::<i32>().unwrap();
            let v2 = parts.next().unwrap().parse::<i32>().unwrap();
            let v3 = parts.next().unwrap().parse::<i32>().unwrap();
            faces.push((v1, v2, v3));
        } else if line.starts_with("v") {
            // println!("{}", line);
            let mut parts = line.split_whitespace();
            parts.next();
            let x = parts.next().unwrap().parse::<f32>().unwrap();
            let y = parts.next().unwrap().parse::<f32>().unwrap();
            let z = parts.next().unwrap().parse::<f32>().unwrap();
            vertices.push((x, y, z));
        }
    }
    (vertices, faces)
}

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

    let dx = x2 - x1;
    let derror2 = i32::abs(y2 - y1) * 2;
    let mut error2 = 0;
    let mut y = y1;
    for x in x1..x2 {
        if steep {
            *image.get_pixel_mut(y as u32, x as u32) = colour;
        } else {
            *image.get_pixel_mut(x as u32, y as u32) = colour;
        }
        error2 += derror2;
        if error2 > dx {
            y += if y2 > y1 { 1 } else { -1 };
            error2 -= dx * 2;
        }
    }
}




