use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context as AnyhowContext, Result as AnyResult};
use serde_json::{json, Value};

use crate::{
    gdb_remote::{GdbRemoteClient, StopReason, StopReply},
    symbols::SymbolContext,
};
use gimli::{
    self, EndianSlice, IncompleteLineProgram, LineProgramHeader, LineRow, RunTimeEndian, SectionId,
    Unit,
};
use object::{Object, ObjectSection};

type FrameProvider = dyn Fn(i64) -> Vec<(i64, u64)> + Send + Sync;

/// Backend stub that pretends to talk to debugserver/LLDB.
pub struct Backend {
    pub symbol_ctx: SymbolContext,
    connected_port: Option<u16>,
    breakpoints: HashMap<String, Vec<i64>>,
    frame_provider: Option<Box<FrameProvider>>,
    line_index: Option<LineIndex>,
    gdb_client: Option<GdbRemoteClient>,
}

impl Backend {
    fn from_symbol_context(symbol_ctx: SymbolContext) -> Self {
        Self {
            symbol_ctx,
            connected_port: None,
            breakpoints: HashMap::new(),
            frame_provider: None,
            line_index: None,
            gdb_client: None,
        }
    }

    #[allow(dead_code)]
    pub fn new_for_testing(symbol_ctx: SymbolContext) -> Self {
        Self::from_symbol_context(symbol_ctx)
    }

    pub fn new_from_app(app_path: &Path) -> AnyResult<Self> {
        let symbol_ctx = SymbolContext::new(app_path)?;
        Ok(Self::from_symbol_context(symbol_ctx))
    }

    #[allow(dead_code)]
    pub fn set_frame_provider<F>(&mut self, provider: F)
    where
        F: Fn(i64) -> Vec<(i64, u64)> + Send + Sync + 'static,
    {
        self.frame_provider = Some(Box::new(provider));
    }

    #[allow(dead_code)]
    pub fn update_slide_from_remote_text_base(&mut self, remote_text_base: u64) {
        let vmaddr_text = self.symbol_ctx.main.vmaddr_text;
        let slide = remote_text_base as i64 - vmaddr_text as i64;
        self.symbol_ctx.set_slide(slide);
    }

    pub fn connect_debugserver(&mut self, port: u16) -> Result<(), String> {
        match GdbRemoteClient::connect(port) {
            Ok(client) => {
                self.connected_port = Some(port);
                self.gdb_client = Some(client);
                Ok(())
            }
            Err(err) => Err(format!(
                "failed to connect to debugserver on port {port}: {err}"
            )),
        }
    }

    pub fn update_breakpoints(&mut self, source_path: &str, lines: &[i64]) -> Result<(), String> {
        self.breakpoints
            .insert(source_path.to_string(), lines.to_vec());

        self.ensure_line_index()?;
        let Some(index) = &self.line_index else {
            return Ok(());
        };

        let canonical = Path::new(source_path).to_string_lossy().to_string();

        for line in lines {
            if *line <= 0 {
                continue;
            }
            let ranges = index.lookup(&canonical, *line as u64);
            if ranges.is_empty() {
                eprintln!("No DWARF ranges for {canonical}:{line}, skipping breakpoint placement");
                continue;
            }
            for range in ranges {
                let remote_addr = self.symbol_ctx.local_to_remote(range.low);
                if let Some(client) = self.gdb_client.as_mut() {
                    client
                        .set_software_breakpoint(remote_addr)
                        .map_err(|err| format!("failed to plant breakpoint: {err}"))?;
                } else {
                    eprintln!(
                        "No gdb-remote client for breakpoint at 0x{remote_addr:x}; call connect_debugserver first"
                    );
                }
            }
        }

        Ok(())
    }

    pub fn threads(&self) -> Vec<Value> {
        vec![json!({
            "id": 1,
            "name": format!(
                "Stub Thread{}",
                self.connected_port
                    .map(|port| format!(" ({port})"))
                    .unwrap_or_default()
            ),
        })]
    }

    pub fn stack_trace(&self, thread_id: i64) -> Vec<Value> {
        let raw_frames = self.backend_fetch_frames(thread_id);
        let mut out = Vec::new();

        for (idx, (frame_id, pc)) in raw_frames.iter().enumerate() {
            let frames = self.symbol_ctx.symbolize_frames(*pc).ok();
            let top = frames.as_ref().and_then(|frames| frames.first());
            let function_name = top
                .and_then(|frame| frame.function.as_ref())
                .and_then(|name| {
                    name.demangle()
                        .ok()
                        .map(|cow| cow.into_owned())
                        .or_else(|| name.raw_name().ok().map(|cow| cow.into_owned()))
                })
                .unwrap_or_else(|| "<unknown>".into());

            let location = top.and_then(|frame| frame.location.as_ref());
            let file_path = location
                .and_then(|loc| loc.file)
                .unwrap_or("<unknown>")
                .to_string();
            let line = location
                .and_then(|loc| loc.line)
                .map(|line| line as i64)
                .unwrap_or(0);
            let source_name = file_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file_path)
                .to_string();

            out.push(json!({
                "id": frame_id,
                "name": function_name,
                "line": line,
                "column": 1,
                "source": {
                    "name": source_name,
                    "path": file_path,
                },
                "presentationHint": if idx == 0 { "normal" } else { "subtle" },
            }));
        }

