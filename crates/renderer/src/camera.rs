use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

/// Orbit camera: rotate around a target point with mouse drag + scroll zoom.
pub struct OrbitCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub radius: f32,
    pub target: Vec3,
    pub aspect: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl OrbitCamera {
    pub fn new(radius: f32, aspect: f32) -> Self {
        OrbitCamera {
            yaw: 0.0,
            pitch: 0.3,
            radius,
            target: Vec3::ZERO,
            aspect,
            fov_y: 45.0_f32.to_radians(),
            near: 0.1,
            far: 1000.0,
        }
    }

    pub fn eye(&self) -> Vec3 {
        let x = self.radius * self.pitch.cos() * self.yaw.sin();
        let y = self.radius * self.pitch.sin();
        let z = self.radius * self.pitch.cos() * self.yaw.cos();
        self.target + Vec3::new(x, y, z)
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, self.aspect, self.near, self.far)
    }

    pub fn view_projection(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }

    /// Rotate camera by mouse delta (in pixels). Sensitivity controls rotation speed.
    pub fn rotate(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.yaw += dx * sensitivity;
        self.pitch = (self.pitch - dy * sensitivity).clamp(
            -std::f32::consts::FRAC_PI_2 + 0.01,
            std::f32::consts::FRAC_PI_2 - 0.01,
        );
    }

    /// Zoom camera by scroll delta.
    pub fn zoom(&mut self, delta: f32) {
        self.radius = (self.radius - delta * self.radius * 0.1).clamp(0.1, 10000.0);
    }

    pub fn uniforms(&self) -> CameraUniforms {
        let eye = self.eye();
        CameraUniforms {
            view_proj: self.view_projection().to_cols_array_2d(),
            eye_pos: [eye.x, eye.y, eye.z, 0.0],
        }
    }
}

/// Camera uniform data sent to the GPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub eye_pos: [f32; 4],
}
