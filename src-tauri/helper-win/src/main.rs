//! Windows exclude-mode split tunneling agent.
//!
//! Three entry points:
//!
//! ```text
//! nimbus-split-win --service             run as the LocalSystem service (SCM)
//! nimbus-split-win --serve               same worker in the foreground, for testing
//! nimbus-split-win --session <file.json> drive the driver once and print events
//! ```

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--service" => {
            if let Err(error) = nimbus_split_win::service::run_service() {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        [flag] if flag == "--serve" => {
            if let Err(error) = nimbus_split_win::service::serve_foreground() {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        [flag] if flag == "--install" => {
            if let Err(error) = nimbus_split_win::service::install() {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        [flag] if flag == "--uninstall" => {
            if let Err(error) = nimbus_split_win::service::uninstall() {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        [flag, path] if flag == "--session" => {
            if let Err(error) = nimbus_split_win::driver::run_console_session(path) {
                eprintln!("nimbus-split-win: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!(
                "nimbus-split-win {version}\n\n  \
                 --install               install and start the split tunnel service\n  \
                 --uninstall             stop and remove the split tunnel service\n  \
                 --service               run as the split tunnel service\n  \
                 --serve                 serve on {pipe} in the foreground\n  \
                 --session <file.json>   engage the driver once and print events\n\n\
                 Requires administrator rights and the Nimbus split tunnel driver.",
                version = env!("CARGO_PKG_VERSION"),
                pipe = nimbus_split_win::service::PIPE_NAME
            );
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "nimbus-split-win is Windows-only; it drives the signed split tunnel driver \
         that Nimbus ships. The protocol and decision layers are unit-tested here."
    );
    std::process::exit(2);
}
