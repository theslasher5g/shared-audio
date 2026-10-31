//! Audio-Engine: ein Capture-Thread (Loopback der Quelle) und ein Render-Thread je Ziel.
//!
//! Datenfluss:
//! ```text
//! Loopback ─▶ Capture-Thread ─▶ Stereo-Mix ─▶ SPSC-Ring (je Ziel) ─▶ Render-Thread
//!                                                 Drift-Regler ▶ Resampler ▶ Delay ▶ Gain ▶ WASAPI
//! ```
//! Im Audiopfad (Schleifen in `capture_session` / `render_loop`) gibt es keine Locks und
//! keine Allokationen: alle Puffer werden beim Sitzungsstart angelegt, der Austausch läuft
//! über lock-freie Ringe und Atomics. Strukturänderungen (Ziel hinzufügen/entfernen, Quelle
//! wechseln) laufen über einen Kommando-Kanal und werden außerhalb der Paketverarbeitung
//! abgearbeitet.

use crate::channels::to_stereo;
use crate::delay::{ms_to_frames, DelayLine};
use crate::devices::{self, id_hash};
use crate::drift::{DriftController, JitterReserve};
use crate::params::{peak_stereo, GainRamp, TargetParams, TargetStatus};
use crate::resample::StereoResampler;
use crate::ring::{discard_frames, frames_available, pop_frames_exact, push_frames, stereo_ring};
use anyhow::{bail, Result};
use ringbuf::HeapProd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasapi::{
    BufferFlags, DeviceEnumerator, DeviceState, Direction, Role, SampleType, StreamMode, WaveFormat,
};

/// Größe des Ringpuffers pro Ziel.
const RING_MS: usize = 500;
/// Jitter-Reserve des Rings über (Chunk + halbe Capture-Periode) hinaus: Start, Schritt, Maximum.
const RING_RESERVE_MS: (u32, u32, u32) = (4, 3, 80);
/// Reserve im Gerätepuffer über eine WASAPI-Periode hinaus: Start, Schritt, Maximum.
const DEVICE_RESERVE_MS: (u32, u32, u32) = (4, 3, 40);
/// Liegt der Füllstand so viel über dem Soll (mind. 2 Capture-Perioden), wird verworfen.
const DISCARD_EXCESS_MS: usize = 30;
/// Verarbeitungs-Chunk des Renderers (klein = geringe Latenz).
const CHUNK_MS: u32 = 2;
/// Nach einem Ring-Unterlauf so lange warten, ob die Quelle nur verstummt ist.
const UNDERRUN_VERIFY: Duration = Duration::from_millis(60);
/// Wartezeit nach einem Fehler, bevor neu verbunden wird.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Zustand der Quelle, lock-frei lesbar für die Render-Threads.
pub struct SourceState {
    /// Samplerate der Quelle in Hz, 0 = keine Verbindung.
    rate: AtomicU32,
    /// Hash der Geräte-ID der aktuell geöffneten Quelle, 0 = unbekannt.
    id_hash: AtomicU64,
    status: AtomicU32,
    /// Capture-Periode der Quelle in Mikrosekunden (Paketabstand des Loopbacks).
    period_us: AtomicU32,
    /// Zähler echter Loopback-Pakete (zur Unterscheidung Aussetzer ↔ Quelle verstummt).
    packets: AtomicU64,
    name: Mutex<String>,
}

