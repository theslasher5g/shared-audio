#[cfg(not(windows))]
fn main() {
    eprintln!("shared-audio läuft nur unter Windows 10/11.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("gui") => shared_audio::gui::run_gui(),
        _ => shared_audio::cli::run_cli(&args),
    };
    if let Err(e) = result {
        eprintln!("Fehler: {e:#}");
        std::process::exit(1);
    }
}
