fn main() {
    if let Err(e) = platform_wayland::panel::run() {
        eprintln!("[kime-panel] {e}");
        std::process::exit(1);
    }
}