impl SourceState {
    fn new() -> Self {
        Self {
            rate: AtomicU32::new(0),
            id_hash: AtomicU64::new(0),
            status: AtomicU32::new(TargetStatus::Stopped as u32),
            period_us: AtomicU32::new(10_000),
            packets: AtomicU64::new(0),
            name: Mutex::new(String::new()),
        }
    }
    pub fn status(&self) -> TargetStatus {
        TargetStatus::from_u8(self.status.load(Relaxed) as u8)
    }
    fn set_status(&self, s: TargetStatus) {
        self.status.store(s as u32, Relaxed);
    }
    pub fn rate(&self) -> u32 {
        self.rate.load(Relaxed)
    }
    /// Name des geöffneten Quellgeräts (leer, wenn nicht verbunden).
    pub fn name(&self) -> String {
        self.name.lock().map(|n| n.clone()).unwrap_or_default()
    }
    fn set_open(&self, id: &str, name: &str, rate: u32) {
        if let Ok(mut n) = self.name.lock() {
            *n = name.to_owned();
        }
        self.id_hash.store(id_hash(id), Relaxed);
        self.rate.store(rate, Relaxed);
        self.set_status(TargetStatus::Active);
    }
    fn set_closed(&self, status: TargetStatus) {
        self.rate.store(0, Relaxed);
        self.id_hash.store(0, Relaxed);
        self.set_status(status);
    }
}

struct Sink {
    id: u64,
    prod: HeapProd<f32>,
    params: Arc<TargetParams>,
}

enum Cmd {
    AddSink(Sink),
    RemoveSink(u64),
    SetSource(Option<String>),
}

/// Ein konfiguriertes Ziel.
pub struct Target {
    pub id: u64,
    pub device_id: String,
    pub name: String,
    pub params: Arc<TargetParams>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub struct Engine {
    cmd_tx: Sender<Cmd>,
    source: Arc<SourceState>,
    source_id: Option<String>,
    stop: Arc<AtomicBool>,
    capture: Option<JoinHandle<()>>,
    targets: Vec<Target>,
    next_id: u64,
}

impl Engine {
    /// Startet die Engine. `source_id = None` bedeutet Windows-Standardausgabe.
    pub fn new(source_id: Option<String>) -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let source = Arc::new(SourceState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let capture = {
            let (source, stop, id) = (source.clone(), stop.clone(), source_id.clone());
            std::thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_thread(cmd_rx, source, stop, id))
                .expect("Capture-Thread konnte nicht gestartet werden")
        };
        Self {
            cmd_tx,
            source,
            source_id,
            stop,
            capture: Some(capture),
            targets: Vec::new(),
            next_id: 1,
        }
    }

    pub fn source(&self) -> &Arc<SourceState> {
        &self.source
    }
    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// ID der tatsächlich verwendeten Quelle (bei „Standard“ die aktuelle Standardausgabe).
    fn effective_source_id(&self) -> Option<String> {
        self.source_id
            .clone()
            .or_else(|| devices::default_render_id().ok())
    }

    /// Quelle wechseln. Schlägt fehl, wenn das Gerät bereits Ziel ist (Rückkopplung).
    pub fn set_source(&mut self, source_id: Option<String>) -> Result<()> {
        let eff = source_id
            .clone()
            .or_else(|| devices::default_render_id().ok());
        if let Some(eff) = eff {
            if let Some(t) = self.targets.iter().find(|t| t.device_id == eff) {
                bail!("\"{}\" ist bereits Ziel und kann nicht Quelle sein", t.name);
            }
        }
        self.source_id = source_id.clone();
        let _ = self.cmd_tx.send(Cmd::SetSource(source_id));
        Ok(())
    }

    /// Ziel hinzufügen und sofort starten. Das Quellgerät wird abgelehnt (Rückkopplung).
    pub fn add_target(
        &mut self,
        device_id: &str,
        name: &str,
        volume: f32,
        muted: bool,
        delay_ms: u32,
    ) -> Result<Arc<TargetParams>> {
        if self.effective_source_id().as_deref() == Some(device_id) {
            bail!("\"{name}\" ist die Quelle und kann nicht gleichzeitig Ziel sein");
        }
        if self.targets.iter().any(|t| t.device_id == device_id) {
            bail!("\"{name}\" ist bereits Ziel");
        }
        let params = Arc::new(TargetParams::new(volume, muted, delay_ms));
        let stop = Arc::new(AtomicBool::new(false));
        let id = self.next_id;
        self.next_id += 1;
        let ctx = RenderCtx {
            id,
            device_id: device_id.to_owned(),
            name: name.to_owned(),
            params: params.clone(),
            source: self.source.clone(),
            cmd_tx: self.cmd_tx.clone(),
            stop: stop.clone(),
        };
        let thread = std::thread::Builder::new()
            .name(format!("render-{id}"))
            .spawn(move || render_thread(ctx))?;
        self.targets.push(Target {
            id,
            device_id: device_id.to_owned(),
            name: name.to_owned(),
            params: params.clone(),
            stop,
            thread: Some(thread),
        });
        Ok(params)
    }

