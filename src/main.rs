mod backend;
mod gdb_remote;
mod symbols;

use backend::{Backend, BackendStopEvent};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

const CONFIG_ENV_VAR: &str = "IOS_LLDB_DAP_CONFIG";

fn main() -> io::Result<()> {
    let _ = env_logger::builder().format_timestamp(None).try_init();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let writer = BufWriter::new(stdout.lock());
    let backend = init_backend()?;
    let mut session = Session::new(backend, writer);

    while let Some(message) = read_dap_message(&mut reader)? {
        let envelope: DapEnvelope = match serde_json::from_str(&message) {
            Ok(payload) => payload,
            Err(err) => {
                eprintln!("Failed to parse DAP message: {err}");
                continue;
            }
        };

        if let DapEnvelope::Request(request) = envelope {
            if !session.handle_request(request)? {
                break;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum DapEnvelope {
    #[serde(rename = "request")]
    Request(RawRequest),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawRequest {
    seq: i64,
    command: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
struct LaunchArguments {
    #[serde(rename = "debugserverPort")]
    debugserver_port: u16,
    program: String,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct AttachArguments {
    #[serde(rename = "debugserverPort")]
    debugserver_port: u16,
    program: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct SetBreakpointsArguments {
    source: Source,
    #[serde(default)]
    breakpoints: Vec<SourceBreakpoint>,
}

#[derive(Deserialize)]
struct Source {
    path: Option<String>,
}

#[derive(Deserialize)]
struct SourceBreakpoint {
    line: i64,
}

#[derive(Deserialize)]
struct StackTraceArguments {
    #[serde(rename = "threadId")]
    thread_id: i64,
}

#[derive(Deserialize)]
struct VariablesArguments {
    #[serde(rename = "variablesReference")]
    variables_reference: i64,
}

#[derive(Deserialize)]
struct ThreadArguments {
    #[serde(rename = "threadId")]
    thread_id: i64,
}

#[derive(Deserialize)]
struct ScopesArguments {
    #[serde(rename = "frameId")]
    _frame_id: i64,
}

struct Session<W: Write> {
    next_seq: i64,
    initialized: bool,
    backend: Backend,
    writer: W,
}

impl<W: Write> Session<W> {
    fn new(backend: Backend, writer: W) -> Self {
        Self {
            next_seq: 1,
            initialized: false,
            backend,
            writer,
        }
    }

    fn handle_request(&mut self, request: RawRequest) -> io::Result<bool> {
        let RawRequest {
            seq,
            command,
            arguments,
        } = request;
        let command_str = command.as_str();
        match command_str {
            "initialize" => self.handle_initialize(seq, command_str),
            "launch" => self.handle_launch(seq, command_str, arguments),
            "attach" => self.handle_attach(seq, command_str, arguments),
            "setBreakpoints" => self.handle_set_breakpoints(seq, command_str, arguments),
            "configurationDone" => self.handle_simple_ok(seq, command_str, Value::Null),
            "threads" => self.handle_threads(seq, command_str),
            "stackTrace" => self.handle_stack_trace(seq, command_str, arguments),
            "scopes" => self.handle_scopes(seq, command_str, arguments),
            "variables" => self.handle_variables(seq, command_str, arguments),
            "continue" => self.handle_continue(seq, command_str, arguments),
            "next" => self.handle_next(seq, command_str, arguments),
            "stepIn" => self.handle_step_in(seq, command_str, arguments),
            "disconnect" => self.handle_disconnect(seq, command_str),
            _ => {
                self.send_error_response(seq, command_str, format!("Unknown command: {command}"))?;
                Ok(true)
            }
        }
    }

    fn handle_initialize(&mut self, seq: i64, command: &str) -> io::Result<bool> {
        self.initialized = true;
        self.respond(
            seq,
            command,
            true,
            Some(json!({
                "supportsConfigurationDoneRequest": true,
            })),
            None,
        )?;
        self.emit_event("initialized", Value::Null)?;
        Ok(true)
    }

    fn handle_launch(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: LaunchArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };

        if let Err(err) = self.backend.connect_debugserver(args.debugserver_port) {
            self.send_error_response(seq, command, err)?;
            return Ok(true);
        }

        self.handle_simple_ok(
            seq,
            command,
            json!({
                "program": args.program,
                "cwd": args.cwd,
                "debugserverPort": args.debugserver_port,
            }),
        )
    }

    fn handle_attach(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: AttachArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };

        if let Err(err) = self.backend.connect_debugserver(args.debugserver_port) {
            self.send_error_response(seq, command, err)?;
            return Ok(true);
        }

        self.handle_simple_ok(
            seq,
            command,
            json!({
                "program": args.program,
                "cwd": args.cwd,
                "debugserverPort": args.debugserver_port,
            }),
        )
    }

    fn handle_set_breakpoints(
        &mut self,
        seq: i64,
        command: &str,
        arguments: Value,
    ) -> io::Result<bool> {
        let args: SetBreakpointsArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };

        let Some(path) = args.source.path else {
            self.send_error_response(seq, command, "source.path missing".to_string())?;
            return Ok(true);
        };

        let lines: Vec<i64> = args.breakpoints.iter().map(|bp| bp.line).collect();
        if let Err(err) = self.backend.update_breakpoints(&path, &lines) {
            self.send_error_response(seq, command, err)?;
            return Ok(true);
        }

        let breakpoints: Vec<_> = args
            .breakpoints
            .into_iter()
            .map(|bp| {
                json!({
                    "verified": true,
                    "line": bp.line,
                })
            })
            .collect();

        self.handle_simple_ok(seq, command, json!({ "breakpoints": breakpoints }))
    }

    fn handle_threads(&mut self, seq: i64, command: &str) -> io::Result<bool> {
        self.handle_simple_ok(seq, command, json!({ "threads": self.backend.threads() }))
    }

    fn handle_stack_trace(
        &mut self,
        seq: i64,
        command: &str,
        arguments: Value,
    ) -> io::Result<bool> {
        let args: StackTraceArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        let frames = self.backend.stack_trace(args.thread_id);
        self.handle_simple_ok(
            seq,
            command,
            json!({
                "stackFrames": frames,
                "totalFrames": 2,
            }),
        )
    }

    fn handle_scopes(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let _args: ScopesArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };

        self.handle_simple_ok(seq, command, json!({ "scopes": self.backend.scopes() }))
    }

    fn handle_variables(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: VariablesArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        self.handle_simple_ok(
            seq,
            command,
            json!({ "variables": self.backend.variables(args.variables_reference) }),
        )
    }

    fn handle_continue(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: ThreadArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        let stop_event = match self.backend.r#continue(args.thread_id) {
            Ok(event) => event,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        self.handle_simple_ok(seq, command, json!({ "allThreadsContinued": true }))?;
        if let Some(event) = stop_event {
            self.emit_stop_event(event)?;
        }
        Ok(true)
    }

    fn handle_next(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: ThreadArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        let stop_event = match self.backend.step_over(args.thread_id) {
            Ok(event) => event,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        self.handle_simple_ok(seq, command, Value::Null)?;
        if let Some(event) = stop_event {
            self.emit_stop_event(event)?;
        }
        Ok(true)
    }

    fn handle_step_in(&mut self, seq: i64, command: &str, arguments: Value) -> io::Result<bool> {
        let args: ThreadArguments = match parse_arguments(arguments) {
            Ok(args) => args,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        let stop_event = match self.backend.step_in(args.thread_id) {
            Ok(event) => event,
            Err(err) => {
                self.send_error_response(seq, command, err)?;
                return Ok(true);
            }
        };
        self.handle_simple_ok(seq, command, Value::Null)?;
        if let Some(event) = stop_event {
            self.emit_stop_event(event)?;
        }
        Ok(true)
    }

    fn handle_disconnect(&mut self, seq: i64, command: &str) -> io::Result<bool> {
        if let Err(err) = self.backend.disconnect() {
            self.send_error_response(seq, command, err)?;
            return Ok(true);
        }
        self.handle_simple_ok(seq, command, Value::Null)?;
        Ok(false)
    }

    fn handle_simple_ok(&mut self, seq: i64, command: &str, body: Value) -> io::Result<bool> {
        let body = if body.is_null() { None } else { Some(body) };
        self.respond(seq, command, true, body, None)?;
        Ok(true)
    }

    fn respond(
        &mut self,
        request_seq: i64,
        command: &str,
        success: bool,
        body: Option<Value>,
        message: Option<String>,
    ) -> io::Result<()> {
        let response = Response {
            seq: self.next_seq(),
            r#type: "response",
            request_seq,
            success,
            command,
            message,
            body,
        };
        write_dap_message(&mut self.writer, &response)
    }

    fn send_error_response(
        &mut self,
        request_seq: i64,
        command: &str,
        message: String,
    ) -> io::Result<()> {
        self.respond(request_seq, command, false, None, Some(message))
    }

    fn emit_event(&mut self, event: &str, body: Value) -> io::Result<()> {
        let event = Event {
            seq: self.next_seq(),
            r#type: "event",
            event,
            body: if body.is_null() { None } else { Some(body) },
        };
        write_dap_message(&mut self.writer, &event)
    }

    fn emit_stop_event(&mut self, event: BackendStopEvent) -> io::Result<()> {
        self.emit_event(
            "stopped",
            json!({
                "reason": event.reason,
                "description": event.description,
                "threadId": event.thread_id
            }),
        )
    }

    fn next_seq(&mut self) -> i64 {
        let current = self.next_seq;
        self.next_seq += 1;
        current
    }
}

#[derive(Serialize)]
struct Response<'a> {
    seq: i64,
    r#type: &'static str,
    request_seq: i64,
    success: bool,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
}

#[derive(Serialize)]
struct Event<'a> {
    seq: i64,
    r#type: &'static str,
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
}

fn read_dap_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut header_line = String::new();

    loop {
        header_line.clear();
        let bytes_read = reader.read_line(&mut header_line)?;
        if bytes_read == 0 {
            if content_length.is_none() {
                return Ok(None);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading headers",
                ));
            }
        }

        let line = header_line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }

        if let Some(rest) = line.strip_prefix("Content-Length:") {
            let len_str = rest.trim();
            let len: usize = len_str.parse().map_err(|err| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid length: {err}"))
            })?;
            content_length = Some(len);
        }
    }

    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Content-Length header missing",
        ));
    };

    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    let payload = String::from_utf8(body)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    Ok(Some(payload))
}

