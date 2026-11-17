//! Claude Desktop's other route: a manifest-only Desktop Extension
//! (`.mcpb`, a zip holding just `manifest.json`) whose `command` is the
//! absolute path to the connector inside this Scriptorium.app, so nothing
//! is copied and nothing goes stale. Hand test 0 decides whether Desktop
//! accepts a command outside the extension; until then the config file is
//! the route and this is ready beside it (pure: bytes out).
//!
//! Manifest: <https://github.com/modelcontextprotocol/mcpb/blob/main/MANIFEST.md>.

use serde_json::{Value, json};

use crate::{Entry, NAME, Refused, command_str};

/// The file name Connect writes.
pub const FILE_NAME: &str = "Scriptorium.mcpb";

/// The bundle's one other file: the relative `entry_point` the spec asks
/// for. Nothing runs it; `mcp_config.command` is the app's connector.
pub const PLACEHOLDER: &str = "server/placeholder";

const PLACEHOLDER_TEXT: &str =
    "Scriptorium runs from the app: see mcp_config.command in manifest.json. This file is never run.\n";

/// The manifest, as pretty JSON. `version` is Scriptorium's own version.
/// `entry_point` is the relative [`PLACEHOLDER`]; only
/// `mcp_config.command` is absolute (the shape hand test 0 tries).
pub fn manifest(entry: &Entry, version: &str) -> Result<String, Refused> {
    let command = command_str(entry)?;
    let v: Value = json!({
        "manifest_version": "0.3",
        "name": NAME,
        "display_name": "Scriptorium",
        "version": version,
        "description": "Review and translate the screenplay open in Scriptorium.",
        "author": { "name": "Scriptorium" },
        "server": {
            "type": "binary",
            "entry_point": PLACEHOLDER,
            "mcp_config": { "command": command, "args": entry.args, "env": {} }
        },
        "compatibility": { "platforms": ["darwin"] }
    });
    Ok(serde_json::to_string_pretty(&v).expect("a JSON value serializes") + "\n")
}

/// The `.mcpb` file: a zip with `manifest.json` and [`PLACEHOLDER`],
/// stored (uncompressed).
pub fn bytes(entry: &Entry, version: &str) -> Result<Vec<u8>, Refused> {
    let manifest = manifest(entry, version)?;
    Ok(zip(&[("manifest.json", manifest.as_bytes()), (PLACEHOLDER, PLACEHOLDER_TEXT.as_bytes())]))
}

/// A zip archive of stored files, dated 1980-01-01.
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    const DOS_DATE: u16 = (1 << 5) | 1;
    let u32_of = |n: usize| u32::try_from(n).expect("a small archive");
    let header = |out: &mut Vec<u8>, name: &str, data: &[u8]| {
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc32(data).to_le_bytes());
        out.extend_from_slice(&u32_of(data.len()).to_le_bytes());
        out.extend_from_slice(&u32_of(data.len()).to_le_bytes());
        out.extend_from_slice(&u16::try_from(name.len()).expect("a short name").to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
    };
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for (name, data) in files {
        offsets.push(u32_of(out.len()));
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        header(&mut out, name, data);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
    }
    let central_at = u32_of(out.len());
    for ((name, data), offset) in files.iter().zip(offsets) {
        out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        header(&mut out, name, data);
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        out.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    let central_len = u32_of(out.len()) - central_at;
    let count = u16::try_from(files.len()).expect("few files");
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// CRC-32 (IEEE), as zip uses.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "/Users/me/My Apps/Scriptorium.app/Contents/MacOS/scriptorium";

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn manifest_points_into_the_app() {
        let m: Value = serde_json::from_str(&manifest(&Entry::new(APP), "0.6.0").unwrap()).unwrap();
        assert_eq!(m["name"], "scriptorium");
        assert_eq!(m["version"], "0.6.0");
        assert_eq!(m["server"]["type"], "binary");
        assert_eq!(m["server"]["entry_point"], PLACEHOLDER);
        assert_eq!(m["server"]["mcp_config"]["command"], APP);
        assert_eq!(m["server"]["mcp_config"]["args"], json!(["mcp"]));
    }

    /// The archive's entries, read from its central directory: (name,
    /// contents, via the local header).
    fn entries(z: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16_at = |i: usize| u16::from_le_bytes(z[i..i + 2].try_into().unwrap()) as usize;
        let u32_at = |i: usize| u32::from_le_bytes(z[i..i + 4].try_into().unwrap()) as usize;
        let end = z.len() - 22;
        assert_eq!(&z[end..end + 4], b"PK\x05\x06");
        let (count, mut at) = (u16_at(end + 10), u32_at(end + 16));
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(&z[at..at + 4], b"PK\x01\x02");
            let (size, name_len, local) = (u32_at(at + 24), u16_at(at + 28), u32_at(at + 42));
            let name = String::from_utf8(z[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(&z[local..local + 4], b"PK\x03\x04");
            let data_at = local + 30 + u16_at(local + 26);
            let data = z[data_at..data_at + size].to_vec();
            assert_eq!(crc32(&data) as usize, u32_at(at + 16));
            out.push((name, data));
            at += 46 + name_len;
        }
        out
    }

    #[test]
    fn zip_holds_the_manifest_and_the_placeholder() {
        let entry = Entry::new(APP);
        let z = bytes(&entry, "0.6.0").unwrap();
        let e = entries(&z);
        let names: Vec<_> = e.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["manifest.json", PLACEHOLDER]);
        assert_eq!(e[0].1, manifest(&entry, "0.6.0").unwrap().into_bytes());
        assert!(!e[1].1.is_empty());
    }
}