    pub fn remove_target(&mut self, id: u64) {
        if let Some(pos) = self.targets.iter().position(|t| t.id == id) {
            let mut t = self.targets.remove(pos);
            t.stop.store(true, Relaxed);
            if let Some(h) = t.thread.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        for t in &self.targets {
            t.stop.store(true, Relaxed);
        }
        for t in &mut self.targets {
            if let Some(h) = t.thread.take() {
                let _ = h.join();
            }
        }
        self.stop.store(true, Relaxed);
        if let Some(h) = self.capture.take() {
            let _ = h.join();
        }
    }
}

/// Schläft bis zu `d`, bricht bei `stop` früh ab. Gibt `true` zurück, wenn gestoppt werden soll.
fn sleep_unless_stopped(stop: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.load(Relaxed)
}

// ───────────────────────────── Capture ─────────────────────────────

fn capture_thread(
    rx: Receiver<Cmd>,
    source: Arc<SourceState>,
    stop: Arc<AtomicBool>,
    mut source_id: Option<String>,
) {
    devices::init_com();
    let mut sinks: Vec<Sink> = Vec::with_capacity(16);
    while !stop.load(Relaxed) {
        source.set_closed(TargetStatus::Connecting);
        match capture_session(&rx, &mut sinks, &source, &stop, &mut source_id) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("Quelle nicht verfügbar: {e:#}");
                source.set_closed(TargetStatus::Unavailable);
                // Während des Wartens weiter Kommandos annehmen.
                let end = Instant::now() + RETRY_DELAY;
                while Instant::now() < end && !stop.load(Relaxed) {
                    if drain_cmds(&rx, &mut sinks, &mut source_id) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
    source.set_closed(TargetStatus::Stopped);
}

/// Verarbeitet Kommandos; `true`, wenn die Quelle gewechselt wurde (Sitzung neu starten).
fn drain_cmds(rx: &Receiver<Cmd>, sinks: &mut Vec<Sink>, source_id: &mut Option<String>) -> bool {
    let mut restart = false;
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::AddSink(s) => sinks.push(s),
            Cmd::RemoveSink(id) => sinks.retain(|s| s.id != id),
            Cmd::SetSource(id) => {
                *source_id = id;
                restart = true;
            }
        }
    }
    restart
}

fn broadcast(sinks: &mut [Sink], data: &[f32]) {
    for s in sinks {
        if push_frames(&mut s.prod, data) > 0 {
            s.params.overruns.fetch_add(1, Relaxed);
        }
    }
}

fn open_source_device(en: &DeviceEnumerator, id: &Option<String>) -> Result<wasapi::Device> {
    Ok(match id {
        Some(id) => en.get_device(id)?,
        None => en.get_default_device_for_role(&Direction::Render, &Role::Console)?,
    })
}

fn capture_session(
    rx: &Receiver<Cmd>,
    sinks: &mut Vec<Sink>,
    source: &SourceState,
    stop: &AtomicBool,
    source_id: &mut Option<String>,
) -> Result<()> {
    let en = DeviceEnumerator::new()?;
    let dev = open_source_device(&en, source_id)?;
    if dev.get_state()? != DeviceState::Active {
        bail!("Quellgerät ist nicht aktiv");
    }
    let dev_id = dev.get_id()?;
    let name = dev.get_friendlyname()?;

    let mut client = dev.get_iaudioclient()?;
    let mix = client.get_mixformat()?;
    let rate = mix.get_samplespersec();
    let ch = mix.get_nchannels() as usize;
    let is_f32 =
        matches!(mix.get_subformat(), Ok(SampleType::Float)) && mix.get_bitspersample() == 32;
    let fmt = if is_f32 {
        mix.clone()
    } else {
        WaveFormat::new(32, 32, &SampleType::Float, rate as usize, ch, None)
    };
    // Kleinere Engine-Periode am Quellgerät erzwingen (falls der Treiber das kann): Ein stiller
    // Low-Latency-Render-Stream hält das Gerät im kurzen Takt, der Loopback liefert dann kleinere
    // Pakete in kürzeren Abständen.
    let keepalive = low_latency_stream(&dev, &mix).and_then(|(c, frames)| {
        c.start_stream().ok()?;
        Some((c, frames))
    });
    let (default_period, _) = client.get_device_period()?;
    let period = match &keepalive {
        Some((_, frames)) => {
            log::info!("Quelle: Low-Latency-Modus, Periode {frames} Frames");
            *frames as i64 * 10_000_000 / rate as i64
        }
        None => default_period,
    };
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: default_period,
    };
    // Render-Gerät + Direction::Capture = Loopback.
    client.initialize_client(&fmt, &Direction::Capture, &mode)?;
    let event = client.set_get_eventhandle()?;
    let cap = client.get_audiocaptureclient()?;

