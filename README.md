# ios-lldb-dap

This repository contains a minimal Debug Adapter Protocol (DAP) server written in Rust,
along with a Zed extension that wires the adapter into Zed’s debugger UI. The focus is
on providing accurate symbolication for iOS-style Mach-O binaries (including DWARF
information from dSYM bundles) and creating a clear path from local host testing to
running against an iOS simulator or device via the Luxmentis/xcede tooling.

The workflow is intentionally broken down into four phases so you can validate each
piece quickly.

## Phase 1 – Backend + unit tests + fake DAP harness

1. Build and test everything locally:

   ```bash
   cargo test
   cargo build
   ```

2. The unit tests exercise symbol parsing, backend stack traces, and a small fake DAP
   harness (`tests/dap_harness.rs`). The harness spawns the adapter binary, speaks DAP
   over stdio, and asserts that stack traces are produced even without LLDB connected.
   This provides a fast feedback loop without Zed or iOS.

## Phase 2 – Zed integration with a host binary

1. Install the extension as a dev extension (from the repo root):

   ```bash
   zed extension install --path .
   ```

2. Build the adapter:

   ```bash
   cargo build --bin ios-lldb-dap
   ```

3. Create a `debug.json` (see `debug.host.json` in this repo) that points to a host
   binary on your machine:

   ```json
   {
     "configurations": [
       {
         "label": "Host Debug (ios-lldb)",
         "adapter": "ios-lldb",
         "request": "launch",
         "program": "/absolute/path/to/host/binary",
         "cwd": "/absolute/path/to/project",
         "debugserverPort": 0
       }
     ]
   }
   ```

4. Open the project in Zed, start a debug session using the `"Host Debug (ios-lldb)"`
   configuration, and observe stack traces sourced from the Mach-O/DWARF data of the
   host binary. The adapter binary is located automatically (or via the user-provided
   path) and the config is forwarded through the `IOS_LLDB_DAP_CONFIG` environment
   variable.

## Phase 3 – iOS simulator via Luxmentis/xcede

Assumptions:

* You followed the Luxmentis guides (“Build, run and debug iOS and Mac apps in Zed
  instead of Xcode”, “Test Xcode apps and Swift packages in Zed”).
* xcede and the Xcode build server are configured.
* You can already build and run the iOS app in the simulator from Zed via the default
  xcede adapter.

Example `debug.json` that keeps the existing xcede adapter while adding this adapter:

```json
{
  "configurations": [
    {
      "label": "iOS (xcede default)",
      "adapter": "xcede-ios",
      "request": "launch",
      "program": "MyApp.app",
      "cwd": "/path/to/MyApp",
      "debugserverPort": 12345
    },
    {
      "label": "iOS (ios-lldb custom)",
      "adapter": "ios-lldb",
      "request": "attach",
      "program": "/path/to/MyApp.app/MyApp",
      "cwd": "/path/to/MyApp",
      "debugserverPort": 12345
    }
  ]
}
```

The Luxmentis/xcede tooling:

* Builds the app and launches it in the simulator.
* Starts `debugserver` (or LLDB) listening on a localhost port (e.g. `12345`).

This adapter:

* Reads the `program` path (the simulator’s Mach-O image) to build the symbol context.
* Connects to the provided `debugserverPort`, performs the gdb-remote handshake, and
  issues real packets over TCP. If no server is reachable, packets are queued and
  replayed once a connection is established.
* Symbolicates stack traces and prepares breakpoints using DWARF data.

## Phase 4 – iOS device (optional for now)

The same adapter logic applies when targeting a physical device. The only change is
transport:

1. Use `iproxy` (or similar) to forward the device’s `debugserver` port to localhost.
2. Launch the app on the device using Luxmentis/xcede or your preferred tooling.
3. Reuse the `"ios-lldb"` configuration with the forwarded port, for example:

   ```json
   {
     "label": "iOS Device (ios-lldb)",
     "adapter": "ios-lldb",
     "request": "attach",
     "program": "/path/to/MyApp.app/MyApp",
     "cwd": "/path/to/MyApp",
     "debugserverPort": 23456
   }
   ```

`Backend::connect_debugserver` attempts to open a TCP connection to the forwarded
port, runs the gdb-remote handshake (`qSupported` + `QStartNoAckMode` + `?`), and
immediately starts emitting packets. If the port is not reachable the adapter logs
the error and continues queuing packets so you can retry once the transport is ready.

## Quick Start – Host Only

1. Build and install:

   ```bash
   cargo build --bin ios-lldb-dap
   zed extension install --path .
   ```

2. Generate a configuration:

   ```bash
   cargo run --bin ios-lldb-gendebug -- --program /absolute/path/to/binary --port 0 --write
   ```

3. Open Zed and pick the `"ios-lldb"` entry from `.zed/debug.json`.

## Quick Start – iOS Simulator (Luxmentis/xcede)

1. Ensure `xcede` can run your app in the simulator.
2. Generate a configuration and keep the helper alive:

   ```bash
   cargo run --bin ios-lldb-setup -- --mode sim --project /path/to/YourApp.xcodeproj --scheme YourApp --write --wait
   ```

3. Start debugging from Zed using the generated entry.

## Quick Start – iOS Device (iproxy + xcede)

1. Connect the device and trust the host.
2. Run:

   ```bash
   cargo run --bin ios-lldb-setup -- --mode device --project /path/to/YourApp.xcodeproj --scheme YourApp --write --wait
   ```

   The helper spawns iproxy, forwards the remote port, and writes the configuration.
3. Debug from Zed using the `"ios-lldb"` entry.

## Notes

* The symbolication layer prefers realistic code paths (it uses `addr2line::Loader`
  pointed at real binaries). Tests auto-skip when DWARF isn’t available.
* The DAP adapter discovers its configuration via `IOS_LLDB_DAP_CONFIG`, which is set
  by the Zed extension (see `src/lib.rs`).
* Neither the Zed core repository nor the Luxmentis tooling is modified – this project
  remains an external adapter/extension.