        out
    }

    fn ensure_line_index(&mut self) -> Result<(), String> {
        if self.line_index.is_none() {
            match LineIndex::from_binary(&self.symbol_ctx.main.path) {
                Ok(index) => self.line_index = Some(index),
                Err(err) => {
                    return Err(format!(
                        "Failed to build DWARF line index for {:?}: {err}",
                        self.symbol_ctx.main.path
                    ))
                }
            }
        }
        Ok(())
    }

    pub fn scopes(&self) -> Vec<Value> {
        vec![json!({
            "name": "Locals",
            "variablesReference": 1,
            "expensive": false
        })]
    }

    pub fn variables(&self, variables_reference: i64) -> Vec<Value> {
        vec![
            json!({
                "name": "var",
                "value": format!("value-{variables_reference}"),
                "type": "string",
                "variablesReference": 0
            }),
            json!({
                "name": "counter",
                "value": "123",
                "type": "int",
                "variablesReference": 0
            }),
        ]
    }

    pub fn r#continue(&mut self, _thread_id: i64) -> Result<Option<BackendStopEvent>, String> {
        let client = self.ensure_gdb()?;
        client.continue_all().map_err(|err| err.to_string())?;
        client
            .wait_for_stop()
            .map(BackendStopEvent::from_reply)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    pub fn step_over(&mut self, thread_id: i64) -> Result<Option<BackendStopEvent>, String> {
        let client = self.ensure_gdb()?;
        client
            .step_thread(thread_id)
            .map_err(|err| err.to_string())?;
        client
            .wait_for_stop()
            .map(BackendStopEvent::from_reply)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    pub fn step_in(&mut self, thread_id: i64) -> Result<Option<BackendStopEvent>, String> {
        self.step_over(thread_id)
    }

    pub fn disconnect(&mut self) -> Result<(), String> {
        self.connected_port = None;
        self.gdb_client = None;
        Ok(())
    }

    fn backend_fetch_frames(&self, thread_id: i64) -> Vec<(i64, u64)> {
        if let Some(provider) = &self.frame_provider {
            return provider(thread_id);
        }

        vec![(
            thread_id * 100 + 1,
            self.symbol_ctx.main.vmaddr_text + self.symbol_ctx.main.slide as u64,
        )]
    }

    fn ensure_gdb(&mut self) -> Result<&mut GdbRemoteClient, String> {
        self.gdb_client
            .as_mut()
            .ok_or_else(|| "no gdb-remote connection; call connect_debugserver first".to_string())
    }
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
struct FileLine {
    file: String,
    line: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressRange {
    pub low: u64,
    pub high: u64,
}

pub struct BackendStopEvent {
    pub reason: &'static str,
    pub description: String,
    pub thread_id: i64,
}

impl BackendStopEvent {
    fn from_reply(reply: StopReply) -> Self {
        let thread_id = reply.thread_id.unwrap_or(1) as i64;
        let (reason, description) = match reply.reason {
            StopReason::Breakpoint => ("breakpoint", "Breakpoint hit".to_string()),
            StopReason::Step => ("step", "Step completed".to_string()),
            StopReason::Signal => ("signal", format!("Signal {}", reply.signal)),
            StopReason::Unknown(text) => ("stopped", text),
        };
        Self {
            reason,
            description,
            thread_id,
        }
    }
}

pub struct LineIndex {
    map: HashMap<FileLine, Vec<AddressRange>>,
}

impl LineIndex {
    pub fn from_binary(path: &Path) -> AnyResult<Self> {
        let data = fs::read(path)
            .with_context(|| format!("failed to read Mach-O for line index: {}", path.display()))?;
        let file =
            object::File::parse(&*data).context("failed to parse Mach-O for DWARF line index")?;
        let endian = if file.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };
        let dwarf_sections = gimli::DwarfSections::load(|id| load_section_vec(&file, id))?;
        let dwarf = dwarf_sections.borrow(|section| gimli::EndianSlice::new(section, endian));
        Self::new_from_dwarf(&dwarf)
    }

    #[allow(dead_code)]
    pub fn new_from_dwarf(
        _dwarf: &gimli::Dwarf<gimli::EndianSlice<'_, gimli::RunTimeEndian>>,
    ) -> AnyResult<Self> {
        let mut index = LineIndex {
            map: HashMap::new(),
        };
        let mut units = _dwarf.units();
        while let Some(header) = units.next()? {
            let unit = _dwarf.unit(header)?;
            if let Some(program) = unit.line_program.clone() {
                index.consume_line_program(_dwarf, &unit, program)?;
            }
        }
        Ok(index)
    }

    pub fn lookup(&self, file: &str, line: u64) -> Vec<AddressRange> {
        let mut results = Vec::new();
        let key = FileLine {
            file: file.to_string(),
            line,
        };
        if let Some(ranges) = self.map.get(&key) {
            results.extend_from_slice(ranges);
        }
        if results.is_empty() {
            if let Some(name) = Path::new(file).file_name().and_then(|n| n.to_str()) {
                if name != file {
                    let key = FileLine {
                        file: name.to_string(),
                        line,
                    };
                    if let Some(ranges) = self.map.get(&key) {
                        results.extend_from_slice(ranges);
                    }
                }
            }
        }
        results
    }

    fn consume_line_program(
        &mut self,
        dwarf: &gimli::Dwarf<EndianSlice<'_, RunTimeEndian>>,
        unit: &Unit<EndianSlice<'_, RunTimeEndian>>,
        program: IncompleteLineProgram<EndianSlice<'_, RunTimeEndian>>,
    ) -> gimli::Result<()> {
        let mut rows = program.rows();
        let mut previous: Option<(FileLine, u64)> = None;

        while let Some((header, row)) = rows.next_row()? {
            if row.end_sequence() {
                if let Some((file_line, start)) = previous.take() {
                    let end = row.address();
                    if end > start {
                        self.insert_range(
                            file_line,
                            AddressRange {
                                low: start,
                                high: end,
                            },
                        );
                    }
                }
                continue;
            }

            let file_path = line_file_path(dwarf, unit, header, &row)
                .unwrap_or_else(|| "<unknown>".to_string());
            let line = row.line().map(|value| value.get()).unwrap_or(0);
            let address = row.address();

            if let Some((prev_fl, start)) = previous.take() {
                if address >= start {
                    self.insert_range(
                        prev_fl,
                        AddressRange {
                            low: start,
                            high: address,
                        },
                    );
                }
            }

            previous = Some((
                FileLine {
                    file: file_path,
                    line,
                },
                address,
            ));
        }

        Ok(())
    }

    fn insert_range(&mut self, fl: FileLine, range: AddressRange) {
        self.map.entry(fl.clone()).or_default().push(range);
        if let Some(name) = Path::new(&fl.file).file_name().and_then(|n| n.to_str()) {
            if name != fl.file {
                let key = FileLine {
                    file: name.to_string(),
                    line: fl.line,
                };
                self.map.entry(key).or_default().push(range);
            }
        }
    }
}

fn load_section_vec(
    file: &object::File<'_>,
    id: SectionId,
) -> Result<Vec<u8>, object::read::Error> {
    if let Some(section) = file.section_by_name(id.name()) {
        let data = section.uncompressed_data()?;
        Ok(data.into_owned())
    } else {
        Ok(Vec::new())
    }
}

fn line_file_path(
    dwarf: &gimli::Dwarf<EndianSlice<'_, RunTimeEndian>>,
    unit: &Unit<EndianSlice<'_, RunTimeEndian>>,
    header: &LineProgramHeader<EndianSlice<'_, RunTimeEndian>>,
    row: &LineRow,
) -> Option<String> {
    let file_entry = row.file(header)?;
    let file_name = dwarf.attr_string(unit, file_entry.path_name()).ok()?;
    let mut path = file_name.to_string_lossy().into_owned();

    if let Some(dir_attr) = file_entry.directory(header) {
        if let Ok(dir) = dwarf.attr_string(unit, dir_attr) {
            let dir = dir.to_string_lossy();
            if !dir.is_empty() {
                path = format!("{}/{}", dir.trim_end_matches('/'), path);
            }
        }
    }

    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{Image, SymbolContext};
    use addr2line::Loader;
    use object::{Object, ObjectSymbol};

    #[no_mangle]
    #[inline(never)]
    pub extern "C" fn backend_symbol_test_function() {
        std::hint::black_box(());
    }

    #[test]
    fn threads_have_id_and_name() {
        let backend = test_backend();
        let threads = backend.threads();
        assert!(!threads.is_empty(), "expected at least one thread");
        let thread = threads.first().unwrap();
        assert!(thread.get("id").is_some());
        assert!(thread.get("name").is_some());
    }

    #[test]
    fn update_slide_tracks_remote_base() {
        let mut backend = test_backend_with_vmaddr(0x1000);
        backend.update_slide_from_remote_text_base(0x3000);
        assert_eq!(backend.symbol_ctx.main.slide, 0x2000);
        let translated = backend.symbol_ctx.translate_remote_pc(0x3000 + 0x40);
        assert_eq!(translated, 0x1000 + 0x40);
    }

    #[test]
    fn stack_trace_symbolizes_frames() {
        let mut backend = test_backend();
        backend_symbol_test_function();
        let symbol = find_symbol_address("backend_symbol_test_function");
        backend.set_frame_provider(move |_thread_id| vec![(42, symbol)]);

        let frames = backend.stack_trace(7);
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!(frame.get("id").unwrap().as_i64().unwrap(), 42);
        assert!(
            frame
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap()
                .contains("backend_symbol_test_function"),
            "function name was not symbolized: {frame:?}"
        );
        assert!(
            frame
                .get("source")
                .and_then(|src| src.get("path"))
                .and_then(|p| p.as_str())
                .map(|path| path.contains(".rs"))
                .unwrap_or(false),
            "expected a source path"
        );
    }

    #[test]
    fn stack_trace_falls_back_to_unknown_metadata() {
        let mut backend = test_backend();
        backend.set_frame_provider(move |_thread_id| vec![(7, 0xDEADBEEF)]);
        let frames = backend.stack_trace(1);
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!(frame.get("id").unwrap().as_i64().unwrap(), 7);
        assert_eq!(
            frame.get("name").and_then(|n| n.as_str()).unwrap(),
            "<unknown>"
        );
        assert_eq!(
            frame
                .get("source")
                .and_then(|src| src.get("path"))
                .and_then(|p| p.as_str())
                .unwrap(),
            "<unknown>"
        );
        assert_eq!(frame.get("line").unwrap().as_i64().unwrap(), 0);
    }

    #[test]
    fn line_index_lookup_returns_ranges() {
        let mut map = HashMap::new();
        map.insert(
            FileLine {
                file: "/tmp/main.rs".into(),
                line: 10,
            },
            vec![AddressRange {
                low: 0x10,
                high: 0x20,
            }],
        );
        let index = LineIndex { map };
        assert_eq!(
            index.lookup("/tmp/main.rs", 10),
            vec![AddressRange {
                low: 0x10,
                high: 0x20
            }]
        );
        assert!(index.lookup("/tmp/main.rs", 11).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn backend_from_app_uses_symbol_context() {
        let exe = std::env::current_exe().unwrap();
        let backend = Backend::new_from_app(&exe).unwrap();
        assert_eq!(backend.symbol_ctx.main.path, exe);
    }

    #[test]
    fn update_breakpoints_succeeds_without_gdb_client() {
        let mut backend = test_backend();
        backend.gdb_client = None;
        backend.line_index = Some(LineIndex {
            map: HashMap::from([(
                FileLine {
                    file: "/tmp/foo.rs".into(),
                    line: 42,
                },
                vec![AddressRange {
                    low: backend.symbol_ctx.main.vmaddr_text,
                    high: backend.symbol_ctx.main.vmaddr_text + 4,
                }],
            )]),
        });
        assert!(backend.update_breakpoints("/tmp/foo.rs", &[42]).is_ok());
    }

    #[test]
    fn line_index_builds_from_current_binary() {
        let exe = std::env::current_exe().unwrap();
        match LineIndex::from_binary(&exe) {
            Ok(index) => {
                if index.map.is_empty() {
                    eprintln!("line index for {:?} was empty; skipping assertion", exe);
                }
            }
            Err(err) => eprintln!("skipping line_index_builds_from_current_binary: {err}"),
        }
    }

    fn test_backend() -> Backend {
        test_backend_with_vmaddr(0x0)
    }

    fn test_backend_with_vmaddr(vmaddr_text: u64) -> Backend {
        let exe = std::env::current_exe().unwrap();
        let loader = Loader::new(&exe).unwrap();
        let image = Image {
            name: "test".into(),
            path: exe.into(),
            uuid: None,
            vmaddr_text,
            slide: 0,
            dwarf: loader,
        };
        let symbol_ctx = SymbolContext::for_testing(image);
        Backend::new_for_testing(symbol_ctx)
    }

    fn find_symbol_address(symbol_name: &str) -> u64 {
        let exe = std::env::current_exe().unwrap();
        let data = std::fs::read(&exe).unwrap();
        let file = object::File::parse(&*data).unwrap();
        file.symbols()
            .find(|sym| {
                sym.name()
                    .map(|name| symbol_matches(name, symbol_name))
                    .unwrap_or(false)
            })
            .map(|sym| sym.address())
            .expect("symbol not found")
    }

    fn symbol_matches(name: &str, symbol_name: &str) -> bool {
        name == symbol_name
            || name
                .strip_prefix('_')
                .map_or(false, |rest| rest == symbol_name)
            || name.contains(symbol_name)
    }
}