    // Alle Puffer vorab anlegen.
    let buf_frames = client.get_buffer_size()? as usize;
    let bpf = fmt.get_blockalign() as usize;
    let mut bytes = vec![0u8; buf_frames * bpf];
    let mut floats = vec![0f32; buf_frames * ch];
    let mut stereo = vec![0f32; buf_frames * 2];
    let silence = vec![0f32; (rate as usize / 5) * 2]; // max. 200 ms Stille am Stück

    log::info!("Quelle \"{name}\": {rate} Hz, {ch} Kanäle");
    source
        .period_us
        .store((period / 10).clamp(1_000, 100_000) as u32, Relaxed);
    source.set_open(&dev_id, &name, rate);
    let _mmcss = mmcss::ProAudio::enter();
    client.start_stream()?;

    // Loopback liefert bei Stille gar keine Pakete. Erst wenn länger als einige Perioden nichts
    // kam, gilt die Quelle als still und es wird Stille im Echtzeittakt nachgefüllt – niemals
    // zwischen zwei regulären Paketen (das würde hörbare Lücken in die Musik schneiden).
    let period_dur = Duration::from_nanos(period as u64 * 100);
    let idle_after = (period_dur * 4).max(Duration::from_millis(40));
    let mut last_data = Instant::now();
    let mut last_emit = Instant::now();
    let mut last_default_check = Instant::now();
    let result = (|| -> Result<()> {
        loop {
            if stop.load(Relaxed) || drain_cmds(rx, sinks, source_id) {
                return Ok(());
            }
            // Event abwarten; bei Timeout trotzdem Pakete abholen (manche Treiber signalisieren im
            // Loopback-Modus unzuverlässig). Fehler hier = Gerät getrennt.
            let _ = event.wait_for_event(period_dur.as_millis().max(1) as u32 + 5);
            let mut got_data = false;
            while let Some(n) = cap.get_next_packet_size()? {
                if n == 0 {
                    break;
                }
                let (frames, info) = cap.read_from_device(&mut bytes)?;
                let frames = frames as usize;
                if frames == 0 {
                    break;
                }
                let out = &mut stereo[..frames * 2];
                if info.flags.silent {
                    out.fill(0.0);
                } else {
                    let fl = &mut floats[..frames * ch];
                    for (f, b) in fl.iter_mut().zip(bytes.chunks_exact(4)) {
                        *f = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    }
                    to_stereo(fl, ch, out);
                }
                broadcast(sinks, out);
                got_data = true;
            }
            let now = Instant::now();
            if got_data {
                last_data = now;
                last_emit = now;
                source.packets.fetch_add(1, Relaxed);
            } else if now.duration_since(last_data) >= idle_after {
                let missing = (now.duration_since(last_emit).as_secs_f64() * rate as f64) as usize;
                let n = missing.min(silence.len() / 2);
                if n > 0 {
                    broadcast(sinks, &silence[..n * 2]);
                    last_emit += Duration::from_secs_f64(n as f64 / rate as f64);
                    if now.duration_since(last_emit) > Duration::from_millis(200) {
                        last_emit = now;
                    }
                }
            }
            // Folgt die Quelle der Windows-Standardausgabe, auf Wechsel achten.
            if source_id.is_none() && last_default_check.elapsed() > Duration::from_secs(2) {
                last_default_check = Instant::now();
                if let Ok(cur) = en
                    .get_default_device_for_role(&Direction::Render, &Role::Console)
                    .and_then(|d| d.get_id())
                {
                    if cur != dev_id {
                        log::info!("Standardausgabe gewechselt – Quelle wird neu geöffnet");
                        return Ok(());
                    }
                }
            }
        }
    })();
    let _ = client.stop_stream();
    if let Some((k, _)) = &keepalive {
        let _ = k.stop_stream();
    }
    source.set_closed(TargetStatus::Connecting);
    result
}

