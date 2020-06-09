#[macro_use]
extern crate log;

mod advisory;
mod nix;
mod package;
mod scan;
#[cfg(test)]
mod tests;
mod ticket;
mod tracker;

use crate::scan::{Branch, Branches, MaintByBranch};
use crate::ticket::Ticket;
use crate::tracker::Tracker;

use anyhow::{bail, Context, Error};
use colored::*;
use env_logger::Env;
use std::borrow::Borrow;
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use tokio::runtime::Runtime;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, StructOpt, Default)]
#[structopt(
    rename_all = "kebab",
    after_help = "\
    Set RUST_LOG=survey=debug in the environment to get full logging output.
    "
)]
pub struct Opt {
    /// Path to `nixpkgs` checkout
    #[structopt(
        short,
        long,
        value_name = "PATH",
        default_value = "nixpkgs",
        parse(from_os_str)
    )]
    nixpkgs: PathBuf,
    /// Base directory for vulnix JSON outputs and tickets (excluding iteration subdir)
    #[structopt(
        short = "o",
        long = "outdir",
        value_name = "DIR",
        default_value = "iterations",
        parse(from_os_str)
    )]
    basedir: PathBuf,
    /// Directory for updated whitelists
    #[structopt(
        short = "w",
        long,
        value_name = "DIR",
        default_value = "whitelists",
        parse(from_os_str)
    )]
    whitelist_dir: PathBuf,
    /// Path to `vulnix` executable
    #[structopt(
        short,
        long,
        value_name = "PATH",
        default_value = "vulnix",
        parse(from_os_str)
    )]
    vulnix: PathBuf,
    /// Don't run vulnix (expects vulnix JSON output already present in iteration dir)
    #[structopt(short = "R", long)]
    no_run: bool,
    /// Create GitHub issues in this repository
    #[structopt(short, long, value_name = "USER/REPO")]
    repo: Option<String>,
    /// GitHub access token
    ///
    /// Alternatively set the GITHUB_TOKEN environment variable
    #[structopt(short, long, env = "GITHUB_TOKEN")]
    github_token: Option<String>,
    /// Ping package maintainers
    #[structopt(short = "m", long)]
    ping_maintainers: bool,
    /// Nth survey iteration
    #[structopt(value_name = "N")]
    iteration: u32,
    /// List of nixpkgs branches to scan
    ///
    /// Format: BRANCH=COMMITID or just BRANCH (uses current branch head).
    /// Examples: "nixos-unstable=55f4cd48" or "nixos-18.09"
    #[structopt(value_name = "BRANCHES", required = true)]
    branches: Vec<Branch>,
}

impl Opt {
    /// Constructs per-iteration directory from basedir and iteration number
    pub fn iterdir(&self) -> PathBuf {
        self.basedir.join(self.iteration.to_string())
    }

    /// Full path to JSON file containing maintainers
    fn maint_json(&self, branch: &str) -> PathBuf {
        self.iterdir().join(format!("maintainers.{}.json", branch))
    }

    /// Full path to JSON file containing vulnix scan results
    fn vulnix_json(&self, branch: &str) -> PathBuf {
        self.iterdir().join(format!("vulnix.{}.json", branch))
    }
}

async fn create(tkt: Ticket, iterdir: &Path, tracker: &dyn Tracker) -> Result<()> {
    let f = iterdir.join(tkt.file_name());
    let name = tkt.name().to_owned();
    if f.exists() {
        warn!("{}: skipping, file exists", name.yellow());
        return Ok(());
    }
    let tkt = tracker
        .create_issue(tkt)
        .await
        .with_context(|| format!("Failed to create issue for {}", name.purple().bold()))?;
    tkt.write(&f)?;
    Ok(())
}

async fn issues(tickets: Vec<Ticket>, iterdir: &Path, tracker: &dyn Tracker) -> Result<()> {
    info!("Creating issues");
    let handles: Vec<_> = tickets
        .into_iter()
        .map(|tkt| create(tkt, iterdir, tracker))
        .collect();
    for hdl in handles {
        hdl.await?;
    }
    Ok(())
}

fn run() -> Result<()> {
    dotenv::dotenv().ok();
    let opt = Opt::from_args();
    let branches = Branches::with_repo(&opt.branches, &opt.nixpkgs)?;
    let dir = opt.iterdir();
    let tracker: Box<dyn Tracker> = match (&opt.repo, &opt.github_token) {
        (Some(repo), Some(token)) => Box::new(tracker::GitHub::new(token.to_string(), repo)?),
        (Some(_), None) => bail!(
            "No Github access token given either as option or via the GITHUB_TOKEN environment \
             variable"
        ),
        (_, _) => Box::new(tracker::Null::new()),
    };
    let (sbb, mbb) = if opt.no_run {
        branches.load(&opt)?
    } else {
        branches.scan(&opt)?
    };
    Runtime::new().unwrap().block_on(issues(
        ticket::ticket_list(
            opt.iteration,
            sbb,
            if opt.ping_maintainers {
                mbb
            } else {
                MaintByBranch::default()
            },
        ),
        &dir,
        tracker.borrow(),
    ))?;
    Ok(())
}

fn main() {
    env_logger::from_env(Env::default().default_filter_or("info")).init();
    if let Err(err) = run() {
        for e in err.chain() {
            error!("{}", e);
            // reqwest seems to fold all causes into its head error
            if e.downcast_ref::<reqwest::Error>().is_some() {
                break;
            }
        }
        std::process::exit(1);
    }
}
