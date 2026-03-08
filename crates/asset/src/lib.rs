use glam::{Vec2, Vec3};
use octo_core::{FaceVertex, Model};
use std::fs::File;
use std::io::{BufRead, BufReader};

/// Parse a face vertex specification like "v", "v/vt", "v/vt/vn", "v//vn".
/// Returns None if the vertex index cannot be parsed.
fn parse_face_vertex(
    s: &str,
    num_verts: usize,
    num_texcoords: usize,
    num_normals: usize,
) -> Option<FaceVertex> {
    let parts: Vec<&str> = s.split('/').collect();

    let v = resolve_index(parts[0], num_verts)?;

    let vt = parts.get(1).and_then(|s| {
        if s.is_empty() {
            None
        } else {
            resolve_index(s, num_texcoords)
        }
    });

    let vn = parts.get(2).and_then(|s| {
        if s.is_empty() {
            None
        } else {
            resolve_index(s, num_normals)
        }
    });

    Some(FaceVertex { v, vt, vn })
}

/// Resolve a 1-based (or negative/relative) OBJ index to a 0-based index.
fn resolve_index(s: &str, count: usize) -> Option<usize> {
    let idx: isize = s.parse().ok()?;
    if idx > 0 {
        Some((idx - 1) as usize)
    } else if idx < 0 {
        // Negative index: -1 means last element
        let abs = (-idx) as usize;
        if abs <= count {
            Some(count - abs)
        } else {
            None
        }
    } else {
        // OBJ indices are 1-based; 0 is invalid
        None
    }
}

/// Load an OBJ file into a Model.
pub fn load_obj(filename: &str) -> Result<Model, String> {
    let file =
        File::open(filename).map_err(|e| format!("Failed to open OBJ file '{}': {}", filename, e))?;
    let reader = BufReader::new(file);
    let mut vertices = Vec::new();
    let mut texcoords = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::new();

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                log::warn!("{}:{}: failed to read line: {}", filename, line_num + 1, e);
                continue;
            }
        };
        let line = line.trim();
        if line.starts_with("f ") {
            let mut parts = line.split_whitespace();
            parts.next();
            let mut face_verts = Vec::new();
            let mut malformed = false;
            for part in parts {
                match parse_face_vertex(part, vertices.len(), texcoords.len(), normals.len()) {
                    Some(fv) => face_verts.push(fv),
                    None => {
                        log::warn!(
                            "{}:{}: skipping face with malformed vertex '{}'",
                            filename,
                            line_num + 1,
                            part
                        );
                        malformed = true;
                        break;
                    }
                }
            }
            if malformed {
                continue;
            }
            // Fan triangulation: for N vertices, emit N-2 triangles
            for i in 1..face_verts.len().saturating_sub(1) {
                faces.push([face_verts[0], face_verts[i], face_verts[i + 1]]);
            }
        } else if line.starts_with("vt ") {
            let mut parts = line.split_whitespace();
            parts.next();
            match (parts.next(), parts.next()) {
                (Some(u_str), Some(v_str)) => {
                    match (u_str.parse::<f32>(), v_str.parse::<f32>()) {
                        (Ok(u), Ok(v)) => texcoords.push(Vec2::new(u, v)),
                        _ => {
                            log::warn!(
                                "{}:{}: skipping malformed texcoord line",
                                filename,
                                line_num + 1
                            );
                        }
                    }
                }
                _ => {
                    log::warn!(
                        "{}:{}: skipping incomplete texcoord line",
                        filename,
                        line_num + 1
                    );
                }
            }
        } else if line.starts_with("vn ") {
            let mut parts = line.split_whitespace();
            parts.next();
            match (parts.next(), parts.next(), parts.next()) {
                (Some(x_str), Some(y_str), Some(z_str)) => {
                    match (
                        x_str.parse::<f32>(),
                        y_str.parse::<f32>(),
                        z_str.parse::<f32>(),
                    ) {
                        (Ok(x), Ok(y), Ok(z)) => normals.push(Vec3::new(x, y, z)),
                        _ => {
                            log::warn!(
                                "{}:{}: skipping malformed normal line",
                                filename,
                                line_num + 1
                            );
                        }
                    }
                }
                _ => {
                    log::warn!(
                        "{}:{}: skipping incomplete normal line",
                        filename,
                        line_num + 1
                    );
                }
            }
        } else if line.starts_with("v ") {
            let mut parts = line.split_whitespace();
            parts.next();
            match (parts.next(), parts.next(), parts.next()) {
                (Some(x_str), Some(y_str), Some(z_str)) => {
                    match (
                        x_str.parse::<f32>(),
                        y_str.parse::<f32>(),
                        z_str.parse::<f32>(),
                    ) {
                        (Ok(x), Ok(y), Ok(z)) => vertices.push(Vec3::new(x, y, z)),
                        _ => {
                            log::warn!(
                                "{}:{}: skipping malformed vertex line",
                                filename,
                                line_num + 1
                            );
                        }
                    }
                }
                _ => {
                    log::warn!(
                        "{}:{}: skipping incomplete vertex line",
                        filename,
                        line_num + 1
                    );
                }
            }
        }
    }

    // Validate face indices
    for (fi, face) in faces.iter().enumerate() {
        for fv in face {
            if fv.v >= vertices.len() {
                return Err(format!(
                    "{}: face {} references vertex index {} but only {} vertices exist",
                    filename,
                    fi,
                    fv.v,
                    vertices.len()
                ));
            }
            if let Some(vt) = fv.vt {
                if vt >= texcoords.len() {
                    return Err(format!(
                        "{}: face {} references texcoord index {} but only {} texcoords exist",
                        filename,
                        fi,
                        vt,
                        texcoords.len()
                    ));
                }
            }
            if let Some(vn) = fv.vn {
                if vn >= normals.len() {
                    return Err(format!(
                        "{}: face {} references normal index {} but only {} normals exist",
                        filename,
                        fi,
                        vn,
                        normals.len()
                    ));
                }
            }
        }
    }

    Ok(Model {
        vertices,
        texcoords,
        normals,
        faces,
    })
}