// ───────────────────────────── Render ─────────────────────────────

struct RenderCtx {
    id: u64,
    device_id: String,
    name: String,
    params: Arc<TargetParams>,
    source: Arc<SourceState>,
    cmd_tx: Sender<Cmd>,
    stop: Arc<AtomicBool>,
}

enum End {
    Stop,
    Restart,
}

fn render_thread(ctx: RenderCtx) {
    devices::init_com();
    let dev_hash = id_hash(&ctx.device_id);
    while !ctx.stop.load(Relaxed) {
        if ctx.source.id_hash.load(Relaxed) == dev_hash {
            ctx.params.set_status(TargetStatus::Feedback);
            if sleep_unless_stopped(&ctx.stop, Duration::from_millis(500)) {
                break;
            }
            continue;
        }
        ctx.params.set_status(TargetStatus::Connecting);
        match render_session(&ctx) {
            Ok(End::Stop) => break,
            Ok(End::Restart) => {
                if sleep_unless_stopped(&ctx.stop, Duration::from_millis(200)) {
                    break;
                }
            }
            Err(e) => {
                log::warn!("Ziel \"{}\" nicht verfügbar: {e:#}", ctx.name);
                ctx.params.set_status(TargetStatus::Unavailable);
                if sleep_unless_stopped(&ctx.stop, RETRY_DELAY) {
                    break;
                }
            }
        }
    }
    ctx.params.set_status(TargetStatus::Stopped);
}

