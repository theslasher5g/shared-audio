//! Kanalanpassung: beliebige Kanalzahl (interleaved) → Stereo.

const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Mischt `input` (interleaved, `in_ch` Kanäle) in `out` (interleaved Stereo).
/// `out.len()` muss `2 * frames` entsprechen, `input.len()` `in_ch * frames`.
///
/// - 1 Kanal: Mono wird auf L und R dupliziert
/// - 2 Kanäle: Kopie
/// - ≥ 3 Kanäle: Standardbelegung (FL FR FC LFE BL BR SL SR) wird abgemischt,
///   LFE verworfen, mit Normierung gegen Übersteuerung. Bei unbekannten
///   Kanalzahlen: gerade Kanäle → L, ungerade → R.
pub fn to_stereo(input: &[f32], in_ch: usize, out: &mut [f32]) {
    assert!(in_ch >= 1);
    let frames = out.len() / 2;
    assert_eq!(input.len(), frames * in_ch);
    match in_ch {
        1 => {
            for (o, &s) in out.chunks_exact_mut(2).zip(input) {
                o[0] = s;
                o[1] = s;
            }
        }
        2 => out.copy_from_slice(input),
        6 | 8 => {
            // Normierung: Summe der Gewichte eines Ausgangskanals
            let norm = if in_ch == 6 {
                1.0 / (1.0 + SQRT_HALF + SQRT_HALF)
            } else {
                1.0 / (1.0 + SQRT_HALF + SQRT_HALF + SQRT_HALF)
            };
            for (o, f) in out.chunks_exact_mut(2).zip(input.chunks_exact(in_ch)) {
                let (fl, fr, fc, bl, br) = (f[0], f[1], f[2], f[4], f[5]);
                let mut l = fl + SQRT_HALF * fc + SQRT_HALF * bl;
                let mut r = fr + SQRT_HALF * fc + SQRT_HALF * br;
                if in_ch == 8 {
                    l += SQRT_HALF * f[6];
                    r += SQRT_HALF * f[7];
                }
                o[0] = l * norm;
                o[1] = r * norm;
            }
        }
        n => {
            let left = n.div_ceil(2);
            let right = n / 2;
            for (o, f) in out.chunks_exact_mut(2).zip(input.chunks_exact(n)) {
                let (mut l, mut r) = (0.0, 0.0);
                for (i, &s) in f.iter().enumerate() {
                    if i % 2 == 0 {
                        l += s;
                    } else {
                        r += s;
                    }
                }
                o[0] = l / left as f32;
                o[1] = r / right as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated() {
        let mut out = [0.0; 6];
        to_stereo(&[0.1, 0.2, 0.3], 1, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
    }

    #[test]
    fn stereo_is_copied() {
        let inp = [0.1, -0.1, 0.5, 0.6];
        let mut out = [0.0; 4];
        to_stereo(&inp, 2, &mut out);
        assert_eq!(out, inp);
    }

    #[test]
    fn surround_51_front_only() {
        // nur FL → nur links
        let mut out = [9.0; 2];
        to_stereo(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], 6, &mut out);
        assert!(out[0] > 0.4 && out[1] == 0.0);
    }

    #[test]
    fn surround_51_full_scale_does_not_clip() {
        let mut out = [0.0; 2];
        to_stereo(&[1.0; 6], 6, &mut out);
        assert!(out[0] <= 1.0001 && out[1] <= 1.0001);
        assert!((out[0] - out[1]).abs() < 1e-6);
    }

    #[test]
    fn surround_51_drops_lfe() {
        let mut out = [9.0; 2];
        to_stereo(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 6, &mut out);
        assert_eq!(out, [0.0, 0.0]);
    }

    #[test]
    fn surround_71_uses_side_channels() {
        let mut out = [0.0; 2];
        to_stereo(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0], 8, &mut out);
        assert!(out[0] > 0.0 && out[1] == 0.0);
    }

    #[test]
    fn odd_channel_count_fallback() {
        let mut out = [0.0; 2];
        to_stereo(&[1.0, 0.5, 1.0], 3, &mut out);
        assert_eq!(out, [1.0, 0.5]);
    }
}
