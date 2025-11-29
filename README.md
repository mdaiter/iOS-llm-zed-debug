# ios-lldb-dap

Rust-powered tooling for debugging Swift/iOS binaries anywhere—Zed, your shell,
or Claude Code. This repo bundles:

* **`ios-lldb-dap`** – a minimal Debug Adapter Protocol server. Any editor
  (Zed, VS Code, Neovim, Helix, etc.) can point its DAP client at this binary.
* **CLI helpers** – `ios-llm-devicectl` and `ios_llm_api` for pairing devices,
  launching `debugserver`, and driving breakpoints completely from the terminal.
* **Claude automation** – `/command`, `/health`, and `/logs` endpoints plus a
  documented tool schema so LLM agents can run the entire build→deploy→debug loop.

The adapter understands Mach-O + dSYM DWARF data, so you get real symbolication
when stepping through Swift.

---

## Getting Started (host only)

```bash
cargo test --features cli                      # run unit + DAP harness tests
cargo build --features cli --bin ios-lldb-dap  # build the adapter

# Optional: install the Zed extension
zed extension install --path .
```

You can now debug any macOS binary by pointing your editor’s DAP config at the
`ios-lldb-dap` binary and supplying `program`, `cwd`, and (optionally) a
`debugserverPort` if you’re attaching.

---

## Running against debugserver manually

You don’t need Xcode UI or Zed—just the CLI:

```bash
PORT=50001
BIN=/path/to/your/Mach-O

# 1. Start debugserver (from Xcode’s toolchain)
/Applications/Xcode.app/Contents/SharedFrameworks/LLDB.framework/Versions/A/Resources/debugserver \
  127.0.0.1:$PORT "$BIN"

# 2. Start the HTTP shim
cargo run --features cli --bin ios-llm-api -- \
  --debugserver-port $PORT \
  --program "$BIN" \
  --port 4000

# 3. Drive it via the tool stub or Claude
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file Sources/Foo.swift --line 42
python tools/claude_tool_stub.py --action continue
```

You’ll see stack traces and breakpoints without leaving the shell.

---

## Full device/simulator workflow

For simulators or devices, use the dedicated helpers:

```bash
# Build or install in Debug (DWARF) as usual
xcodebuild -scheme MyApp -configuration Debug -destination 'id=<udid>'

# Launch bridge + shim automatically
DEVICE=<udid> \
BUNDLE_ID=com.example.MyApp \
APP_BUNDLE=/absolute/path/MyApp.app \
make autonomy
```

The `autonomy` target runs `ios-llm-devicectl` (`--start-stopped`, optional
install) and `ios_llm_api --manage-bridge --enable-log-stream`, then waits for
`/health` to report success. You can watch logs with `curl -Ns
http://127.0.0.1:4000/logs` and interact over `/command`.

Documentation for Claude automation lives in:

* `docs/CLAUDE_AUTONOMY.md` – mission overview, required commands, safeguards.
* `docs/CLAUDE_TOOL.md` – the `ios_debug_command` schema and response contracts.
* `docs/DEBUGGING.md` – log patterns, troubleshooting steps, severity levels.

Drop those into your CLAUDE.md chain and register the tool to let Claude
deploy/debug autonomously.

---

## Editor integration (Zed & friends)

The Zed extension is included (`extension.toml`) but it’s optional—any editor
with DAP support works. For Zed:

1. `zed extension install --path .`
2. Generate `.zed/debug.json` via
   `cargo run --features cli --bin ios-lldb-gendebug -- --program /absolute/path --port 0 --write`
3. Pick the `ios-lldb` profile inside Zed.

Other editors just need a DAP config pointing to the `ios-lldb-dap` binary and
the same arguments.

---

## DWARF requirements

Breakpoints rely on DWARF line tables. Always build your Mach-O (or its `.dSYM`)
with Debug info (`-g`, `SWIFT_OPTIMIZATION_LEVEL=-Onone`, or Xcode’s Debug
configuration). When `ios_llm_api` starts it inspects the provided path and
warns if DWARF is missing. Pass `--require-dwarf` to force the process to abort
instead of running without symbolication.

---

## Advanced features

`ios_llm_api` exposes more than stack traces:

| Capability | Command |
|------------|---------|
| Watch expressions | `watch_expr` / `evaluate_swift` |
| Thread control | `threads`, `select_thread` |
| Session management | `restart`, `launch`, `disconnect` |
| Build hook | `build` (when `--build-cmd` provided) |
| Logs & health | `GET /logs`, `GET /health` |

All of these are covered in `docs/CLAUDE_TOOL.md` and exercised by
`tools/claude_tool_stub.py`.

---

## Helpful references

* `tests/dap_harness.rs` – proves the DAP adapter works even without LLDB.
* `src/bin/ios-llm-devicectl.rs` – how we wrap `xcrun devicectl`.
* `src/bin/ios_llm_api.rs` – the HTTP shim plus log streaming and restart logic.

We built this so fellow Swift developers (and the broader Apple ecosystem) can
debug with the tooling they like—command line, editor, or Claude Code. If you
share it with the Swift Reddit crowd, just remind them they need DWARF-enabled
builds and the Xcode CLI tools installed. Happy debugging!
