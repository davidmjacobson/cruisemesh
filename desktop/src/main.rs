#[cfg(windows)]
mod windows_main {
    use anyhow::Result;
    use clap::{Parser, Subcommand};
    use qrcode::{render::unicode, QrCode};
    use std::sync::Arc;
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    use cruisemesh_node::{
        bootstrap::BootstrapStore,
        ipc,
        platform::lifecycle::{
            hide_console, install_firewall_rule, install_logon_task, open_default_browser,
        },
        runtime,
        store_paths::AppPaths,
    };

    #[derive(Debug, Parser)]
    #[command(
        name = "cruisemesh-node",
        version,
        about = "CruiseMesh Helper for Windows"
    )]
    struct Cli {
        #[command(subcommand)]
        command: Option<Command>,
    }

    #[derive(Debug, Subcommand)]
    enum Command {
        /// Run the helper in the foreground.
        Run {
            #[arg(long)]
            foreground: bool,
        },
        /// Print this helper's deposit-safe friend link.
        ShowCard,
        /// Import a Shore Pass (CMRELAY1 text or link).
        ImportRelay { text: String },
        /// Import a phone's friend card for offline mutual setup.
        ImportFriend { text: String },
        /// Print a redacted node status snapshot.
        Status,
        /// Install the current executable as a per-user logon task.
        InstallAutostart,
        /// Add an inbound rule after confirming use on Public networks.
        AllowFirewall,
    }

    pub async fn run() -> Result<()> {
        let paths = AppPaths::discover()?;
        let file = tracing_appender::rolling::daily(&paths.logs, "helper.log");
        let (file, _log_guard) = tracing_appender::non_blocking(file);
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "cruisemesh_node=info".into()),
            )
            .with_writer(std::io::stdout.and(file))
            .with_target(false)
            .init();

        let bootstrap = BootstrapStore::open(paths.clone())?;
        match Cli::parse()
            .command
            .unwrap_or(Command::Run { foreground: true })
        {
            Command::Run { foreground } => {
                if foreground {
                    println!("CruiseMesh Helper identity is ready. Network runtime is starting.");
                } else {
                    hide_console();
                }
                runtime::run(paths, Arc::new(bootstrap)).await?;
            }
            Command::ShowCard => {
                if let Some(value) = ipc::try_request(serde_json::json!({
                    "command": "GetFriendCard"
                }))
                .await?
                {
                    show_card(
                        value
                            .get("text")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default(),
                    )?;
                } else {
                    show_card(&bootstrap.friend_link()?)?;
                }
            }
            Command::ImportRelay { text } => {
                if ipc::try_request(serde_json::json!({
                    "command": "ImportRelaySetup",
                    "text": &text,
                }))
                .await?
                .is_none()
                {
                    bootstrap.import_relay_setup(&text)?;
                }
                println!("Shore Pass imported.");
            }
            Command::ImportFriend { text } => {
                if let Some(value) = ipc::try_request(serde_json::json!({
                    "command": "ImportFriendCard",
                    "text": &text,
                }))
                .await?
                {
                    println!(
                        "Imported {}.",
                        value
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or("contact")
                    );
                } else {
                    let (contact, _) = bootstrap.import_friend(&text)?;
                    println!("Imported {}.", contact.name);
                }
            }
            Command::Status => {
                let status = match ipc::try_request(serde_json::json!({
                    "command": "GetStatus"
                }))
                .await?
                {
                    Some(value) => value,
                    None => serde_json::to_value(bootstrap.status()?)?,
                };
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
            Command::InstallAutostart => {
                install_logon_task(&std::env::current_exe()?)?;
                println!("CruiseMesh Helper will start when you sign in.");
            }
            Command::AllowFirewall => {
                install_firewall_rule(&std::env::current_exe()?)?;
                println!(
                    "CruiseMesh Helper firewall rule installed for Private and Public networks."
                );
            }
        }
        #[allow(unreachable_code)]
        Ok(())
    }

    fn show_card(friend_link: &str) -> Result<()> {
        let web_link = friend_web_link(friend_link);
        match open_default_browser(&web_link) {
            Ok(()) => {
                println!("Opened this helper's friend card in the default browser.");
                println!("{web_link}");
            }
            Err(error) => {
                eprintln!("Could not open the default browser: {error}");
                eprintln!("Showing the QR code in this terminal instead.");
                print_terminal_card(&web_link)?;
            }
        }
        Ok(())
    }

    fn friend_web_link(friend_link: &str) -> String {
        format!("https://cruisemesh.app/f#{friend_link}")
    }

    fn print_terminal_card(web_link: &str) -> Result<()> {
        let code = QrCode::new(web_link.as_bytes())?;
        println!(
            "{}",
            code.render::<unicode::Dense1x2>().quiet_zone(true).build()
        );
        println!("{web_link}");
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn friend_card_browser_url_matches_the_mobile_qr_contract() {
            assert_eq!(
                friend_web_link("CMFRIEND3:abc"),
                "https://cruisemesh.app/f#CMFRIEND3:abc"
            );
        }
    }
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    windows_main::run().await
}

#[cfg(not(windows))]
fn main() {
    eprintln!("CruiseMesh Helper is currently available only on Windows.");
}
