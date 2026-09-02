use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

struct FlushWriter<W: Write>(W);

impl<W: Write> Write for FlushWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.0.write(buf)?;
        self.0.flush()?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

use herdr_kontrol::herdr::default_socket;
use herdr_kontrol::nihia::NihiaGuard;
use herdr_kontrol::{app, nihia};

#[derive(Parser, Debug)]
#[command(
    name = "herdr-kontrol",
    about = "Maschine MK3 control surface for herdr"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Skip killing NIHostIntegrationAgent (screens will fail if it holds IF#5).
    #[arg(long, global = true)]
    no_inhibit: bool,
    /// Leave NI agents down on exit.
    #[arg(long, global = true)]
    no_restore: bool,
    #[arg(long, env = "HERDR_SOCKET_PATH", global = true)]
    socket: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the control surface (default).
    Run,
    /// Inhibit NIHIA, rainbow pads, blit test frames, dump HID.
    Probe,
    /// Kill NIHostIntegrationAgent / NIHardwareAgent.
    Inhibit,
    /// Relaunch the NI agents.
    Restore,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(|| FlushWriter(io::stderr()))
        .init();

    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Inhibit => nihia::kill_agents(),
        Cmd::Restore => nihia::restore_agents(),
        Cmd::Probe => {
            let mut guard = maybe_inhibit(cli.no_inhibit)?;
            let r = app::probe().await;
            if cli.no_restore {
                guard.forget();
            }
            r
        }
        Cmd::Run => {
            let mut guard = maybe_inhibit(cli.no_inhibit)?;
            let r = app::run(app::Options {
                socket: cli.socket.unwrap_or_else(default_socket),
            })
            .await;
            if cli.no_restore {
                guard.forget();
            }
            r
        }
    }
}

fn maybe_inhibit(no_inhibit: bool) -> Result<NihiaGuard> {
    if no_inhibit {
        Ok(NihiaGuard::noop())
    } else {
        NihiaGuard::inhibit()
    }
}
