/// Legacy `claw` binary — delegates to `ember` with a deprecation notice.
fn main() {
    eprintln!(
        "\x1b[33m⚠ `claw` is deprecated. Use `ember` instead.\x1b[0m"
    );
    // Re-run using the same logic as the ember binary.
    // Since both are compiled from the same crate, we just call main directly.
    std::process::exit(
        std::process::Command::new("ember")
            .args(std::env::args_os().skip(1))
            .status()
            .map(|s| s.code().unwrap_or(1))
            .unwrap_or(1),
    );
}
