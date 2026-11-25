#![cfg(feature = "automation")]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const CLI: &str = env!("CARGO_BIN_EXE_disc-xplorer-cli");
const SECTOR: usize = 2048;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("dx-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn image(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, data).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn both32(bytes: &mut [u8], at: usize, number: u32) {
    bytes[at..at + 4].copy_from_slice(&number.to_le_bytes());
    bytes[at + 4..at + 8].copy_from_slice(&number.to_be_bytes());
}

fn record(name: &[u8], lba: u32, size: u32, flags: u8) -> Vec<u8> {
    let mut bytes = vec![0; (33 + name.len()).next_multiple_of(2)];
    bytes[0] = bytes.len() as u8;
    both32(&mut bytes, 2, lba);
    both32(&mut bytes, 10, size);
    bytes[18..25].copy_from_slice(&[98, 10, 20, 12, 23, 56, 0]);
    bytes[25] = flags;
    bytes[28] = 1;
    bytes[31] = 1;
    bytes[32] = name.len() as u8;
    bytes[33..33 + name.len()].copy_from_slice(name);
    bytes
}

fn iso(value: u8) -> Vec<u8> {
    let mut bytes = include_bytes!("../crates/iso9660/test.iso").to_vec();
    bytes.resize(48 * SECTOR, 0);
    both32(&mut bytes, 16 * SECTOR + 80, 48);
    both32(&mut bytes, 16 * SECTOR + 158, 24);
    both32(&mut bytes, 16 * SECTOR + 166, SECTOR as u32);
    bytes[24 * SECTOR..25 * SECTOR].fill(0);
    let mut at = 24 * SECTOR;
    for entry in [
        record(&[0], 24, SECTOR as u32, 2),
        record(&[1], 24, SECTOR as u32, 2),
        record(b"A.BIN;1", 32, 3, 0),
        record(b"EMPTY;1", 0, 0, 0),
    ] {
        bytes[at..at + entry.len()].copy_from_slice(&entry);
        at += entry.len();
    }
    bytes[32 * SECTOR..32 * SECTOR + 3].fill(value);
    bytes
}

fn cli(command: &str, paths: &[&Path], arguments: &[&str]) -> Output {
    Command::new(CLI)
        .arg(command)
        .args(paths)
        .args(arguments)
        .output()
        .unwrap()
}

fn response(output: Output, succeeded: bool) -> Value {
    assert_eq!(output.status.success(), succeeded, "{output:?}");
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["api_version"], 1);
    data
}

#[test]
fn cli_inspects_paginates_and_verifies_files_with_equal_sizes() {
    let fixture = Fixture::new();
    let a = fixture.image("a.iso", &iso(b'a'));
    let b = fixture.image("b.iso", &iso(b'b'));

    let info = response(cli("inspect-image", &[&a], &[]), true);
    assert!(info["result"]["disc_info"]["filesystems"]
        .as_array()
        .unwrap()
        .contains(&json!("ISO 9660")));
    let first = response(cli("list-entries", &[&a], &["--limit", "1"]), true);
    assert_eq!(first["result"]["entries"][0]["name"], "A.BIN");
    assert_eq!(first["result"]["next_offset"], 1);
    let next = response(
        cli("list-entries", &[&a], &["--limit", "1", "--offset", "1"]),
        true,
    );
    assert_eq!(next["result"]["entries"][0]["name"], "EMPTY");
    assert!(next["result"]["next_offset"].is_null());

    let dates = response(cli("disc-dates", &[&a], &[]), true);
    assert_eq!(
        dates["result"]["report"]["latest_file_date"],
        "1998-10-20 12:23:56"
    );
    assert_eq!(dates["result"]["complete"], true);
    let hashes = response(cli("file-hashes", &[&a, Path::new("/A.BIN")], &[]), true);
    assert_eq!(hashes["result"]["hashes"]["bytes"], 3);
    assert_eq!(
        hashes["result"]["hashes"]["sha256"],
        "9834876dcfb05cb167a5c24953eba58c4ac89b1adf57f28f2f9d09af107ee8f0"
    );
    let budget_error = response(
        cli(
            "file-hashes",
            &[&a, Path::new("/A.BIN")],
            &["--max-bytes", "2"],
        ),
        false,
    );
    assert_eq!(budget_error["error"]["code"], "limit_exceeded");
    let location = response(cli("file-location", &[&a, Path::new("/A.BIN")], &[]), true);
    assert_eq!(location["result"]["location"]["ranges"][0]["start_lba"], 32);

    let same_metadata = response(cli("compare-directories", &[&a, &b], &[]), true);
    assert_eq!(
        same_metadata["result"]["entries"][0]["status"],
        "same_metadata"
    );
    let compared = response(
        cli("compare-directories", &[&a, &b], &["--verify-contents"]),
        true,
    );
    assert_eq!(
        compared["result"]["entries"][0]["status"],
        "different_contents"
    );
    assert_eq!(compared["result"]["entries"][1]["status"], "same_contents");
    let partial = response(
        cli(
            "compare-directories",
            &[&a, &b],
            &["--verify-contents", "--max-bytes", "5"],
        ),
        true,
    );
    assert_eq!(partial["result"]["entries"][0]["status"], "not_verified");
}

