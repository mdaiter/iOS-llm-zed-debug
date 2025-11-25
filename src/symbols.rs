use std::{
    fs,
    path::{Path, PathBuf},
};

use addr2line::{Frame, Loader, LoaderReader, Location};
use anyhow::{anyhow, Context, Result};
use object::{
    read::{macho, ReadRef},
    BinaryFormat, File as ObjectFile, Object, ObjectSegment,
};

type LoaderFrame<'a> = Frame<'a, LoaderReader<'a>>;

#[allow(dead_code)]
pub struct Image {
    pub name: String,
    pub path: PathBuf,
    pub uuid: Option<[u8; 16]>,
    pub vmaddr_text: u64,
    pub slide: i64,
    pub dwarf: Loader,
}

pub struct SymbolContext {
    pub main: Image,
}

impl SymbolContext {
    pub fn new(app_path: &Path) -> Result<Self> {
        let data = fs::read(app_path)
            .with_context(|| format!("failed to read Mach-O {}", app_path.display()))?;
        let file = ObjectFile::parse(&*data)
            .map_err(|err| anyhow!("failed to parse Mach-O {}: {err}", app_path.display()))?;
        if file.format() != BinaryFormat::MachO {
            return Err(anyhow!(
                "expected Mach-O binary at {}, found {:?}",
                app_path.display(),
                file.format()
            ));
        }

        let vmaddr_text = find_text_vmaddr(&file)?;
        let uuid = extract_macho_uuid(&file)?;
        let dwarf = Loader::new(app_path)
            .map_err(|err| anyhow!("failed to load DWARF from {}: {err}", app_path.display()))?;
        let name = app_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| app_path.display().to_string());

        Ok(Self {
            main: Image {
                name,
                path: app_path.to_path_buf(),
                uuid,
                vmaddr_text,
                slide: 0,
                dwarf,
            },
        })
    }

    #[allow(dead_code)]
    pub fn set_slide(&mut self, slide: i64) {
        self.main.slide = slide;
    }

    pub fn translate_remote_pc(&self, remote_pc: u64) -> u64 {
        if self.main.slide >= 0 {
            remote_pc.wrapping_sub(self.main.slide as u64)
        } else {
            remote_pc.wrapping_add((-self.main.slide) as u64)
        }
    }

    pub fn local_to_remote(&self, local_pc: u64) -> u64 {
        if self.main.slide >= 0 {
            local_pc.wrapping_add(self.main.slide as u64)
        } else {
            local_pc.wrapping_sub((-self.main.slide) as u64)
        }
    }

    #[allow(dead_code)]
    pub fn symbolize_location(&self, remote_pc: u64) -> Result<Option<Location<'_>>> {
        let probe = self.translate_remote_pc(remote_pc);
        self.main
            .dwarf
            .find_location(probe)
            .map_err(|err| anyhow!("addr2line location lookup failed: {err}"))
    }

    pub fn symbolize_frames(&self, remote_pc: u64) -> Result<Vec<LoaderFrame<'_>>> {
        let probe = self.translate_remote_pc(remote_pc);
        let mut frames_iter = self
            .main
            .dwarf
            .find_frames(probe)
            .map_err(|err| anyhow!("addr2line frame lookup failed: {err}"))?;
        let mut frames = Vec::new();
        while let Some(frame) = frames_iter
            .next()
            .map_err(|err| anyhow!("addr2line frame iteration failed: {err}"))?
        {
            frames.push(frame);
        }
        Ok(frames)
    }

    #[cfg(test)]
    pub fn for_testing(main: Image) -> Self {
        Self { main }
    }
}

pub fn find_text_vmaddr(file: &ObjectFile<'_>) -> Result<u64> {
    if file.format() != BinaryFormat::MachO {
        return Err(anyhow!("expected Mach-O format"));
    }

    let mut fallback = None;
    for segment in file.segments() {
        let address = segment.address();
        if fallback.is_none() {
            fallback = Some(address);
        }
        if let Some(name) = segment
            .name()
            .map_err(|err| anyhow!("failed to read segment name: {err}"))?
        {
            if name == "__TEXT" {
                return Ok(address);
            }
        }
    }

    fallback.ok_or_else(|| anyhow!("no segments found"))
}

pub fn extract_macho_uuid(file: &ObjectFile<'_>) -> Result<Option<[u8; 16]>> {
    match file {
        ObjectFile::MachO32(macho) => uuid_from_macho(macho),
        ObjectFile::MachO64(macho) => uuid_from_macho(macho),
        _ => Ok(None),
    }
}

