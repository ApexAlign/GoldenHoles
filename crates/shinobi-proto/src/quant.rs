//! Fixed-point quantisation shared by client and server. Both sides must use these exact functions.

use std::f32::consts::TAU;

/// Position unit: 1/512 m (~2 mm). i32 covers +-4 km.
pub const POS_SCALE: f32 = 512.0;
/// Velocity unit: 1/64 m/s. i16 covers +-512 m/s (dash is 30 m/s).
pub const VEL_SCALE: f32 = 64.0;

pub fn q_pos(v: f32) -> i32 {
    (v * POS_SCALE).round().clamp(-(1 << 30) as f32, (1 << 30) as f32) as i32
}

pub fn dq_pos(v: i32) -> f32 {
    v as f32 / POS_SCALE
}

pub fn q_pos3(v: [f32; 3]) -> [i32; 3] {
    [q_pos(v[0]), q_pos(v[1]), q_pos(v[2])]
}

pub fn dq_pos3(v: [i32; 3]) -> [f32; 3] {
    [dq_pos(v[0]), dq_pos(v[1]), dq_pos(v[2])]
}

pub fn q_vel(v: f32) -> i16 {
    (v * VEL_SCALE).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

pub fn dq_vel(v: i16) -> f32 {
    v as f32 / VEL_SCALE
}

/// Radians -> 0..65535 (wraps).
pub fn q_yaw(rad: f32) -> u16 {
    let t = rad.rem_euclid(TAU) / TAU;
    ((t * 65536.0).round() as u32 & 0xffff) as u16
}

pub fn dq_yaw(v: u16) -> f32 {
    v as f32 / 65536.0 * TAU
}

/// Stick axis -1..1 -> -127..127.
pub fn q_axis(v: f32) -> i8 {
    (v.clamp(-1.0, 1.0) * 127.0).round() as i8
}

pub fn dq_axis(v: i8) -> f32 {
    (v.max(-127) as f32) / 127.0
}

/// Pitch radians (-pi/2..pi/2) -> 12-bit code (see `intent::PITCH_BITS`). Out-of-range input saturates.
pub fn q_pitch(rad: f32) -> u16 {
    use std::f32::consts::FRAC_PI_2;
    let t = (rad.clamp(-FRAC_PI_2, FRAC_PI_2) + FRAC_PI_2) / std::f32::consts::PI;
    (t * super::intent::PITCH_MAX as f32).round() as u16
}

pub fn dq_pitch(v: u16) -> f32 {
    let v = v.min(super::intent::PITCH_MAX);
    v as f32 / super::intent::PITCH_MAX as f32 * std::f32::consts::PI - std::f32::consts::FRAC_PI_2
}

#[cfg(test)]
mod pitch_tests {
    use super::*;

    #[test]
    fn pitch_roundtrip_and_saturation() {
        for deg in [-90.0f32, -45.0, 0.0, 12.3, 89.0, 90.0] {
            let back = dq_pitch(q_pitch(deg.to_radians())).to_degrees();
            assert!((back - deg).abs() < 0.05, "{deg} -> {back}");
        }
        assert_eq!(q_pitch(10.0), super::super::intent::PITCH_MAX);
        assert_eq!(q_pitch(-10.0), 0);
    }
}
