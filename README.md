# shared-audio

Schlankes Windows-Programm (Rust), das den Ton einer **Quelle** per WASAPI-Loopback abgreift und
in Echtzeit auf **beliebig viele Ausgabegeräte** gleichzeitig ausgibt – z. B. auf zwei
Bluetooth-Kopfhörer. Ähnlich wie VoiceMeeter, aber nur das Nötigste.

Pro Ziel einstellbar: **Lautstärke**, **Mute**, **Verzögerung (0–1000 ms)** zum Ausgleich unterschiedlicher
Bluetooth-Latenz, dazu Pegelanzeige und Verbindungsstatus.

> **Stand / Testhinweis:** Der Code wurde ohne Zugriff auf einen Windows-Rechner mit Audiogeräten entwickelt.
> Der plattformunabhängige Kern (Delay, Kanal-Mix, Resampling, Drift-Regler, Ringpuffer, Config) ist per
> Unit-Test abgedeckt; der WASAPI-Teil, die Engine und die GUI sind gegen das Windows-Target kompiliert und
> mit Clippy geprüft (`.github/workflows/ci.yml` baut zusätzlich auf einem Windows-Runner), aber **noch nicht
> mit echter Hardware erprobt**. Bitte Fehler mit `RUST_LOG=debug` melden.

## Warum die Crate `wasapi` (und nicht `cpal`)?

- **Loopback ist explizit und dokumentiert:** Ein Render-Gerät wird als `Direction::Capture` initialisiert
  (Flag `AUDCLNT_STREAMFLAGS_LOOPBACK`). `cpal` bietet Loopback nur als Nebeneffekt beim Öffnen eines
  Ausgabegeräts als Eingabe und gibt wenig Kontrolle über Pufferdauer, Event-Modus und Fehlercodes.
- **Event-getriebener Shared Mode mit `autoconvert`:** Windows übernimmt Format-/Kanalanpassung der
  Ausgabe, unser Code braucht nur Stereo-`f32`.
- **Stabile Geräte-IDs und Gerätestatus** (`Active`/`NotPresent`/`Unplugged`) – nötig für das automatische
  Wiederverbinden von Bluetooth-Geräten. Ein Fehler in `GetNextPacketSize`/`GetCurrentPadding` zeigt eine
  getrennte Verbindung zuverlässig an.
- `cpal` abstrahiert plattformübergreifend; hier ist Windows das einzige Ziel, daher bringt die dünne,
  direkte WASAPI-Bindung mehr Kontrolle bei weniger Umwegen.

## Installation

