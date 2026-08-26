use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "web")]
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use treex::{Command, ScanOptions, Session};

const DEFAULT_WEB_ADDR: &str = "127.0.0.1:11711";

#[derive(Parser, Debug)]
#[command(
    name = "treex",
    version,
    about = "An interactive directory tree for the terminal and the browser"
)]
struct Cli {
    /// Directory to browse.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// List directories only.
    #[arg(short = 'd', long)]
    dirs_only: bool,

    /// Follow symbolic links.
    #[arg(long)]
    follow_links: bool,

    /// Levels expanded on startup.
    #[arg(short = 'L', long = "level", default_value_t = 1, value_name = "N")]
    level: usize,

    /// Print the tree and exit, instead of opening the UI.
    #[arg(short = 'p', long)]
    print: bool,

    /// Serve the same tree over HTTP. Accepts a port, :port, or host:port.
    /// If the port is taken, the next free one is used.
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = DEFAULT_WEB_ADDR)]
    web: Option<String>,

    /// Largest file the web view will show the contents of. Accepts 1m, 512k, 4096.
    #[arg(
        long,
        default_value = "1m",
        value_name = "SIZE",
        value_parser = treex::preview::parse_size,
        conflicts_with = "no_preview",
    )]
    max_preview_size: u64,

    /// Do not serve file contents at all.
    #[arg(long)]
    no_preview: bool,

    /// Do not open the terminal UI. With --web, this is a headless server.
    #[arg(long)]
    no_tui: bool,

    /// Do not react to filesystem changes.
    #[arg(long)]
    no_watch: bool,

    /// How long to coalesce filesystem events, in milliseconds.
    #[arg(
        long,
        default_value_t = 250,
        value_name = "MS",
        value_parser = clap::value_parser!(u64).range(1..=600_000),
        conflicts_with = "no_watch",
    )]
    debounce: u64,

    /// Disable mouse capture, restoring native terminal text selection.
    #[arg(long)]
    no_mouse: bool,

    /// Require a click on the ▸ marker to expand, rather than anywhere on the row.
    #[arg(long)]
    no_click_toggle: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Dotfile visibility is not a startup flag: both views toggle it live.
    let opts = ScanOptions {
        dirs_only: cli.dirs_only,
        follow_links: cli.follow_links,
        ..ScanOptions::default()
    };

    // No added context: treex::Error already names the path and the reason.
    let session = Session::new(&cli.path, opts)?;
    session.apply(Command::ExpandDepth { depth: cli.level });

    // A pipe or a redirect cannot host a TUI. Falling back to the printed tree
    // is what makes `treex | less` and `treex > structure.txt` do the obvious
    // thing instead of failing on raw-mode setup.
    let interactive = std::io::stdout().is_terminal();
    if cli.print || (!interactive && cli.web.is_none()) {
        print_tree(&session);
        return Ok(());
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli, session))
}

fn print_tree(session: &Session) {
    use std::io::Write;

    let snapshot = session.snapshot();
    // Buffered and locked: a closed pipe (`treex -p | head`) is a normal way
    // for this to end, not a panic out of `println!`.
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    if writeln!(out, "{}", snapshot.root.display()).is_err() {
        return;
    }

    // Whether each ancestor was the last of its siblings, which decides between
    // a continuing "\u{2502}" and blank padding at that depth.
    let mut lasts: Vec<bool> = Vec::new();
    for row in snapshot.rows.iter().skip(1) {
        lasts.truncate(row.depth - 1);
        let mut prefix = String::new();
        for &last in &lasts {
            prefix.push_str(if last { "    " } else { "\u{2502}   " });
        }
        prefix.push_str(if row.last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        });
        lasts.push(row.last);

        let mut name = format!("{}{}", row.name, row.kind.suffix());
        if row.omitted > 0 {
            name.push_str(&format!("  … {} more not listed", row.omitted));
        }
        if writeln!(out, "{prefix}{name}").is_err() {
            return;
        }
    }
    let _ = out.flush();
}