fn render_session(ctx: &RenderCtx) -> Result<End> {
    let src_rate = ctx.source.rate();
    if src_rate == 0 {
        // Quelle (noch) nicht bereit
        return Ok(End::Restart);
    }
    let en = DeviceEnumerator::new()?;
    let dev = en.get_device(&ctx.device_id)?;
    if dev.get_state()? != DeviceState::Active {
        bail!("Gerät ist nicht aktiv");
    }
    let probe = dev.get_iaudioclient()?;
    let mix = probe.get_mixformat()?;
    let dst_rate = mix.get_samplespersec();
    // Bevorzugt: Low-Latency-Stream (IAudioClient3, kleinste Engine-Periode). Geht nur, wenn das
    // Mix-Format bereits Stereo-f32 ist (dort gibt es kein autoconvert).
    let low = if is_stereo_f32(&mix) {
        low_latency_stream(&dev, &mix)
    } else {
        None
    };
    let (client, period) = match low {
        Some((c, frames)) => {
            log::info!(
                "Ziel \"{}\": Low-Latency-Modus, Periode {frames} Frames",
                ctx.name
            );
            (c, frames as i64 * 10_000_000 / dst_rate as i64)
        }
        None => {
            let mut client = probe;
            // Stereo-Float in der Mix-Samplerate; Kanalzahl/Format passt Windows per autoconvert an.
            let fmt = WaveFormat::new(32, 32, &SampleType::Float, dst_rate as usize, 2, None);
            let (period, _) = client.get_device_period()?;
            client.initialize_client(
                &fmt,
                &Direction::Render,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: 2 * period,
                },
            )?;
            (client, period)
        }
    };
    let event = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;

    let chunk = (dst_rate * CHUNK_MS / 1000) as usize;
    let period_frames = (period as u64 * dst_rate as u64 / 10_000_000) as usize;
    let buffer_frames = client.get_buffer_size()? as usize;
    let mut resampler = StereoResampler::new(src_rate, dst_rate, chunk)?;
    let src_chunk = (src_rate * CHUNK_MS / 1000) as usize;
    // Der Loopback liefert Pakete im Abstand einer Capture-Periode; im Mittel liegt also eine halbe
    // Periode zusätzlich im Ring. Das Soll muss das abdecken, sonst läuft der Ring vor jedem Paket leer.
    let src_period_frames =
        (ctx.source.period_us.load(Relaxed) as u64 * src_rate as u64 / 1_000_000) as usize;
    let mut st = RenderState {
        in_buf: vec![0f32; resampler.input_frames_max() * 2 + 64],
        out_buf: vec![0f32; chunk * 2],
        bytes: vec![0u8; chunk * 8],
        delay: DelayLine::for_rate(dst_rate),
        gain: GainRamp::new(ctx.params.effective_gain()),
        drift: DriftController::new(src_chunk, src_rate),
        ring_reserve: JitterReserve::new(RING_RESERVE_MS.0, RING_RESERVE_MS.1, RING_RESERVE_MS.2),
        dev_reserve: JitterReserve::new(
            DEVICE_RESERVE_MS.0,
            DEVICE_RESERVE_MS.1,
            DEVICE_RESERVE_MS.2,
        ),
        target_fill: 0,
        discard_at: 0,
        base_fill: src_chunk + src_period_frames / 2,
        src_period_frames,
        max_padding: 0,
        period_frames,
        buffer_frames,
        chunk,
        period_ms: ((period / 10_000) as u32).max(1),
        dst_rate,
        src_rate,
        pending_underrun: None,
    };
    st.update_targets();

    // Ring registrieren (nach Aufbau aller Puffer), Gerät mit minimaler Stille vorfüllen.
    let (prod, mut cons) = stereo_ring(src_rate as usize * RING_MS / 1000);
    let prime = st
        .max_padding
        .min(client.get_available_space_in_frames()? as usize);
    render.write_to_device(prime, &vec![0u8; prime * 8], Some(BufferFlags::new(2)))?;
    let _ = ctx.cmd_tx.send(Cmd::AddSink(Sink {
        id: ctx.id,
        prod,
        params: ctx.params.clone(),
    }));
    let _mmcss = mmcss::ProAudio::enter();
    client.start_stream()?;
    ctx.params.set_status(TargetStatus::Active);
    log::info!(
        "Ziel \"{}\": {dst_rate} Hz (Quelle {src_rate} Hz), Periode {:.1} ms, Latenz ≈ {} ms",
        ctx.name,
        period as f64 / 10_000.0,
        st.latency_ms()
    );

    let result = render_loop(
        ctx,
        &client,
        &render,
        &event,
        &mut cons,
        &mut resampler,
        &mut st,
    );
    let _ = client.stop_stream();
    let _ = ctx.cmd_tx.send(Cmd::RemoveSink(ctx.id));
    ctx.params.set_latency_ms(0);
    result
}

