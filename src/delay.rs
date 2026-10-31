//! Delay-Line für interleaved Stereo-Frames (vorallokiert, ohne Allokation im Betrieb).

/// Maximal einstellbare Verzögerung in Millisekunden.
pub const MAX_DELAY_MS: u32 = 1000;

/// Ringpuffer-basierte Verzögerung. Arbeitet in Frames (je 2 × f32).
pub struct DelayLine {
    buf: Vec<f32>,
    cap_frames: usize,
    write: usize,
    delay: usize,
}

impl DelayLine {
    /// `max_delay_frames`: größte erlaubte Verzögerung in Frames.
    pub fn new(max_delay_frames: usize) -> Self {
        let cap_frames = max_delay_frames + 1;
        Self {
            buf: vec![0.0; cap_frames * 2],
            cap_frames,
            write: 0,
            delay: 0,
        }
    }

    /// Delay-Line für eine Samplerate mit [`MAX_DELAY_MS`] Maximum.
    pub fn for_rate(sample_rate: u32) -> Self {
        Self::new(ms_to_frames(MAX_DELAY_MS, sample_rate))
    }

    pub fn max_delay_frames(&self) -> usize {
        self.cap_frames - 1
    }

    pub fn delay_frames(&self) -> usize {
        self.delay
    }

    /// Setzt die Verzögerung (wird auf das Maximum begrenzt).
    pub fn set_delay_frames(&mut self, frames: usize) {
        self.delay = frames.min(self.max_delay_frames());
    }

    /// Schreibt die Frames in `data` in die Line und ersetzt sie durch die verzögerten.
    pub fn process_in_place(&mut self, data: &mut [f32]) {
        let cap = self.cap_frames;
        for frame in data.chunks_exact_mut(2) {
            let w = self.write;
            self.buf[2 * w] = frame[0];
            self.buf[2 * w + 1] = frame[1];
            let r = (w + cap - self.delay) % cap;
            frame[0] = self.buf[2 * r];
            frame[1] = self.buf[2 * r + 1];
            self.write = if w + 1 == cap { 0 } else { w + 1 };
        }
    }

    /// Leert die Line (Stille).
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

pub fn ms_to_frames(ms: u32, sample_rate: u32) -> usize {
    (ms as u64 * sample_rate as u64 / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| [i as f32 + 1.0, -(i as f32 + 1.0)])
            .collect()
    }

    #[test]
    fn zero_delay_is_passthrough() {
        let mut d = DelayLine::new(16);
        let mut x = ramp(8);
        let orig = x.clone();
        d.process_in_place(&mut x);
        assert_eq!(x, orig);
    }

    #[test]
    fn delays_by_n_frames() {
        let mut d = DelayLine::new(16);
        d.set_delay_frames(3);
        let mut x = ramp(8);
        d.process_in_place(&mut x);
        // erste 3 Frames Stille, danach Frame 1, 2, ...
        assert_eq!(&x[..6], &[0.0; 6]);
        assert_eq!(x[6], 1.0);
        assert_eq!(x[7], -1.0);
        assert_eq!(x[14], 5.0);
    }

    #[test]
    fn delay_persists_across_blocks() {
        let mut d = DelayLine::new(64);
        d.set_delay_frames(10);
        let all = ramp(40);
        let mut out = Vec::new();
        for chunk in all.chunks(14) {
            let mut c = chunk.to_vec();
            d.process_in_place(&mut c);
            out.extend(c);
        }
        let mut expect = vec![0.0; 20];
        expect.extend_from_slice(&all[..all.len() - 20]);
        assert_eq!(out, expect);
    }

    #[test]
    fn delay_is_clamped_to_max() {
        let mut d = DelayLine::new(5);
        d.set_delay_frames(100);
        assert_eq!(d.delay_frames(), 5);
    }

    #[test]
    fn max_delay_1000ms_at_48k() {
        let d = DelayLine::for_rate(48_000);
        assert_eq!(d.max_delay_frames(), 48_000);
        assert_eq!(ms_to_frames(250, 44_100), 11_025);
    }

    #[test]
    fn wraps_around_buffer() {
        let mut d = DelayLine::new(4);
        d.set_delay_frames(4);
        let all = ramp(30);
        let mut x = all.clone();
        d.process_in_place(&mut x);
        assert_eq!(&x[8..], &all[..all.len() - 8]);
    }
}
