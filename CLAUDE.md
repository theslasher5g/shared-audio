# shared-audio – Arbeitsanweisungen

Windows-Programm (Rust, Edition 2021): greift per WASAPI-Loopback das Audio einer Quelle ab und
dupliziert es in Echtzeit auf beliebig viele Ausgabegeräte (z. B. zwei Bluetooth-Kopfhörer), mit
Lautstärke, Mute und Delay (0–1000 ms) pro Ziel.

## Build & Qualität

```bash
cargo test                                                   # Kern-Tests (laufen auch unter Linux)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
# Windows-Code (WASAPI, Engine, GUI) von Linux aus nur typprüfen:
rustup target add x86_64-pc-windows-gnu
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
# Echter Build/Start (nur unter Windows):
cargo build --release          # → target/release/shared-audio.exe
```

Die Audio-Pfade lassen sich nur auf einem Windows-Rechner mit echten Geräten testen.
`RUST_LOG=debug` aktiviert Logging.

## Architektur

Plattformunabhängiger Kern (testbar auf jedem OS) – Windows-spezifisch ist nur, was `cfg(windows)` ist.

| Modul | Aufgabe |
|---|---|
| `delay` | `DelayLine`: Ringpuffer-Verzögerung (Stereo, vorallokiert, 0–1000 ms) |
| `channels` | `to_stereo`: Mono/Stereo/5.1/7.1/beliebig → Stereo |
| `resample` | `StereoResampler`: rubato-Sinc, fester Ausgabe-Chunk, Verhältnis zur Laufzeit verstellbar |
| `drift` | `DriftController`: P-Regler Ringfüllstand → relatives Resampling-Verhältnis (±0,5 %) |
| `ring` | Hilfsfunktionen für `ringbuf` (ganze Stereo-Frames, Overrun-Zählung, Verwerfen) |
| `params` | `TargetParams` (Atomics: Lautstärke, Mute, Delay, Pegel, Status, Zähler), `GainRamp` |
| `config` | JSON-Konfiguration in `%APPDATA%\shared-audio\config.json` |
| `devices` *(win)* | Geräteliste, Standardgerät, Geräteauswahl per Nummer/ID/Name |
| `engine` *(win)* | Capture-Thread + Render-Thread je Ziel, Reconnect, Feedback-Schutz |
| `cli` *(win)* | `list`, `run` |
| `gui` *(win)* | eframe/egui-Oberfläche |

### Datenfluss und Threads

```
Loopback ─▶ Capture-Thread ─▶ to_stereo ─▶ SPSC-Ring pro Ziel ─▶ Render-Thread pro Ziel
                                                  Drift-Regler ▶ Resampler ▶ Delay ▶ Gain/Mute ▶ Pegel ▶ WASAPI
```

- **Capture-Thread** (1×): öffnet die Quelle (`None` = Windows-Standard, folgt Wechseln der Standardausgabe),
  liest das Mix-Format, mischt auf Stereo, schreibt in alle Sinks. Bei Stille liefert Loopback keine Pakete →
  bei Event-Timeout (20 ms) werden Nullen nachgefüllt. Hält die Sink-Liste; Add/Remove/SetSource kommen per `mpsc`.
- **Render-Thread** (1× je Ziel): eigene Reconnect-Schleife (Fehler → `Unavailable`, alle 1 s neu versuchen).
  Pro 10-ms-Chunk: Füllstand → Drift-Regler → Resampler → Delay → Gain-Rampe → Peak → WASAPI.
  Bei Unterlauf wird bis zum Soll-Füllstand (40 ms) neu gepuffert, bei > 4× Soll werden die ältesten Samples verworfen.
- **Kein Lock/keine Allokation** in den Audio-Schleifen; Parameter und Pegel sind Atomics (`params.rs`).
  Pegel: `fetch_max` auf die f32-Bits (nicht-negative Floats sind monoton), GUI liest mit `swap(0)`.
- **Rückkopplung:** `Engine::add_target`/`set_source` lehnen dasselbe Gerät ab; Render-Threads vergleichen
  zusätzlich zur Laufzeit den Hash der Quell-ID (Status `Feedback`); die GUI filtert das Quellgerät aus der Auswahl.
- Das Render-Format ist immer Stereo-f32 in der Mix-Samplerate des Geräts (`autoconvert` passt Kanäle/Format an);
  das Capture-Format ist das native Mix-Format der Quelle (Kanalmischung durch `channels::to_stereo`).

## Konventionen

- Sprache in UI, Logs und Doku: Deutsch. Code-Bezeichner: Englisch.
- Neue DSP-Logik zuerst plattformunabhängig + mit Unit-Tests; WASAPI-Code bleibt dünn.
- Vor jedem Commit: `cargo fmt`, `cargo clippy ... -D warnings` (Linux **und** Windows-Target), `cargo test`.
