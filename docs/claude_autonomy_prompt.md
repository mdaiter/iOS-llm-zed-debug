# CLAUDE_AUTONOMY.md – Deployment & Debug Orchestration

**[USER CONFIGURATION REQUIRED]**

This runbook teaches Claude (or any orchestrator agent) how to take a raw prompt
like “build and debug MyApp on device X” and turn it into a working deployment
with iOS-lldb-dap, `ios-llm-devicectl`, and `ios_llm_api`. Keep it updated so
agents follow the correct procedures every time.

---

## 📋 Log Format & Patterns

**Purpose**: Explain how the tooling emits logs so Claude can monitor progress.

**Structure**:
```
[TIMESTAMP] [LEVEL] [COMPONENT] Message
Example:
2025-02-10T12:00:01Z [INFO] [Bridge] Process 1234 for bundle com.example.MyApp is suspended
2025-02-10T12:00:05Z [WARN] [ios_llm_api] DWARF line info missing for /path/MyApp

Log Levels:
- INFO: Normal lifecycle steps (install, launch, connect)
- WARN: Recoverable issues (missing DWARF, connection retries)
- ERROR: Action failed (devicectl exited, build failed)

Destinations:
- stdout/stderr for both binaries
- `/logs` SSE endpoint from `ios_llm_api --enable-log-stream`
- Optional files ignored via `.gitignore` (e.g., `bridge.log`, `ios_llm_api.log`)
```

---

## 🔍 Common Error Patterns

```
ERROR: "DWARF line info missing for <path>"
Cause: App built without Debug symbols.
Solution: Rebuild with `xcodebuild -configuration Debug` or Swift `-g`. Retry bridge.

ERROR: "failed to spawn ios-llm-devicectl bridge"
Cause: Missing device pairing, invalid bundle id, or devicectl not on PATH.
Solution: Run pairing commands, verify `--bundle-id`, ensure `xcrun` available.

ERROR: "timed out waiting for bridge on port <N>"
Cause: debugserver tunnel failed to bind.
Solution: Check if another process owns the port, ensure device trust dialog accepted.
```

---

## 🔎 Diagnostic Procedures

```
Issue: "Claude can't reach /command"

Steps:
1. Run `curl -sf http://127.0.0.1:4000/health`.
2. If it fails, restart `ios_llm_api` with `--manage-bridge`.
3. Inspect bridge logs (stdout or `/logs` SSE) for devicectl errors.
4. Confirm `ios-llm-devicectl` still running (`ps aux | grep ios-llm`).
5. Reconnect using `LlmCommand::restart`.

Runnable example:
make autonomy DEVICE=<udid> BUNDLE_ID=com.example.MyApp APP_BUNDLE=/path/MyApp.app
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue
```

---

## 🚨 Error Categories & Priority

```
Critical (P0):
- Bridge not starting, debugserver unreachable, /health fails.
- Response: Immediate restart (`restart` command or rebuild + relaunch).

High (P1):
- Build failures, missing DWARF, pairing errors.
- Response: Repair environment within 1 hour.

Medium (P2):
- Log-stream interruptions, watch expressions failing.
- Response: Later in the day.

Low (P3):
- Cosmetic logs, informational warnings.
- Response: Next maintenance window.
```

---

## 🔧 Debugging Tools & Techniques

```
Core commands Claude should know:
- Pair/mount: `xcrun devicectl manage pair --device <udid>` and `manage ddis install`.
- Build: `xcodebuild -scheme <scheme> -configuration Debug -destination 'id=<udid>'`.
- Bridge: `cargo run --features cli --bin ios-llm-devicectl -- ...`.
- HTTP shim: `cargo run --features cli --bin ios_llm_api -- --manage-bridge ...`.
- Logs: `curl -N http://127.0.0.1:4000/logs`.
- Debug actions: `ios_debug_command` tool (see `docs/claude_tool.md`).
```

---

## 📊 Performance Debugging

```
Monitor:
- Bridge startup time (target < 10s).
- `/health` latency (should return immediately).
- Log-stream throughput (no gaps > 30s).
When slow: inspect `ios_llm_api` logs for repeated reconnects or build retries.
```

---

## 🐛 Known Issues & Workarounds

```
Issue: devicectl prompts for trust mid-run.
Workaround: unlock device and accept prompt manually, then re-run `make autonomy`.

Issue: Build command too long.
Workaround: create a shell wrapper (e.g., `./scripts/build_debug.sh`) and reference it in `--build-cmd`.
```

---

## 📈 Monitoring & Alerts

```
Monitoring Tools:
- `/health`: ensures shim responds with program + ports.
- `/logs`: stream to watch for fatal errors.

Alerting (manual for now):
- If `/health` fails twice, restart bridge.
- If log stream closes unexpectedly, call `restart`.
```

---

## 🔄 Debugging Workflow

```
1. Pair/mount device.
2. Build app in Debug (DWARF).
3. `make autonomy` (or manual bridge + API).
4. Wait for `/health`.
5. Use `ios_debug_command` (`stacktrace`, `set_breakpoint`, `continue`, etc.).
6. If app crashes, call `restart` then resume debugging.
7. Use `build` command when code changes require rebuild.
```

---

## 📝 Configuration Guide

1. Export required env vars before `make autonomy` (DEVICE, BUNDLE_ID, APP_BUNDLE).
2. Maintain `.zed/ios-llm-state.json`; it feeds `APP_PROGRAM`.
3. Update this doc when workflows change or new commands become available.