Voraussetzung: Windows 10/11, [Rust (stable)](https://rustup.rs/).

```bash
git clone https://github.com/theslasher5g/shared-audio
cd shared-audio
cargo build --release
```

Das Programm liegt danach unter `target\release\shared-audio.exe` (eine einzelne Datei, kein Installer).
Alternativ lädt man die `.exe` als Artefakt des CI-Laufs unter *Actions* herunter.

## Bedienung

### GUI (Standard)

`shared-audio.exe` startet die Oberfläche:

1. **Quelle** wählen – „Windows-Standardausgabe“ oder ein bestimmtes Gerät. Das Programm folgt einem Wechsel
   der Standardausgabe automatisch.
2. Unter **Ziel hinzufügen** ein Gerät wählen und auf *Hinzufügen* klicken. Das Quellgerät und bereits
   gewählte Ziele werden nicht angeboten.
3. Pro Ziel: **Lautstärke**, **Mute**, **Verzögerung**, Pegel (L/R) und Status.
4. **Delay einstellen:** Klingt ein Kopfhörer „früher“ als der andere, bei diesem so lange die Verzögerung
   erhöhen (typisch 0–300 ms Unterschied), bis beide synchron sind. Hintergrund: Der Bluetooth-Codec
   (SBC/AAC/aptX …) bringt je Gerät eine andere Latenz mit.
5. Alle Einstellungen werden automatisch in `%APPDATA%\shared-audio\config.json` gespeichert und beim
   Start geladen.

Status eines Ziels: *Aktiv*, *Verbinde …*, *Nicht verfügbar (neuer Versuch läuft)* (Gerät getrennt/aus – es wird
jede Sekunde neu versucht) und *Gesperrt: ist die Quelle*.

### Kommandozeile

```bash
shared-audio.exe list
shared-audio.exe run --target 2 --target 3:120:80
shared-audio.exe run --source "CABLE Input" --target "Kopfhörer A" --target "Kopfhörer B:150"
```

Ziel-Syntax: `GERÄT[:DELAY_MS[:LAUTSTÄRKE_%]]`; `GERÄT` ist die Nummer aus `list`, die Geräte-ID oder ein
eindeutiger Namensteil. `Strg+C` beendet.

## Windows richtig einstellen (Quelle & Standardausgabe)

Das Programm hört mit, was **an die Quelle** abgespielt wird. Damit der Ton auf beiden Kopfhörern ankommt –
und nicht doppelt oder gar nicht:

1. **Die Kopfhörer dürfen nicht die Windows-Standardausgabe sein.** Die Quelle darf nicht zugleich Ziel sein
   (das Programm sperrt das, sonst gäbe es Rückkopplung).
2. **Empfohlen: ein virtuelles Audiokabel** (z. B. *VB-Cable*) als Quelle:
   - *Einstellungen → System → Ton → Ausgabe:* „CABLE Input“ als Standardgerät wählen.
   - In shared-audio als Quelle „Windows-Standardausgabe“ (oder „CABLE Input“) wählen.
   - Beide Kopfhörer als Ziele hinzufügen. Der Ton ist dann ausschließlich auf den Kopfhörern zu hören.
3. **Ohne virtuelles Kabel:** Ein vorhandenes anderes Ausgabegerät als Standard setzen (Lautsprecher, Monitor
   per HDMI/DisplayPort …) und als Quelle verwenden. Man hört dann zusätzlich dieses Gerät; es lässt sich
   über die Windows-Lautstärke dämpfen. Bei Loopback wirkt die Endpunkt-Lautstärke je nach Treiber nicht
   auf das abgegriffene Signal – bitte kurz ausprobieren.
4. **Bluetooth-Kopfhörer im Stereo-Modus (A2DP)** verwenden („Kopfhörer“, nicht „Freisprechen/Headset“):
   Im Hands-Free-Profil sind es 16 kHz Mono. Das Programm wandelt zwar automatisch um, klingt aber schlecht.
5. Verbundene Kopfhörer müssen in Windows als *aktives* Ausgabegerät erscheinen (Bluetooth verbunden, nicht
   deaktiviert). Getrennte Geräte werden in der GUI als „nicht verfügbar“ geführt und automatisch wieder
   aufgenommen, sobald sie zurück sind.
6. App-Lautstärke: Die Lautstärke einer Anwendung im Windows-Mixer wirkt noch vor dem Loopback, die
   *Ziel-Lautstärke* in shared-audio gilt je Kopfhörer.

## Technik in Kürze

Siehe [`CLAUDE.md`](CLAUDE.md) für Details.

- Ein Capture-Thread liest das Loopback, mischt auf Stereo (Mono/5.1/7.1 …) und schreibt in einen lock-freien
  SPSC-Ringpuffer (`ringbuf`) pro Ziel.
- Je Ziel ein Render-Thread: Drift-Regler → Resampling (`rubato`, Sinc) → Delay-Line → Gain/Mute → WASAPI.
  Im Audiopfad gibt es keine Locks und keine Allokationen; Parameter und Pegel laufen über Atomics.
- **Drift-Ausgleich:** Ein P-Regler stellt das Resampling-Verhältnis (max. ±0,5 %) anhand des Ringfüllstands
  nach, sodass Geräte mit leicht abweichender Taktrate den Puffer nicht leeren oder überfüllen. Läuft der Puffer
  trotzdem über, werden die ältesten Samples verworfen; bei Unterlauf wird neu gepuffert.
- **Stille:** Loopback liefert bei Stille keine Daten; der Capture-Thread füllt dann Nullen nach.
- **Hotplug/Reconnect:** Fehler beim Zugriff auf ein Gerät werden abgefangen, das Ziel wird „nicht verfügbar“
  markiert und jede Sekunde neu geöffnet. Fällt die Quelle aus, geschieht dasselbe.

## Bekannte Grenzen

- **Nicht auf Hardware getestet** (siehe oben); die Reaktion auf echte Bluetooth-Trennungen ist
  nur über Fehlercodes des Treibers abgesichert.
- **Gesamtlatenz:** Die Verteilung selbst fügt ca. 60–100 ms hinzu (Puffer 40 ms + Geräte-Puffer 50 ms +
  Resampler). Für Musik/Video mit Delay-Abgleich unkritisch, für Echtzeit-Spiele oder Instrumente nicht geeignet.
  Das Delay gleicht nur die *Unterschiede* zwischen den Zielen aus – ein Ziel kann nicht „früher“ als ein
  anderes gemacht werden, außer man verzögert das andere.
- **Delay-Änderungen** springen hörbar (kurzes Knacken), da die Verzögerung hart umgeschaltet wird.
- **Kopiergeschützte Inhalte** (DRM) liefern im Loopback ggf. Stille.
- Der Shared Mode wird verwendet; Anwendungen im exklusiven Modus erscheinen nicht im Loopback.
- Das Render-Format ist Stereo; Surround-Quellen werden auf Stereo abgemischt.
- Der System-Tray-Modus („Minimieren in den Tray“, optional in Phase 3) ist **nicht** umgesetzt.
- Das Konsolenfenster erscheint auch beim GUI-Start, wenn die `.exe` per Doppelklick gestartet wird.

## Entwicklung

```bash
cargo test                                   # Kern-Tests, auch unter Linux/macOS
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings   # Windows-Code von Linux aus prüfen
```

## Lizenz

MIT
