# CLAUDE_TOOL.md – /command Schema & Usage

**[USER CONFIGURATION REQUIRED]**

This file documents the single `ios_debug_command` tool exposed by
`ios_llm_api`. Claude references it whenever issuing debugger commands so it
knows the schema, expected responses, and troubleshooting steps.

---

## 📋 Log Format & Patterns

```
Command Payload:
{ "action": "<name>", ...args }

Success Response:
{ "ok": true, ...payload }

Failure Response:
{ "ok": false, "error": "<message>" }

All commands are POSTed to http://127.0.0.1:<port>/command with JSON bodies.
```

---

## 🔍 Common Error Patterns

```
ERROR: "expression `<expr>` is not supported"
Cause: evaluate/watch called with unknown variable.
Fix: Inspect locals (`locals` command) or use `evaluate_swift`.

ERROR: "restart requires --manage-bridge"
Cause: ios_llm_api not launched with --manage-bridge.
Fix: Restart shim with that flag or avoid restart/launch commands.

ERROR: "build command not configured"
Cause: No --build-cmd provided.
Fix: Start ios_llm_api with `--build-cmd <script>` or skip build requests.
```

---

## 🔎 Diagnostic Procedures

```
Issue: Tool call timed out / refused.
1. curl -sf http://127.0.0.1:4000/health
2. If failing, relaunch ios_llm_api.
3. Check /logs for backend errors.

Issue: Breakpoint didn’t set.
1. Confirm DWARF warning not logged.
2. Ensure `file` path matches DWARF line paths.
3. Use `watch_expr` to confirm symbol resolution.
```

---

## 🚨 Error Categories & Priority

```
Critical: { ok:false, error: "failed to connect to debugserver" }
High: { ok:false, error: "build command not configured" }
Medium: { ok:false, error: "expression ... is not supported" }
Low: Informational messages (missing watch, duplicate breakpoint).
```

---

## 🔧 Debugging Tools & Techniques

```
Registered Tool:
name: ios_debug_command
description: Send debugger command to ios-LLDB HTTP bridge.

Input Schema (action enum):
- stacktrace, threads, continue, next, step_in
- set_breakpoint (requires file + line)
- locals, scopes, variables (optional variablesReference)
- evaluate, evaluate_swift (requires expression)
- watch_expr (requires expression; stores watch)
- select_thread (requires threadId)
- restart, launch (require --manage-bridge)
- build (requires --build-cmd)
- disconnect

Python stub usage:
python tools/claude_tool_stub.py --action stacktrace
python tools/claude_tool_stub.py --action set_breakpoint --file ViewController.swift --line 42
python tools/claude_tool_stub.py --action continue
```

---

## 📊 Performance Debugging

```
Monitor latency: tool calls should complete < 1s.
If requests stall:
 - Check bridge logs for network contention.
 - Ensure device is still connected.
```

---

## 🐛 Known Issues & Workarounds

```
Known: evaluate_swift currently mirrors evaluate.
Workaround: Use locals/watch expressions for now.

Known: restart/launch only work when ios_llm_api controls the bridge.
Workaround: Run `make autonomy` (which sets --manage-bridge) or restart manually.
```

---

## 📈 Monitoring & Alerts

```
Expose metrics by counting `ok:false` responses.
For automation: alert if >3 consecutive failures per action.
Watch `/logs` for repeated restart/build failures.
```

---

## 🔄 Debugging Workflow

```
1. stacktrace – capture current frames.
2. set_breakpoint – provide file + line.
3. continue – resume execution.
4. watch_expr / evaluate – inspect state when stopped.
5. restart / launch – reattach if process exits.
6. build – rebuild before repeating the loop.
```

---

## 📝 Configuration Guide

1. Keep this schema in sync with `src/bin/ios_llm_api.rs`.
2. Update when new actions are added or payloads change.
3. Reference from CLAUDE.md so the agent always knows how to call the tool.
