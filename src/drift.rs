//! Drift-Ausgleich: regelt das Resampling-Verhältnis anhand des Ringpuffer-Füllstands.

/// Maximale relative Abweichung des Verhältnisses (±0,5 %).
pub const MAX_REL_ADJUST: f64 = 0.005;

/// P-Regler mit Glättung. Ist der Puffer voller als das Ziel, wird schneller
/// konsumiert (Verhältnis < 1), ist er leerer, langsamer (> 1).
pub struct DriftController {
    target_frames: f64,
    smoothed: f64,
    alpha: f64,
    gain_per_second: f64,
    rate: f64,
    initialized: bool,
}

impl DriftController {
    /// `target_frames`: Soll-Füllstand in Frames der Quellrate, `rate`: Quellrate in Hz.
    pub fn new(target_frames: usize, rate: u32) -> Self {
        Self {
            target_frames: target_frames as f64,
            smoothed: target_frames as f64,
            alpha: 0.02,
            // 50 ms Abweichung → 0,25 % Anpassung
            gain_per_second: 0.05,
            rate: rate as f64,
            initialized: false,
        }
    }

    /// Füllstand (Frames) melden, liefert das relative Verhältnis (1.0 = unverändert).
    pub fn update(&mut self, fill_frames: usize) -> f64 {
        let f = fill_frames as f64;
        if !self.initialized {
            self.smoothed = f;
            self.initialized = true;
        } else {
            self.smoothed += self.alpha * (f - self.smoothed);
        }
        let err_seconds = (self.smoothed - self.target_frames) / self.rate;
        let adj = (err_seconds * self.gain_per_second).clamp(-MAX_REL_ADJUST, MAX_REL_ADJUST);
        1.0 - adj
    }

    pub fn reset(&mut self) {
        self.initialized = false;
    }

    /// Soll-Füllstand ändern (z. B. wenn die Jitter-Reserve wächst).
    pub fn set_target(&mut self, target_frames: usize) {
        self.target_frames = target_frames as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_target_is_neutral() {
        let mut c = DriftController::new(2000, 48_000);
        for _ in 0..100 {
            assert!((c.update(2000) - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn too_full_speeds_up_consumption() {
        let mut c = DriftController::new(2000, 48_000);
        let r = c.update(6000);
        assert!(r < 1.0);
    }

    #[test]
    fn too_empty_slows_down() {
        let mut c = DriftController::new(2000, 48_000);
        let r = c.update(100);
        assert!(r > 1.0);
    }

    #[test]
    fn adjustment_is_clamped() {
        let mut c = DriftController::new(100, 48_000);
        let r = c.update(10_000_000);
        assert!((1.0 - r - MAX_REL_ADJUST).abs() < 1e-12);
    }

    /// Simuliert ein Gerät, das 0,01 % (100 ppm) schneller läuft: der Füllstand muss beschränkt bleiben.
    #[test]
    fn compensates_clock_offset() {
        let rate = 48_000.0;
        let target = 2000.0;
        let mut ctl = DriftController::new(target as usize, 48_000);
        let mut fill = target;
        let device_speed = 1.0001; // Senke verbraucht 100 ppm mehr
        let mut max_dev: f64 = 0.0;
        for step in 0..60_000 {
            // 10-ms-Schritte, insgesamt 10 Minuten
            let produced = rate * 0.01;
            let rel = ctl.update(fill as usize);
            let consumed = rate * 0.01 * device_speed / rel;
            fill += produced - consumed;
            if step > 30_000 {
                max_dev = max_dev.max((fill - target).abs());
            }
        }
        assert!(max_dev < 500.0, "Füllstand driftet weg: {max_dev}");
    }
}

/// Adaptive Sicherheitsreserve des Jitter-Puffers: startet klein (geringe Latenz) und wächst nur,
/// wenn wiederholt Aussetzer auftreten. Einzelne Aussetzer (z. B. beim Übergang in Stille) bleiben
/// folgenlos.
pub struct JitterReserve {
    reserve_ms: u32,
    max_ms: u32,
    step_ms: u32,
    threshold: u32,
    window: std::time::Duration,
    count: u32,
    window_start: Option<std::time::Instant>,
}

impl JitterReserve {
    pub fn new(start_ms: u32, max_ms: u32) -> Self {
        Self {
            reserve_ms: start_ms,
            max_ms,
            step_ms: 4,
            threshold: 3,
            window: std::time::Duration::from_secs(5),
            count: 0,
            window_start: None,
        }
    }

    pub fn reserve_ms(&self) -> u32 {
        self.reserve_ms
    }

    /// Aussetzer melden. Gibt `true` zurück, wenn die Reserve vergrößert wurde.
    pub fn underrun(&mut self, now: std::time::Instant) -> bool {
        match self.window_start {
            Some(start) if now.duration_since(start) <= self.window => self.count += 1,
            _ => {
                self.window_start = Some(now);
                self.count = 1;
            }
        }
        if self.count >= self.threshold && self.reserve_ms < self.max_ms {
            self.reserve_ms = (self.reserve_ms + self.step_ms).min(self.max_ms);
            self.count = 0;
            self.window_start = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod jitter_tests {
    use super::JitterReserve;
    use std::time::{Duration, Instant};

    #[test]
    fn single_underruns_do_not_grow() {
        let mut j = JitterReserve::new(6, 60);
        let t = Instant::now();
        assert!(!j.underrun(t));
        assert!(!j.underrun(t + Duration::from_secs(10)));
        assert!(!j.underrun(t + Duration::from_secs(20)));
        assert_eq!(j.reserve_ms(), 6);
    }

    #[test]
    fn repeated_underruns_grow_up_to_max() {
        let mut j = JitterReserve::new(6, 12);
        let t = Instant::now();
        j.underrun(t);
        j.underrun(t + Duration::from_millis(100));
        assert!(j.underrun(t + Duration::from_millis(200)));
        assert_eq!(j.reserve_ms(), 10);
        for k in 0..3 {
            j.underrun(t + Duration::from_millis(300 + k));
        }
        assert_eq!(j.reserve_ms(), 12);
        for k in 0..3 {
            j.underrun(t + Duration::from_millis(400 + k));
        }
        assert_eq!(j.reserve_ms(), 12);
    }
}
