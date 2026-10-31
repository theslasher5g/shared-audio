//! Hilfen für den Umgang mit den SPSC-Ringpuffern zwischen Capture und Render.

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

/// Erzeugt einen Ringpuffer für `frames` Stereo-Frames (2 × f32 pro Frame).
pub fn stereo_ring(frames: usize) -> (HeapProd<f32>, HeapCons<f32>) {
    HeapRb::<f32>::new(frames * 2).split()
}

/// Schreibt Stereo-Frames; gibt die Zahl **nicht** geschriebener Frames zurück (Overrun).
/// Es werden nur ganze Frames geschrieben.
pub fn push_frames(prod: &mut HeapProd<f32>, data: &[f32]) -> usize {
    let free_frames = prod.vacant_len() / 2;
    let want = data.len() / 2;
    let n = want.min(free_frames);
    prod.push_slice(&data[..n * 2]);
    want - n
}

/// Zahl der lesbaren ganzen Frames.
pub fn frames_available(cons: &HeapCons<f32>) -> usize {
    cons.occupied_len() / 2
}

/// Liest genau `out.len() / 2` Frames, falls vorhanden; sonst nichts (gibt `false` zurück).
pub fn pop_frames_exact(cons: &mut HeapCons<f32>, out: &mut [f32]) -> bool {
    if cons.occupied_len() < out.len() {
        return false;
    }
    cons.pop_slice(out) == out.len()
}

/// Verwirft bis zu `frames` Frames am Anfang (älteste zuerst). Gibt die Zahl verworfener Frames zurück.
pub fn discard_frames(cons: &mut HeapCons<f32>, frames: usize) -> usize {
    cons.skip(frames * 2) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_roundtrip() {
        let (mut p, mut c) = stereo_ring(8);
        assert_eq!(push_frames(&mut p, &[1.0, 2.0, 3.0, 4.0]), 0);
        assert_eq!(frames_available(&c), 2);
        let mut out = [0.0; 4];
        assert!(pop_frames_exact(&mut c, &mut out));
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(frames_available(&c), 0);
    }

    #[test]
    fn overrun_reports_missing_frames_and_keeps_alignment() {
        let (mut p, c) = stereo_ring(4);
        let data: Vec<f32> = (0..12).map(|i| i as f32).collect(); // 6 Frames
        assert_eq!(push_frames(&mut p, &data), 2);
        assert_eq!(frames_available(&c), 4);
    }

    #[test]
    fn exact_pop_does_nothing_when_short() {
        let (mut p, mut c) = stereo_ring(8);
        push_frames(&mut p, &[1.0, 1.0]);
        let mut out = [0.0; 4];
        assert!(!pop_frames_exact(&mut c, &mut out));
        assert_eq!(frames_available(&c), 1);
    }

    #[test]
    fn discard_drops_oldest() {
        let (mut p, mut c) = stereo_ring(8);
        push_frames(&mut p, &[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        assert_eq!(discard_frames(&mut c, 2), 2);
        let mut out = [0.0; 2];
        assert!(pop_frames_exact(&mut c, &mut out));
        assert_eq!(out, [3.0, 3.0]);
    }

    #[test]
    fn works_across_threads_without_loss() {
        let (mut p, mut c) = stereo_ring(1024);
        let h = std::thread::spawn(move || {
            let mut i = 0u32;
            while i < 20_000 {
                let frame = [i as f32, i as f32];
                if push_frames(&mut p, &frame) == 0 {
                    i += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });
        let mut next = 0u32;
        let mut out = [0.0; 2];
        while next < 20_000 {
            if pop_frames_exact(&mut c, &mut out) {
                assert_eq!(out[0], next as f32);
                next += 1;
            } else {
                std::thread::yield_now();
            }
        }
        h.join().unwrap();
    }
}
