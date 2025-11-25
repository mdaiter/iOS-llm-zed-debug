use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};

#[path = "../cli.rs"]
mod cli;

use cli::{load_debug_json, save_debug_json, AdapterConfig};

#[derive(Debug, Parser)]
#[command(about = "Drive Luxmentis/xcede + iproxy flows and emit Zed configs")]
struct Args {
    #[arg(long, value_enum, default_value = "host")]
    mode: Mode,
    /// Path to the Xcode project/workspace root.
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// Scheme to build (required for sim/device).
    #[arg(long)]
    scheme: Option<String>,
    /// Binary to use for symbolication (host mode).
    #[arg(long)]
    program: Option<PathBuf>,
    /// CWD for the debuggee.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Override debugserver port.
    #[arg(long)]
    port: Option<u16>,
    /// Gendebug label.
    #[arg(long, default_value = "ios-lldb")]
    label: String,
    /// Request type.
    #[arg(long, value_enum, default_value = "attach")]
    request: RequestKind,
    /// Path to debug.json (default .zed/debug.json when --write).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Write config to debug.json.
    #[arg(long)]
    write: bool,
    /// Path to `xcede` binary.
    #[arg(long, default_value = "xcede")]
    xcede: String,
    /// Additional xcede arguments (pass multiple times).
    #[arg(long)]
    xcede_arg: Vec<String>,
    /// iproxy binary path (device mode).
    #[arg(long, default_value = "iproxy")]
    iproxy: String,
    /// Remote device port for debugserver (device mode).
    #[arg(long, default_value_t = 2331)]
    device_port: u16,
    /// Keep the helper process alive awaiting Enter key (useful for iproxy).
    #[arg(long)]
    wait: bool,
}

#[derive(Debug, Clone, ValueEnum)]
enum Mode {
    Host,
    Sim,
    Device,
}

#[derive(Debug, Clone, ValueEnum)]
enum RequestKind {
    Launch,
    Attach,
}

impl RequestKind {
    fn as_str(&self) -> &'static str {
        match self {
            RequestKind::Launch => "launch",
            RequestKind::Attach => "attach",
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.mode {
        Mode::Host => host_flow(&args),
        Mode::Sim => sim_flow(&args),
        Mode::Device => device_flow(&args),
    }
}

fn host_flow(args: &Args) -> anyhow::Result<()> {
    let program = args
        .program
        .as_ref()
        .context("--program is required in host mode")?;
    let program = dunce::canonicalize(program)?;
    let cwd = args
        .cwd
        .clone()
        .unwrap_or_else(|| program.parent().unwrap().to_path_buf());
    let port = args.port.unwrap_or(0);

    emit_config(args, &program, &cwd, port)
}

fn sim_flow(args: &Args) -> anyhow::Result<()> {
    let info = run_xcede(args)?;
    let program = info
        .app_binary
        .clone()
        .context("xcede output missing app_binary; pass --program manually")?;
    let cwd = args.cwd.clone().unwrap_or_else(|| args.project.clone());
    let port = args.port.or(info.debugserver_port).unwrap_or(0);
    emit_config(args, &program, &cwd, port)
}

fn device_flow(args: &Args) -> anyhow::Result<()> {
    let local_port = args.port.unwrap_or(23456);
    ensure_port_free(local_port)?;
    let info = run_xcede(args)?;
    let program = info
        .app_binary
        .clone()
        .context("xcede output missing app_binary; pass --program manually")?;
    let cwd = args.cwd.clone().unwrap_or_else(|| args.project.clone());
    let remote_port = info.debugserver_port.unwrap_or(args.device_port);

    let mut iproxy = Command::new(&args.iproxy)
        .arg(local_port.to_string())
        .arg(remote_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn iproxy")?;
    println!(
        "iproxy started on port {local_port} -> device {remote_port}. Press Ctrl+C to terminate."
    );

    let result = emit_config(args, &program, &cwd, local_port);
    if args.wait {
        let mut input = String::new();
        let _ = std::io::stdin().read_line(&mut input);
    }
    let _ = iproxy.kill();
    result
}

fn emit_config(args: &Args, program: &Path, cwd: &Path, port: u16) -> anyhow::Result<()> {
    let entry = AdapterConfig {
        label: args.label.clone(),
        adapter: "ios-lldb".into(),
        request: args.request.as_str().into(),
        program: program.display().to_string(),
        cwd: cwd.display().to_string(),
        debugserver_port: port,
    };
    if args.write {
        let output = args
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from(".zed/debug.json"));
        let mut json = load_debug_json(&output)?;
        cli::upsert_configuration(&mut json.configurations, entry.clone());
        save_debug_json(&output, &json)?;
        println!(
            "Wrote configuration \"{}\" to {}",
            entry.label,
            output.display()
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&entry)?);
    }
    println!("program: {}", entry.program);
    println!("cwd    : {}", entry.cwd);
    println!("port   : {}", entry.debugserver_port);
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct XcedeInfo {
    #[serde(rename = "debugserver_port")]
    debugserver_port: Option<u16>,
    #[serde(rename = "app_binary")]
    app_binary: Option<PathBuf>,
}

fn run_xcede(args: &Args) -> anyhow::Result<XcedeInfo> {
    let scheme = args
        .scheme
        .as_deref()
        .context("--scheme is required for simulator/device modes")?;
    let mut command = Command::new(&args.xcede);
    command.arg("debug-session");
    command.arg("--scheme");
    command.arg(scheme);
    command.arg("--project");
    command.arg(&args.project);
    for extra in &args.xcede_arg {
        command.arg(extra);
    }
    command.stdout(Stdio::piped());
    let output = command.output().context("failed to run xcede")?;
    if !output.status.success() {
        bail!(
            "xcede failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let info: XcedeInfo = serde_json::from_slice(&output.stdout).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse xcede JSON (stdout={}): {err}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    Ok(info)
}

fn ensure_port_free(port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("port {port} is already in use"))?;
    drop(listener);
    Ok(())
}