fn write_dap_message<W: Write, T: Serialize>(writer: &mut W, payload: &T) -> io::Result<()> {
    let json = serde_json::to_string(payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let header = format!("Content-Length: {}\r\n\r\n", json.as_bytes().len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

fn parse_arguments<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|err| err.to_string())
}

fn init_backend() -> io::Result<Backend> {
    if let Ok(raw) = env::var(CONFIG_ENV_VAR) {
        if let Some(program) = parse_program_from_config(&raw)? {
            return backend_from_program(&program);
        }
    }
    let exe = env::current_exe()?;
    backend_from_program(&exe)
}

fn backend_from_program(program: &Path) -> io::Result<Backend> {
    Backend::new_from_app(program).map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

fn parse_program_from_config(raw: &str) -> io::Result<Option<PathBuf>> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(value
        .get("program")
        .and_then(Value::as_str)
        .map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{Image, SymbolContext};
    use addr2line::Loader;

    #[derive(Serialize)]
    struct DummyResponse<'a> {
        seq: i64,
        r#type: &'static str,
        request_seq: i64,
        command: &'a str,
        success: bool,
    }

    #[derive(Serialize)]
    struct DummyEvent<'a> {
        seq: i64,
        r#type: &'static str,
        event: &'a str,
    }

    #[test]
    fn write_dap_message_formats_response() {
        let mut buf = Vec::new();
        let payload = DummyResponse {
            seq: 1,
            r#type: "response",
            request_seq: 1,
            command: "initialize",
            success: true,
        };
        write_dap_message(&mut buf, &payload).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("Content-Length:"), "{text}");
        assert!(
            text.contains(r#""type":"response""#),
            "payload missing response type"
        );
        assert!(
            !text.ends_with("\r\n\r\n"),
            "response should not end with framing: {text}"
        );
    }

    #[test]
    fn write_dap_message_formats_event() {
        let mut buf = Vec::new();
        let payload = DummyEvent {
            seq: 2,
            r#type: "event",
            event: "initialized",
        };
        write_dap_message(&mut buf, &payload).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.contains(r#""event":"initialized""#),
            "missing initialized event payload"
        );
        assert!(
            text.contains("\r\n\r\n"),
            "missing separator between headers and payload"
        );
    }

    #[test]
    fn session_handles_initialize_request() {
        let mut session = Session::new(test_backend(), Vec::new());
        let request = RawRequest {
            seq: 1,
            command: "initialize".into(),
            arguments: Value::Null,
        };
        session.handle_request(request).unwrap();
        assert!(session.initialized);
        let output = String::from_utf8(session.writer.clone()).unwrap();
        assert!(
            output.contains(r#""supportsConfigurationDoneRequest":true"#),
            "initialize response missing capabilities: {output}"
        );
        assert!(
            output.contains(r#""event":"initialized""#),
            "initialize should emit initialized event: {output}"
        );
    }

    #[test]
    fn session_handles_unknown_command() {
        let mut session = Session::new(test_backend(), Vec::new());
        let request = RawRequest {
            seq: 1,
            command: "bogus".into(),
            arguments: Value::Null,
        };
        session.handle_request(request).unwrap();
        let output = String::from_utf8(session.writer.clone()).unwrap();
        assert!(
            output.contains(r#""success":false"#),
            "unknown command should report failure"
        );
        assert!(
            output.contains(r#""message":"Unknown command: bogus""#),
            "unknown command should include message"
        );
    }

    fn test_backend() -> Backend {
        let exe = std::env::current_exe().unwrap();
        let loader = Loader::new(&exe).unwrap();
        let image = Image {
            name: "test".into(),
            path: exe.into(),
            uuid: None,
            vmaddr_text: 0,
            slide: 0,
            dwarf: loader,
        };
        let symbol_ctx = SymbolContext::for_testing(image);
        Backend::new_for_testing(symbol_ctx)
    }
}
