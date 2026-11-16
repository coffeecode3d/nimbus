//! Windows exclude-mode split tunneling agent.
//!
//! Console harness for now; the service wrapper and the NSIS packaging come
//! after the integration has been exercised on a real machine.

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, path] if flag == "--session" => {
            if let Err(error) = nimbus_split_win::driver::run_console_session(path) {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!(
                "nimbus-split-win {version}\n\n  \
                 --session <file.json>   engage the split tunnel driver and print events\n\n\
                 Requires administrator rights and the Mullvad split tunnel driver.",
                version = env!("CARGO_PKG_VERSION")
            );
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "nimbus-split-win is Windows-only; it drives the Mullvad split tunnel driver \
         exported by Eddie's Windows tunnel. The protocol it speaks is unit-tested here."
    );
    std::process::exit(2);
}
