use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};

#[path = "../cli.rs"]
mod cli;

use cli::{load_debug_json, save_debug_json, AdapterConfig, DebugJson};

#[derive(Debug, Parser)]
#[command(about = "Generate or update Zed debug.json entries for ios-lldb")]
struct Args {
    /// Path to the debuggee binary (Mach-O).
    #[arg(long)]
    program: PathBuf,
    /// Working directory for the debuggee (defaults to the parent of program).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Debugserver port to use.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Display label for the configuration.
    #[arg(long, default_value = "ios-lldb")]
    label: String,
    /// Request kind.
    #[arg(long, value_enum, default_value = "attach")]
    request: RequestKind,
    /// Output file (defaults to .zed/debug.json if --write is set).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Update the output file instead of printing to stdout.
    #[arg(long)]
    write: bool,
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
    let program = fs::canonicalize(&args.program)?;
    let cwd = args
        .cwd
        .clone()
        .or_else(|| program.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let entry = AdapterConfig {
        label: args.label.clone(),
        adapter: "ios-lldb".into(),
        request: args.request.as_str().into(),
        program: program.display().to_string(),
        cwd: cwd.display().to_string(),
        debugserver_port: args.port,
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
            "Updated {} with configuration \"{}\"",
            output.display(),
            entry.label
        );
    } else {
        let mut json = DebugJson::default();
        json.configurations.push(entry.clone());
        println!("{}", serde_json::to_string_pretty(&json)?);
    }

    println!("program : {}", entry.program);
    println!("cwd     : {}", entry.cwd);
    println!("adapter : {}", entry.adapter);
    println!("request : {}", entry.request);
    println!("port    : {}", entry.debugserver_port);
    Ok(())
}
