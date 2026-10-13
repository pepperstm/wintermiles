//! Road-speed model: what a given power does on a given gradient.

#[derive(Debug, Clone, Copy)]
pub struct PhysicsParams {
    /// Rider + bike, kg.
    pub mass: f64,
    pub crr: f64,
    /// Drag area, m².
    pub cda: f64,
    /// Top speed cap in m/s — stands in for braking on descents, since corners aren't modelled.
    pub max_speed: f64,
}

const G: f64 = 9.80665;
const DRIVETRAIN_EFFICIENCY: f64 = 0.975;
/// Wheels add rotational inertia roughly equal to their rim mass again.
const ROTATING_MASS: f64 = 1.4;

/// Air density falls with altitude; riding at 2000 m is noticeably faster for the same power.
pub fn air_density(elevation: f64) -> f64 {
    1.225 * (-elevation.max(0.0) / 8500.0).exp()
}

/// Advances road speed (m/s) by `dt` seconds for a given pedalling power and gradient.
pub fn step_speed(mut v: f64, power: f64, grade: f64, elevation: f64, p: &PhysicsParams, dt: f64) -> f64 {
    let rho = air_density(elevation);
    let cos = 1.0 / (1.0 + grade * grade).sqrt();
    let sin = grade * cos;
    let mut remaining = dt;
    while remaining > 1e-9 {
        let h = remaining.min(0.1);
        remaining -= h;
        // Dividing by a floor speed caps the drive force from a standstill.
        let drive = power * DRIVETRAIN_EFFICIENCY / v.max(1.5);
        let resist = p.mass * G * (sin + p.crr * cos) + 0.5 * rho * p.cda * v * v;
        v = (v + (drive - resist) / (p.mass + ROTATING_MASS) * h).clamp(0.0, p.max_speed);
    }
    v
}

/// Aerodynamic coefficient in kg/m, the form trainers expect for simulation mode.
pub fn wind_coefficient(cda: f64, elevation: f64) -> f64 {
    0.5 * air_density(elevation) * cda
}