/// Accepts `11711`, `:11711`, `0.0.0.0:11711` and `localhost:11711`.
#[cfg(feature = "web")]
fn parse_addr(s: &str) -> Result<std::net::SocketAddr> {
    if let Ok(port) = s.parse::<u16>() {
        return Ok(([127, 0, 0, 1], port).into());
    }
    let s = if let Some(port) = s.strip_prefix(':') {
        format!("127.0.0.1:{port}")
    } else {
        s.to_string()
    };
    use std::net::ToSocketAddrs;
    s.to_socket_addrs()
        .with_context(|| format!("cannot resolve {s}"))?
        .next()
        .with_context(|| format!("no address for {s}"))
}

/// Returns the URLs to advertise, once the server is actually listening.
#[cfg(feature = "web")]
async fn start_web(cli: &Cli, session: &Arc<Session>) -> Result<Option<Vec<String>>> {
    let Some(addr) = &cli.web else {
        return Ok(None);
    };
    let opts = treex::web::WebOptions {
        addr: parse_addr(addr)?,
        preview: (!cli.no_preview).then_some(treex::preview::PreviewOptions {
            max_bytes: cli.max_preview_size,
        }),
        ..Default::default()
    };

    let wanted = opts.addr.port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let session = session.clone();
    tokio::spawn(async move {
        let bind = |bound| {
            let _ = tx.send(bound);
        };
        if let Err(err) = treex::web::serve(session, opts, bind).await {
            eprintln!("treex: web server stopped: {err}");
        }
    });

    let bound = rx.await.context("web server failed to bind")?;
    if bound.port() != wanted {
        eprintln!("treex: port {wanted} was taken, using {}", bound.port());
    }
    if !bound.ip().is_loopback() {
        // The exposure is disclosure, not damage: treex never writes to the
        // filesystem.
        eprintln!(
            "treex: warning \u{2014} {} is reachable off this machine and there is no \
             authentication. Anyone who can reach this port can read your whole \
             directory structure.",
            bound.ip()
        );
    }
    Ok(Some(treex::web::display_urls(bound)))
}

#[cfg(not(feature = "web"))]
async fn start_web(cli: &Cli, _session: &Arc<Session>) -> Result<Option<Vec<String>>> {
    if cli.web.is_some() {
        anyhow::bail!("this build has no web support; rebuild with --features web");
    }
    Ok(None)
}

async fn run(cli: Cli, session: Arc<Session>) -> Result<()> {
    #[cfg(feature = "watch")]
    let _watcher = if cli.no_watch {
        None
    } else {
        Some(treex::watch::watch(
            session.clone(),
            std::time::Duration::from_millis(cli.debounce),
        )?)
    };

    let urls = start_web(&cli, &session).await?;

    let headless = cli.no_tui || !std::io::stdout().is_terminal() || cfg!(not(feature = "tui"));
    if headless {
        match &urls {
            Some(urls) => {
                println!("treex {}", session.root().display());
                for url in urls {
                    println!("  {url}");
                }
            }
            None => anyhow::bail!("--no-tui without --web leaves nothing to do"),
        }
        tokio::signal::ctrl_c().await?;
        return Ok(());
    }

    #[cfg(feature = "tui")]
    {
        let opts = treex::tui::TuiOptions {
            mouse: !cli.no_mouse,
            click_toggles_dirs: !cli.no_click_toggle,
            status_note: urls.as_ref().map(|u| u[0].clone()),
        };
        treex::tui::run(session, opts).await?;
    }
    Ok(())
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::parse_addr;

    #[test]
    fn a_bare_port_means_loopback() {
        assert_eq!(parse_addr("11711").unwrap().to_string(), "127.0.0.1:11711");
        assert_eq!(parse_addr(":11711").unwrap().to_string(), "127.0.0.1:11711");
    }

    #[test]
    fn a_host_and_port_is_taken_as_written() {
        assert_eq!(
            parse_addr("0.0.0.0:8080").unwrap().to_string(),
            "0.0.0.0:8080"
        );
        assert_eq!(
            parse_addr("127.0.0.1:9").unwrap().to_string(),
            "127.0.0.1:9"
        );
    }

    #[test]
    fn a_hostname_is_resolved() {
        assert_eq!(
            parse_addr("localhost:11711").unwrap().port(),
            11711,
            "localhost must resolve"
        );
    }

    #[test]
    fn nonsense_is_reported_rather_than_guessed() {
        for bad in ["", "not a port", "1.2.3.4", "99999", "host:notaport"] {
            assert!(parse_addr(bad).is_err(), "{bad:?} was accepted");
        }
    }
}
