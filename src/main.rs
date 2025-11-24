#[cfg(feature = "cli")]
mod cli;

#[cfg(feature = "cli")]
fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "cli"))]
fn main() {
    eprintln!("The CLI is disabled. Rebuild with `--features cli` to enable it.");
}
