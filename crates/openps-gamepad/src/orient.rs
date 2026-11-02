//! Orientation from gyro + accelerometer (Mahony complementary filter). GameController gives rotation rate and acceleration but no
//! attitude for the DualSense, and the console wants a quaternion. Frame: x right, y up, z toward the player; flat at rest = identity,
//! accelerometer reads +1 g on y.

#[derive(Clone, Copy, Debug)]
pub struct Orient {
    q: [f32; 4], // x, y, z, w
}

const GAIN: f32 = 0.5;

impl Default for Orient {
    fn default() -> Self {
        Self { q: [0.0, 0.0, 0.0, 1.0] }
    }
}

impl Orient {
    /// `gyro` in rad/s, `accel` in g, `dt` in seconds since the previous call.
    pub fn update(&mut self, gyro: [f32; 3], accel: [f32; 3], dt: f32) -> [f32; 4] {
        let [qx, qy, qz, qw] = self.q;
        let mut g = gyro;
        let n = (accel[0] * accel[0] + accel[1] * accel[1] + accel[2] * accel[2]).sqrt();
        if n > 0.3 {
            let a = [accel[0] / n, accel[1] / n, accel[2] / n];
            // gravity (0, 1, 0) in the body frame for the current estimate
            let v = [2.0 * (qx * qy + qw * qz), qw * qw - qx * qx + qy * qy - qz * qz, 2.0 * (qy * qz - qw * qx)];
            let e = [a[1] * v[2] - a[2] * v[1], a[2] * v[0] - a[0] * v[2], a[0] * v[1] - a[1] * v[0]];
            for i in 0..3 {
                g[i] += GAIN * e[i];
            }
        }
        let h = 0.5 * dt;
        let nq = [
            qx + h * (qw * g[0] + qy * g[2] - qz * g[1]),
            qy + h * (qw * g[1] + qz * g[0] - qx * g[2]),
            qz + h * (qw * g[2] + qx * g[1] - qy * g[0]),
            qw + h * (-qx * g[0] - qy * g[1] - qz * g[2]),
        ];
        let m = nq.iter().map(|v| v * v).sum::<f32>().sqrt();
        self.q = if m > 0.0 { [nq[0] / m, nq[1] / m, nq[2] / m, nq[3] / m] } else { [0.0, 0.0, 0.0, 1.0] };
        self.q
    }
}
