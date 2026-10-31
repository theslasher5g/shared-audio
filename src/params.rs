//! Zwischen GUI/CLI und Audio-Threads geteilter Zustand (nur Atomics, lock-frei).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};

/// Verbindungszustand eines Ziels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TargetStatus {
    Stopped = 0,
    Connecting = 1,
    Active = 2,
    /// Gerät getrennt/nicht vorhanden – es wird automatisch neu verbunden.
    Unavailable = 3,
    /// Gerät ist gleichzeitig die Quelle (Rückkopplung).
    Feedback = 4,
}

impl TargetStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Connecting,
            2 => Self::Active,
            3 => Self::Unavailable,
            4 => Self::Feedback,
            _ => Self::Stopped,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Gestoppt",
            Self::Connecting => "Verbinde …",
            Self::Active => "Aktiv",
            Self::Unavailable => "Nicht verfügbar (neuer Versuch läuft)",
            Self::Feedback => "Gesperrt: ist die Quelle",
        }
    }
}

/// Lautstärke-Regler (0..=1) → Verstärkung mit quadratischer Kennlinie.
pub fn volume_to_gain(volume: f32) -> f32 {
    let v = volume.clamp(0.0, 1.0);
    v * v
}

/// Parameter und Messwerte eines Ziels.
pub struct TargetParams {
    volume: AtomicU32,
    muted: AtomicBool,
    delay_ms: AtomicU32,
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    status: AtomicU8,
    pub underruns: AtomicU64,
    pub overruns: AtomicU64,
    pub dropped_frames: AtomicU64,
}

impl TargetParams {
    pub fn new(volume: f32, muted: bool, delay_ms: u32) -> Self {
        Self {
            volume: AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()),
            muted: AtomicBool::new(muted),
            delay_ms: AtomicU32::new(delay_ms.min(crate::delay::MAX_DELAY_MS)),
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
            status: AtomicU8::new(TargetStatus::Stopped as u8),
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Relaxed)
    }
    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Relaxed);
    }
    pub fn delay_ms(&self) -> u32 {
        self.delay_ms.load(Relaxed)
    }
    pub fn set_delay_ms(&self, ms: u32) {
        self.delay_ms
            .store(ms.min(crate::delay::MAX_DELAY_MS), Relaxed);
    }
    pub fn status(&self) -> TargetStatus {
        TargetStatus::from_u8(self.status.load(Relaxed))
    }
    pub fn set_status(&self, s: TargetStatus) {
        self.status.store(s as u8, Relaxed);
    }

    /// Gewünschte Verstärkung (0 bei Mute).
    pub fn effective_gain(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            volume_to_gain(self.volume())
        }
    }

    /// Audio-Thread: Spitzenpegel melden (Maximum seit dem letzten Auslesen).
    /// Nicht-negative Floats sind als u32-Bits monoton, daher genügt `fetch_max`.
    pub fn report_peak(&self, l: f32, r: f32) {
        self.peak_l.fetch_max(l.to_bits(), Relaxed);
        self.peak_r.fetch_max(r.to_bits(), Relaxed);
    }

    /// GUI: Spitzenpegel seit dem letzten Aufruf (L, R) auslesen und zurücksetzen.
    pub fn take_peak(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak_l.swap(0, Relaxed)),
            f32::from_bits(self.peak_r.swap(0, Relaxed)),
        )
    }
}

/// Betrag-Spitze eines interleaved Stereo-Blocks (L, R).
pub fn peak_stereo(data: &[f32]) -> (f32, f32) {
    let (mut l, mut r) = (0.0f32, 0.0f32);
    for f in data.chunks_exact(2) {
        l = l.max(f[0].abs());
        r = r.max(f[1].abs());
    }
    (l, r)
}

/// Linearer Spitzenwert → Anzeigewert 0..1 (−60 dB … 0 dB).
pub fn level_to_meter(peak: f32) -> f32 {
    if peak <= 0.000_001 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Gain mit linearer Rampe über einen Block (verhindert Knackser bei Änderungen).
pub struct GainRamp {
    current: f32,
}

impl GainRamp {
    pub fn new(initial: f32) -> Self {
        Self { current: initial }
    }

    pub fn apply(&mut self, data: &mut [f32], target: f32) {
        let frames = data.len() / 2;
        if frames == 0 {
            return;
        }
        if (self.current - target).abs() < 1e-6 {
            self.current = target;
            if target != 1.0 {
                for s in data.iter_mut() {
                    *s *= target;
                }
            }
            return;
        }
        let start = self.current;
        let delta = target - start;
        for (i, f) in data.chunks_exact_mut(2).enumerate() {
            let g = start + delta * ((i + 1) as f32 / frames as f32);
            f[0] *= g;
            f[1] *= g;
        }
        self.current = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_overrides_volume() {
        let p = TargetParams::new(0.8, false, 0);
        assert!(p.effective_gain() > 0.0);
        p.set_muted(true);
        assert_eq!(p.effective_gain(), 0.0);
    }

    #[test]
    fn delay_and_volume_are_clamped() {
        let p = TargetParams::new(5.0, false, 5000);
        assert_eq!(p.volume(), 1.0);
        assert_eq!(p.delay_ms(), 1000);
        p.set_delay_ms(77);
        assert_eq!(p.delay_ms(), 77);
    }

    #[test]
    fn peak_keeps_maximum_until_taken() {
        let p = TargetParams::new(1.0, false, 0);
        p.report_peak(0.2, 0.9);
        p.report_peak(0.5, 0.1);
        assert_eq!(p.take_peak(), (0.5, 0.9));
        assert_eq!(p.take_peak(), (0.0, 0.0));
    }

    #[test]
    fn gain_ramp_is_smooth_and_reaches_target() {
        let mut g = GainRamp::new(1.0);
        let mut x = vec![1.0; 200];
        g.apply(&mut x, 0.0);
        assert!(x[0] < 1.0 && x[0] > 0.9);
        assert_eq!(x[198], 0.0);
        let mut y = vec![1.0; 20];
        g.apply(&mut y, 0.0);
        assert!(y.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn peak_stereo_works() {
        assert_eq!(peak_stereo(&[0.1, -0.7, -0.3, 0.2]), (0.3, 0.7));
    }

    #[test]
    fn meter_scale() {
        assert_eq!(level_to_meter(0.0), 0.0);
        assert!((level_to_meter(1.0) - 1.0).abs() < 1e-6);
        assert!((level_to_meter(0.1) - 40.0 / 60.0).abs() < 1e-4);
        assert_eq!(level_to_meter(0.0001), 0.0);
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            TargetStatus::Stopped,
            TargetStatus::Connecting,
            TargetStatus::Active,
            TargetStatus::Unavailable,
            TargetStatus::Feedback,
        ] {
            assert_eq!(TargetStatus::from_u8(s as u8), s);
        }
    }
}
