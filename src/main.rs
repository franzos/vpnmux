mod cli;
mod coexistence;
mod daemon;
mod dns;
mod fsutil;
mod log;
mod mullvad;
mod nft;
mod paths;
mod provider;
mod reconcile;
mod runner;
mod state;
mod sys;
mod tailscale;
mod types;

fn main() {
    log::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("daemon") => daemon::run(),
        Some("status") => cli::status(),
        Some("set") => cli::set(&args[1..]),
        _ => {
            eprintln!("usage: vpnmux {{set [provider...] [--yes] | status | daemon}}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("vpnmux: {e}");
        std::process::exit(1);
    }
}