struct RenderState {
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    bytes: Vec<u8>,
    delay: DelayLine,
    gain: GainRamp,
    drift: DriftController,
    ring_reserve: JitterReserve,
    dev_reserve: JitterReserve,
    /// Soll-Füllstand des Rings in Quell-Frames.
    target_fill: usize,
    discard_at: usize,
    /// Chunk + halbe Capture-Periode (Quell-Frames).
    base_fill: usize,
    src_period_frames: usize,
    /// Maximale Füllung des Gerätepuffers (Ziel-Frames).
    max_padding: usize,
    period_frames: usize,
    buffer_frames: usize,
    chunk: usize,
    period_ms: u32,
    dst_rate: u32,
    src_rate: u32,
    /// Ring-Unterlauf, der noch geprüft wird: (Zeitpunkt, Paketzähler der Quelle).
    pending_underrun: Option<(Instant, u64)>,
}

impl RenderState {
    fn update_targets(&mut self) {
        let rate = self.src_rate as usize;
        let reserve = rate * self.ring_reserve.reserve_ms() as usize / 1000;
        self.target_fill = self.base_fill + reserve;
        let excess = (rate * DISCARD_EXCESS_MS / 1000).max(2 * self.src_period_frames);
        self.discard_at = self.target_fill + excess;
        self.drift.set_target(self.target_fill);
        let dev_extra = self.dst_rate as usize * self.dev_reserve.reserve_ms() as usize / 1000;
        self.max_padding = (self.period_frames + self.chunk + dev_extra).min(self.buffer_frames);
    }

    /// Geschätzte Latenz der Kette in ms (Ring + Gerätepuffer, ohne einstellbares Delay).
    fn latency_ms(&self) -> u32 {
        let ring = self.target_fill as f64 * 1000.0 / self.src_rate as f64;
        let dev = self.max_padding as f64 * 1000.0 / self.dst_rate as f64;
        (ring + dev).round() as u32
    }
}

