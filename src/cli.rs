//! Kommandozeile: Geräte auflisten und Verteilung ohne GUI starten.

use crate::devices::{self, DeviceInfo};
use crate::engine::Engine;
use anyhow::{anyhow, bail, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

pub const USAGE: &str = "\
shared-audio – Systemaudio auf mehrere Ausgabegeräte verteilen

Verwendung:
  shared-audio                 GUI starten
  shared-audio gui             GUI starten
  shared-audio list            Ausgabegeräte auflisten
  shared-audio run [--source QUELLE] --target ZIEL[:DELAY_MS[:LAUTSTÄRKE_%]] [--target ...]

QUELLE/ZIEL: Listennummer (aus `list`), Geräte-ID oder eindeutiger Namensteil.
Ohne --source (oder mit `default`) wird die Windows-Standardausgabe abgegriffen.

Beispiel:
  shared-audio run --target 2:0 --target \"Kopfhörer B:180:80\"
";

struct TargetSpec {
    spec: String,
    delay_ms: u32,
    volume_pct: u32,
}

fn parse_target(s: &str) -> Result<TargetSpec> {
    // Namen enthalten selten ':'; von rechts bis zu zwei numerische Felder abspalten.
    let mut parts: Vec<&str> = s.split(':').collect();
    let mut nums = Vec::new();
    while parts.len() > 1 && nums.len() < 2 {
        match parts.last().and_then(|p| p.parse::<u32>().ok()) {
            Some(n) => {
                nums.push(n);
                parts.pop();
            }
            None => break,
        }
    }
    nums.reverse();
    let (delay_ms, volume_pct) = match nums.as_slice() {
        [] => (0, 100),
        [d] => (*d, 100),
        [d, v] => (*d, *v),
        _ => unreachable!(),
    };
    Ok(TargetSpec {
        spec: parts.join(":"),
        delay_ms: delay_ms.min(crate::delay::MAX_DELAY_MS),
        volume_pct: volume_pct.min(100),
    })
}

fn print_devices(devs: &[DeviceInfo]) {
    for (i, d) in devs.iter().enumerate() {
        println!(
            "{:>2}. {}{}\n    {}",
            i + 1,
            d.name,
            if d.is_default { "  [Standard]" } else { "" },
            d.id
        );
    }
}

pub fn run_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") => {
            print_devices(&devices::list_render_devices()?);
            Ok(())
        }
        Some("run") => run_cmd(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("Unbekannter Befehl \"{other}\"\n\n{USAGE}"),
        None => unreachable!("ohne Argumente startet die GUI"),
    }
}

fn run_cmd(args: &[String]) -> Result<()> {
    let mut source: Option<String> = None;
    let mut targets = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--source" | "-s" => {
                source = Some(it.next().context("--source braucht einen Wert")?.clone())
            }
            "--target" | "-t" => targets.push(parse_target(
                it.next().context("--target braucht einen Wert")?,
            )?),
            other => bail!("Unbekannte Option \"{other}\"\n\n{USAGE}"),
        }
    }
    if targets.is_empty() {
        bail!("Mindestens ein --target angeben\n\n{USAGE}");
    }

    let devs = devices::list_render_devices()?;
    let source_dev = match source.as_deref() {
        None | Some("default") => None,
        Some(s) => Some(devices::resolve_device(&devs, s)?.clone()),
    };
    let mut engine = Engine::new(source_dev.as_ref().map(|d| d.id.clone()));
    println!(
        "Quelle: {}",
        source_dev
            .as_ref()
            .map(|d| d.name.as_str())
            .unwrap_or("Windows-Standardausgabe")
    );
    let mut handles = Vec::new();
    for t in &targets {
        let dev = devices::resolve_device(&devs, &t.spec)?;
        let params = engine
            .add_target(
                &dev.id,
                &dev.name,
                t.volume_pct as f32 / 100.0,
                false,
                t.delay_ms,
            )
            .map_err(|e| anyhow!("{e}"))?;
        println!(
            "Ziel:   {} (Delay {} ms, Lautstärke {} %)",
            dev.name, t.delay_ms, t.volume_pct
        );
        handles.push((dev.name.clone(), params));
    }

    let quit = Arc::new(AtomicBool::new(false));
    {
        let q = quit.clone();
        ctrlc::set_handler(move || q.store(true, Relaxed))?;
    }
    println!("Läuft – Strg+C beendet.\n");
    while !quit.load(Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let src = engine.source();
        let mut line = format!(
            "Quelle: {:<12}",
            src.status().label().chars().take(12).collect::<String>()
        );
        for (name, p) in &handles {
            let (l, r) = p.take_peak();
            line.push_str(&format!(
                " | {}: {} {:.0}% ",
                name.chars().take(14).collect::<String>(),
                match p.status() {
                    crate::params::TargetStatus::Active => "OK",
                    crate::params::TargetStatus::Unavailable => "weg",
                    crate::params::TargetStatus::Feedback => "FB!",
                    _ => "…",
                },
                l.max(r) * 100.0
            ));
        }
        print!("\r{line:<110}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    println!("\nBeende …");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_spec_parsing() {
        let t = parse_target("Kopfhörer B:180:80").unwrap();
        assert_eq!(
            (t.spec.as_str(), t.delay_ms, t.volume_pct),
            ("Kopfhörer B", 180, 80)
        );
        let t = parse_target("2:0").unwrap();
        assert_eq!((t.spec.as_str(), t.delay_ms, t.volume_pct), ("2", 0, 100));
        let t = parse_target("Headset").unwrap();
        assert_eq!(
            (t.spec.as_str(), t.delay_ms, t.volume_pct),
            ("Headset", 0, 100)
        );
        let t = parse_target("x:5000:300").unwrap();
        assert_eq!((t.delay_ms, t.volume_pct), (1000, 100));
    }
}