fn uuid_from_macho<'data, Mach, R>(
    macho: &macho::MachOFile<'data, Mach, R>,
) -> Result<Option<[u8; 16]>>
where
    Mach: macho::MachHeader,
    R: ReadRef<'data>,
{
    let mut commands = macho
        .macho_load_commands()
        .map_err(|err| anyhow!("failed to read Mach-O load commands: {err}"))?;
    while let Some(command) = commands
        .next()
        .map_err(|err| anyhow!("failed to iterate load commands: {err}"))?
    {
        if let Some(uuid) = command
            .uuid()
            .map_err(|err| anyhow!("failed to parse UUID command: {err}"))?
        {
            return Ok(Some(uuid.uuid));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::read::File;

    const TEST_UUID: [u8; 16] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x11, 0x12, 0x13, 0x14, 0x21, 0x22, 0x23,
        0x24,
    ];

    #[test]
    fn text_vmaddr_detected() {
        let macho = build_test_macho(0x1000, TEST_UUID);
        let file = File::parse(&*macho).unwrap();
        let vmaddr = find_text_vmaddr(&file).unwrap();
        assert_eq!(vmaddr, 0x1000);
    }

    #[test]
    fn uuid_is_extracted() {
        let macho = build_test_macho(0x2000, TEST_UUID);
        let file = File::parse(&*macho).unwrap();
        let uuid = extract_macho_uuid(&file).unwrap().unwrap();
        assert_eq!(uuid, TEST_UUID);
    }

    #[test]
    fn translate_remote_pc_applies_slide() {
        let Some(dummy_loader) = test_loader() else {
            eprintln!("skipping translate_remote_pc_applies_slide: missing DWARF loader");
            return;
        };
        let mut ctx = SymbolContext::for_testing(Image {
            name: "test".into(),
            path: PathBuf::from("/tmp/test"),
            uuid: None,
            vmaddr_text: 0x1000,
            slide: 0,
            dwarf: dummy_loader,
        });
        ctx.set_slide(0x4000);
        let translated = ctx.translate_remote_pc(0x9000);
        assert_eq!(translated, 0x5000);
    }

    #[test]
    fn local_to_remote_applies_slide() {
        let Some(dummy_loader) = test_loader() else {
            eprintln!("skipping local_to_remote_applies_slide: missing DWARF loader");
            return;
        };
        let mut ctx = SymbolContext::for_testing(Image {
            name: "test".into(),
            path: PathBuf::from("/tmp/test"),
            uuid: None,
            vmaddr_text: 0x0,
            slide: 0x2000,
            dwarf: dummy_loader,
        });
        let remote = ctx.local_to_remote(0x1000);
        assert_eq!(remote, 0x3000);
        ctx.set_slide(-0x2000);
        let remote = ctx.local_to_remote(0x3000);
        assert_eq!(remote, 0x1000);
    }

    #[test]
    fn symbolize_frames_handles_missing_or_real_debug_info() {
        let exe = std::env::current_exe().unwrap();
        let ctx = match SymbolContext::new(&exe) {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("skipping symbolize_frames_handles_missing_or_real_debug_info: {err}");
                return;
            }
        };
        let frames = ctx.symbolize_frames(ctx.main.vmaddr_text);
        assert!(
            frames.is_ok(),
            "symbolize_frames should not panic even without DWARF"
        );
    }

    fn build_test_macho(vmaddr: u64, uuid: [u8; 16]) -> Vec<u8> {
        let mut commands = Vec::new();
        commands.push(build_segment_command(vmaddr));
        commands.push(build_uuid_command(uuid));
        build_header(&commands)
    }

    fn build_header(commands: &[Vec<u8>]) -> Vec<u8> {
        let sizeofcmds: u32 = commands.iter().map(|c| c.len() as u32).sum();
        let ncmds = commands.len() as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&0xfeedfacfu32.to_le_bytes());
        buf.extend_from_slice(&0x0100000cu32.to_le_bytes()); // CPU_TYPE_ARM64
        buf.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        buf.extend_from_slice(&0x2u32.to_le_bytes()); // filetype MH_EXECUTE
        buf.extend_from_slice(&ncmds.to_le_bytes());
        buf.extend_from_slice(&sizeofcmds.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags
        buf.extend_from_slice(&0u32.to_le_bytes()); // reserved
        for cmd in commands {
            buf.extend_from_slice(cmd);
        }
        while buf.len() % 8 != 0 {
            buf.push(0);
        }
        buf
    }

    fn build_segment_command(vmaddr: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        let cmdsize = 72u32;
        buf.extend_from_slice(&0x19u32.to_le_bytes()); // LC_SEGMENT_64
        buf.extend_from_slice(&cmdsize.to_le_bytes());
        let mut segname = [0u8; 16];
        segname[..6].copy_from_slice(b"__TEXT");
        buf.extend_from_slice(&segname);
        buf.extend_from_slice(&vmaddr.to_le_bytes());
        buf.extend_from_slice(&0x1000u64.to_le_bytes()); // vmsize
        buf.extend_from_slice(&0u64.to_le_bytes()); // fileoff
        buf.extend_from_slice(&0x1000u64.to_le_bytes()); // filesize
        buf.extend_from_slice(&7u32.to_le_bytes()); // maxprot
        buf.extend_from_slice(&5u32.to_le_bytes()); // initprot
        buf.extend_from_slice(&0u32.to_le_bytes()); // nsects
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags
        buf
    }

    fn build_uuid_command(uuid: [u8; 16]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x1bu32.to_le_bytes()); // LC_UUID
        buf.extend_from_slice(&24u32.to_le_bytes());
        buf.extend_from_slice(&uuid);
        buf
    }

    fn test_loader() -> Option<Loader> {
        let exe = std::env::current_exe().ok()?;
        match Loader::new(&exe) {
            Ok(loader) => Some(loader),
            Err(err) => {
                eprintln!("test_loader: unable to create loader for {:?}: {err}", exe);
                None
            }
        }
    }
}