fn render_loop(
    ctx: &RenderCtx,
    client: &wasapi::AudioClient,
    render: &wasapi::AudioRenderClient,
    event: &wasapi::Handle,
    cons: &mut ringbuf::HeapCons<f32>,
    resampler: &mut StereoResampler,
    st: &mut RenderState,
) -> Result<End> {
    let dev_hash = id_hash(&ctx.device_id);
    let params = &ctx.params;
    let mut started = false;
    loop {
        if ctx.stop.load(Relaxed) {
            return Ok(End::Stop);
        }
        let rate_now = ctx.source.rate.load(Relaxed);
        if rate_now != st.src_rate || ctx.source.id_hash.load(Relaxed) == dev_hash {
            return Ok(End::Restart); // Quelle geändert / Rückkopplung → neu aufsetzen
        }
        st.delay
            .set_delay_frames(ms_to_frames(params.delay_ms(), st.dst_rate));
        let target_gain = params.effective_gain();
        params.set_latency_ms(st.latency_ms());

        // Ring-Unterlauf auswerten: kamen danach weiter Pakete, war es ein echter Aussetzer
        // (Reserve erhöhen). Kam nichts mehr, ist die Quelle nur verstummt (nichts tun).
        if let Some((t, pkts)) = st.pending_underrun {
            if t.elapsed() >= UNDERRUN_VERIFY {
                st.pending_underrun = None;
                if ctx.source.packets.load(Relaxed) >= pkts + 2 {
                    params.underruns.fetch_add(1, Relaxed);
                    if st.ring_reserve.underrun(Instant::now()) {
                        st.update_targets();
                        log::info!(
                            "Aussetzer – Ring-Reserve jetzt {} ms",
                            st.ring_reserve.reserve_ms()
                        );
                    }
                }
            }
        }

        let padding = client.get_current_padding()? as usize;
        if started && padding == 0 {
            // Gerätepuffer leergelaufen, obwohl Daten da waren → Thread wurde zu spät geweckt.
            params.underruns.fetch_add(1, Relaxed);
            if st.dev_reserve.underrun(Instant::now()) {
                st.update_targets();
                log::info!(
                    "Aussetzer – Geräte-Reserve jetzt {} ms",
                    st.dev_reserve.reserve_ms()
                );
            }
        }
        let mut space = st.max_padding.saturating_sub(padding);
        while space >= st.chunk {
            let mut fill = frames_available(cons);
            if !started {
                if fill < st.target_fill {
                    break; // erst bis zum Soll-Füllstand puffern
                }
                started = true;
                st.drift.reset();
            }
            if fill > st.discard_at {
                // Puffer läuft über (z. B. nach Scheduling-Pause) → älteste Samples verwerfen.
                let d = discard_frames(cons, fill - st.target_fill);
                params.dropped_frames.fetch_add(d as u64, Relaxed);
                fill -= d;
                st.drift.reset();
            }
            resampler.set_relative_ratio(st.drift.update(fill));
            let need = resampler.input_frames_next();
            if !pop_frames_exact(cons, &mut st.in_buf[..need * 2]) {
                if st.pending_underrun.is_none() {
                    st.pending_underrun = Some((Instant::now(), ctx.source.packets.load(Relaxed)));
                }
                started = false; // neu puffern
                break;
            }
            resampler.process(&st.in_buf[..need * 2], &mut st.out_buf)?;
            st.delay.process_in_place(&mut st.out_buf);
            st.gain.apply(&mut st.out_buf, target_gain);
            let (l, r) = peak_stereo(&st.out_buf);
            params.report_peak(l, r);
            for (b, s) in st.bytes.chunks_exact_mut(4).zip(st.out_buf.iter()) {
                b.copy_from_slice(&s.to_le_bytes());
            }
            render.write_to_device(st.chunk, &st.bytes, None)?;
            space -= st.chunk;
        }

        // Das Event kommt einmal pro Periode. Solange gepuffert wird, öfter nachsehen.
        let timeout = if started { st.period_ms * 2 } else { 1 };
        if event.wait_for_event(timeout).is_err() {
            // Timeout: prüft zugleich, ob das Gerät noch lebt (Fehler bei Trennung).
            client.get_current_padding()?;
        }
    }
}

fn is_stereo_f32(fmt: &WaveFormat) -> bool {
    fmt.get_nchannels() == 2
        && fmt.get_bitspersample() == 32
        && matches!(fmt.get_subformat(), Ok(SampleType::Float))
}

/// Öffnet einen Shared-Mode-Render-Stream mit der kleinsten vom Treiber unterstützten
/// Engine-Periode (IAudioClient3). `None`, wenn nicht unterstützt oder nicht kleiner als Standard.
fn low_latency_stream(
    dev: &wasapi::Device,
    mix: &WaveFormat,
) -> Option<(wasapi::AudioClient, u32)> {
    let mut c = dev.get_iaudioclient().ok()?;
    let p = c.get_shared_mode_engine_period(mix).ok()?;
    if p.min == 0 || p.min >= p.default {
        return None;
    }
    match c.initialize_shared_audio_stream(mix, wasapi::TimingMode::Events, p.min) {
        Ok(()) => Some((c, p.min)),
        Err(e) => {
            log::debug!("Low-Latency-Modus nicht möglich: {e}");
            None
        }
    }
}

/// MMCSS-Priorität „Pro Audio“ für den aktuellen Thread (pünktliches Aufwachen bei kleinen Puffern).
mod mmcss {
    use windows::core::w;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    };

    pub struct ProAudio(Option<HANDLE>);

    impl ProAudio {
        pub fn enter() -> Self {
            let mut index = 0u32;
            match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) } {
                Ok(h) => Self(Some(h)),
                Err(e) => {
                    log::debug!("MMCSS nicht verfügbar: {e}");
                    Self(None)
                }
            }
        }
    }

    impl Drop for ProAudio {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
            }
        }
    }
}
