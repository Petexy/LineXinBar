fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("privileged-job") => lxb_updates::authorization::serve(),
        Some("serve") => lxb_updates::service::serve(),
        // One reviewed step of Update LineXinBar, in the job's terminal: see
        // lxb_updates::release_install. The operation is the one argument.
        Some("release") => match std::env::args().nth(2) {
            Some(operation) if std::env::args().count() == 3 => {
                lxb_updates::release_install::run(&operation)
            }
            _ => anyhow::bail!("Usage: lxb-updates release <operation>"),
        },
        Some("discover") => {
            serde_json::to_writer_pretty(std::io::stdout(), &lxb_updates::discovery::discover())?;
            Ok(())
        }
        Some("--version") => {
            println!("lxb-updates {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            println!("lxb-updates: the LineXinBar update coordinator\nUsage: lxb-updates serve | discover | --version\nInstallation is initiated and reviewed in Settings > Updates.");
            Ok(())
        }
    }
}
