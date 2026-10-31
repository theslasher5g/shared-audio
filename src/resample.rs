//! Stereo-Resampler (rubato, Sinc-Interpolation mit Anti-Aliasing) mit festem Ausgabe-Chunk
//! und zur Laufzeit verstellbarem Verhältnis (für den Drift-Ausgleich).

use anyhow::{anyhow, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Maximal zulässige relative Verhältnis-Änderung.
const MAX_RELATIVE: f64 = 1.05;

pub struct StereoResampler {
    inner: Async<f32>,
    out_chunk: usize,
}

impl StereoResampler {
    /// `in_rate` → `out_rate`; jeder Aufruf von [`process`](Self::process) erzeugt genau
    /// `out_chunk` Frames.
    pub fn new(in_rate: u32, out_rate: u32, out_chunk: usize) -> Result<Self> {
        let ratio = out_rate as f64 / in_rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::Blackman2,
        };
        let inner = Async::<f32>::new_sinc(
            ratio,
            MAX_RELATIVE,
            &params,
            out_chunk,
            2,
            FixedAsync::Output,
        )
        .map_err(|e| anyhow!("Resampler: {e}"))?;
        Ok(Self { inner, out_chunk })
    }

    /// Anzahl Eingangs-Frames, die der nächste Aufruf benötigt.
    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    /// Größtmögliche Zahl benötigter Eingangs-Frames (für Pufferdimensionierung).
    pub fn input_frames_max(&self) -> usize {
        self.inner.input_frames_max()
    }

    pub fn out_chunk(&self) -> usize {
        self.out_chunk
    }

    /// Verhältnis relativ zum Nennverhältnis anpassen (1.0 = Nennwert).
    pub fn set_relative_ratio(&mut self, rel: f64) {
        let rel = rel.clamp(1.0 / MAX_RELATIVE, MAX_RELATIVE);
        // Fehler nur bei ungültigen Werten; durch das Clamping ausgeschlossen.
        let _ = self.inner.set_resample_ratio_relative(rel, true);
    }

    /// Verarbeitet `input` (≥ `input_frames_next()` Frames, interleaved Stereo) nach
    /// `output` (≥ `out_chunk` Frames). Gibt (verbrauchte Eingangs-Frames, erzeugte Frames) zurück.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<(usize, usize)> {
        let in_frames = input.len() / 2;
        let out_frames = output.len() / 2;
        let ain = InterleavedSlice::new(input, 2, in_frames).map_err(|e| anyhow!("{e}"))?;
        let mut aout =
            InterleavedSlice::new_mut(output, 2, out_frames).map_err(|e| anyhow!("{e}"))?;
        self.inner
            .process_into_buffer(&ain, &mut aout, None)
            .map_err(|e| anyhow!("Resampler: {e}"))
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, frames: usize, offset: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = (i + offset) as f32 / rate as f32;
                let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    /// Zählt Perioden im linken Kanal (Schmitt-Trigger, ignoriert Rauschen beim Einschwingen).
    fn crossings(data: &[f32]) -> usize {
        let (mut high, mut n) = (false, 0);
        for f in data.chunks_exact(2) {
            if !high && f[0] > 0.2 {
                high = true;
                n += 1;
            } else if high && f[0] < -0.2 {
                high = false;
            }
        }
        n
    }

    fn run(in_rate: u32, out_rate: u32, rel: f64, seconds: usize) -> (Vec<f32>, usize) {
        let chunk = (out_rate / 100) as usize;
        let mut rs = StereoResampler::new(in_rate, out_rate, chunk).unwrap();
        rs.set_relative_ratio(rel);
        let mut out_all = Vec::new();
        let mut out = vec![0.0; chunk * 2];
        let mut consumed = 0usize;
        for _ in 0..seconds * 100 {
            let need = rs.input_frames_next();
            let inp = sine(1000.0, in_rate, need, consumed);
            consumed += need;
            let (_, n) = rs.process(&inp, &mut out).unwrap();
            assert_eq!(n, chunk);
            out_all.extend_from_slice(&out);
        }
        (out_all, consumed)
    }

    #[test]
    fn upsample_44k1_to_48k_preserves_frequency() {
        let (out, consumed) = run(44_100, 48_000, 1.0, 2);
        // 1 kHz: eine Periode pro Millisekunde verbrauchter Eingangszeit (±Einschwingen)
        let expected = consumed as f64 / 44.1;
        let c = crossings(&out) as f64;
        assert!(
            (c - expected).abs() < 5.0,
            "Nulldurchgänge: {c}, erwartet {expected}"
        );
    }

    #[test]
    fn downsample_48k_to_44k1_preserves_frequency() {
        let (out, consumed) = run(48_000, 44_100, 1.0, 2);
        let expected = consumed as f64 / 48.0;
        let c = crossings(&out) as f64;
        assert!(
            (c - expected).abs() < 5.0,
            "Nulldurchgänge: {c}, erwartet {expected}"
        );
    }

    #[test]
    fn consumes_input_in_proportion_to_ratio() {
        let (_, consumed) = run(44_100, 48_000, 1.0, 2);
        let expected = 44_100 * 2;
        assert!(
            (consumed as i64 - expected as i64).abs() < 200,
            "{consumed}"
        );
    }

    #[test]
    fn relative_ratio_changes_consumption() {
        let (_, nominal) = run(48_000, 48_000, 1.0, 2);
        let (_, slower) = run(48_000, 48_000, 1.004, 2); // mehr Output pro Input → weniger Input
        assert!(slower < nominal, "{slower} !< {nominal}");
    }

    #[test]
    fn output_is_not_silent_and_not_clipping() {
        let (out, _) = run(44_100, 48_000, 1.0, 1);
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.4 && peak < 0.6, "Peak {peak}");
    }
}
