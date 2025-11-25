use std::{
    env,
    io::Write,
    process::{Command, Stdio},
};

use serde_json::json;

#[test]
fn dap_harness_produces_stack_trace() {
    let bin = match env::var("CARGO_BIN_EXE_ios-lldb-dap") {
        Ok(path) => path,
        Err(_) => {
            eprintln!("CARGO_BIN_EXE_ios-lldb-dap missing; skipping harness test");
            return;
        }
    };

    let exe = env::current_exe().expect("current_exe");
    let program = exe.to_string_lossy().to_string();
    let cwd = exe.parent().unwrap().to_string_lossy().to_string();
    let config = json!({
        "request": "launch",
        "program": program,
        "cwd": cwd,
        "debugserverPort": 0
    })
    .to_string();

    let mut child = Command::new(bin)
        .env("IOS_LLDB_DAP_CONFIG", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ios-lldb-dap");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        send_request(
            &mut stdin,
            json!({
                "seq": 1,
                "type": "request",
                "command": "initialize",
                "arguments": {}
            }),
        );
        send_request(
            &mut stdin,
            json!({
                "seq": 2,
                "type": "request",
                "command": "launch",
                "arguments": {
                    "debugserverPort": 0,
                    "program": program,
                    "cwd": cwd
                }
            }),
        );
        send_request(
            &mut stdin,
            json!({
                "seq": 3,
                "type": "request",
                "command": "threads",
                "arguments": {}
            }),
        );
        send_request(
            &mut stdin,
            json!({
                "seq": 4,
                "type": "request",
                "command": "stackTrace",
                "arguments": {
                    "threadId": 1
                }
            }),
        );
        send_request(
            &mut stdin,
            json!({
                "seq": 5,
                "type": "request",
                "command": "disconnect",
                "arguments": {}
            }),
        );
    }

    let output = child.wait_with_output().expect("wait on child");
    if !output.status.success() {
        eprintln!(
            "dap server stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("dap server exited with {:?}", output.status);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(r#""type":"response""#),
        "expected at least one response, got: {stdout}"
    );
    assert!(
        stdout.contains(r#""stackFrames""#),
        "expected stackFrames payload, got: {stdout}"
    );
}

fn send_request(stdin: &mut impl Write, payload: serde_json::Value) {
    let body = serde_json::to_string(&payload).expect("serialize payload");
    let message = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(message.as_bytes()).expect("write request");
    stdin.flush().expect("flush request");
}
