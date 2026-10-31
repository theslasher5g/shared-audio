//! Geräte-Enumeration (WASAPI, nur Ausgabegeräte).

use anyhow::{anyhow, Result};
use std::hash::{Hash, Hasher};
use wasapi::{Direction, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// COM für den aktuellen Thread initialisieren (mehrfach aufrufbar).
pub fn init_com() {
    let _ = wasapi::initialize_mta();
}

/// Stabiler Hash einer Geräte-ID (für lock-freie Vergleiche im Audio-Thread).
pub fn id_hash(id: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish().max(1) // 0 ist reserviert für "unbekannt"
}

/// Alle aktiven Ausgabegeräte.
pub fn list_render_devices() -> Result<Vec<DeviceInfo>> {
    init_com();
    let en = wasapi::DeviceEnumerator::new()?;
    let default_id = en
        .get_default_device_for_role(&Direction::Render, &Role::Console)
        .and_then(|d| d.get_id())
        .ok();
    let coll = en.get_device_collection(&Direction::Render)?;
    let n = coll.get_nbr_devices()?;
    let mut out = Vec::new();
    for i in 0..n {
        let dev = match coll.get_device_at_index(i) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let (Ok(id), Ok(name)) = (dev.get_id(), dev.get_friendlyname()) else {
            continue;
        };
        out.push(DeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
        });
    }
    Ok(out)
}

/// ID der Windows-Standardausgabe.
pub fn default_render_id() -> Result<String> {
    init_com();
    let en = wasapi::DeviceEnumerator::new()?;
    let dev = en.get_default_device_for_role(&Direction::Render, &Role::Console)?;
    Ok(dev.get_id()?)
}

/// Gerät per Listenindex (`1`…), exakter ID oder eindeutigem Namensteil finden.
pub fn resolve_device<'a>(devs: &'a [DeviceInfo], spec: &str) -> Result<&'a DeviceInfo> {
    if let Ok(idx) = spec.trim_start_matches('#').parse::<usize>() {
        return devs
            .get(idx.wrapping_sub(1))
            .ok_or_else(|| anyhow!("Kein Gerät mit Nummer {idx}"));
    }
    if let Some(d) = devs.iter().find(|d| d.id == spec) {
        return Ok(d);
    }
    let needle = spec.to_lowercase();
    let hits: Vec<_> = devs
        .iter()
        .filter(|d| d.name.to_lowercase().contains(&needle))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(anyhow!("Kein Gerät passt auf \"{spec}\"")),
        many => Err(anyhow!(
            "\"{spec}\" ist nicht eindeutig: {}",
            many.iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}