#[test]
fn cli_reports_scan_continuation_and_both_directions_of_a_length_mismatch() {
    let fixture = Fixture::new();
    let a = fixture.image("a.iso", &iso(b'a'));
    let b = fixture.image("b.iso", &iso(b'b'));
    let sector = response(
        cli("read-sector", &[&a, Path::new("32")], &["--length", "3"]),
        true,
    );
    assert_eq!(sector["result"]["hex"], "616161");

    let page = response(
        cli("find-sector-difference", &[&a, &b], &["--limit", "32"]),
        true,
    );
    assert!(page["result"]["first_difference_lba"].is_null());
    assert_eq!(page["result"]["next_lba"], 32);
    let difference = response(
        cli("find-sector-difference", &[&a, &b], &["--from-lba", "32"]),
        true,
    );
    assert_eq!(difference["result"]["first_difference_lba"], 32);

    let mut bytes = iso(b'a');
    bytes.extend_from_slice(&[0; SECTOR]);
    let longer = fixture.image("longer.iso", &bytes);
    for (left, right, kind) in [(&a, &longer, "only_in_b"), (&longer, &a, "only_in_a")] {
        let result = response(cli("find-sector-difference", &[left, right], &[]), true);
        assert_eq!(result["result"]["first_difference_lba"], 48);
        assert_eq!(result["result"]["difference_kind"], kind);
    }
}

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<String>,
}

impl McpProcess {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(CLI)
            .args(["mcp", "--root"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            replies,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let line = self.replies.recv_timeout(Duration::from_secs(10)).unwrap();
        let reply: Value = serde_json::from_str(&line).expect(&line);
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_discovers_read_only_tools_and_enforces_roots_and_cue_companions() {
    let root = Fixture::new();
    let outside = Fixture::new();
    let allowed = root.image("inside.iso", &iso(b'a'));
    let denied = outside.image("outside.iso", &iso(b'b'));
    let mut server = McpProcess::start(&root.0);
    let handshake = server.call(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "automation-test", "version": "1" }
        }),
    );
    assert_eq!(handshake["result"]["serverInfo"]["name"], "disc-xplorer");
    server.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let reply = server.call(2, "tools/list", json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    assert!(tools
        .iter()
        .all(|tool| tool["annotations"]["readOnlyHint"] == true));
    let response = server.call(
        3,
        "tools/call",
        json!({
            "name": "inspect_image", "arguments": { "image": allowed }
        }),
    );
    assert_eq!(response["result"]["structuredContent"]["api_version"], 1);
    assert_eq!(
        response["result"]["structuredContent"]["result"]["format"],
        "iso"
    );
    let response = server.call(
        4,
        "tools/call",
        json!({
            "name": "inspect_image", "arguments": { "image": denied }
        }),
    );
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "outside_root"
    );

    let cue = root.image(
        "outside.cue",
        format!(
            "FILE \"{}\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n",
            denied.display()
        )
        .as_bytes(),
    );
    let response = server.call(
        5,
        "tools/call",
        json!({
            "name": "inspect_image", "arguments": { "image": cue }
        }),
    );
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "unsupported_layout"
    );

    let missing = server.call(
        8,
        "tools/call",
        json!({
            "name": "inspect_image", "arguments": { "image": outside.0.join("missing.iso") }
        }),
    );
    assert_eq!(
        missing["result"]["structuredContent"]["error"]["code"],
        "outside_root"
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&denied, root.0.join("alias.bin")).unwrap();
        let cue = root.image(
            "alias.cue",
            b"FILE \"alias.bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n",
        );
        let result = server.call(
            6,
            "tools/call",
            json!({
                "name": "inspect_image", "arguments": { "image": cue }
            }),
        );
        assert_eq!(
            result["result"]["structuredContent"]["error"]["code"],
            "outside_root"
        );

        std::os::unix::fs::symlink(&denied, allowed.with_extension("sbi")).unwrap();
        let result = server.call(
            7,
            "tools/call",
            json!({
                "name": "read_subchannels", "arguments": { "image": allowed, "lba": 0 }
            }),
        );
        assert_eq!(
            result["result"]["structuredContent"]["error"]["code"],
            "outside_root"
        );
    }
}
