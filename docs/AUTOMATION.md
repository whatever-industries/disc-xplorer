# Disc Xplorer automation

The JSON CLI and local Model Context Protocol (MCP) server use the same Rust readers as Disc Xplorer. They run headlessly and read existing disc images.

## Build from source

From the repository root, with the prerequisites for the desktop app installed:

```sh
npm install
npm run build
cd src-tauri
cargo build --release --target-dir target --features automation --bin disc-xplorer-cli
```

With CMake 4, set `CMAKE_POLICY_VERSION_MINIMUM=3.5` in the build environment for the bundled FLAC library.

The executable is `src-tauri/target/release/disc-xplorer-cli` relative to the repository root. On Windows it ends in `.exe`. This is a separate build from the desktop installer.

## Command line

```sh
disc-xplorer-cli inspect-image "/images/Prototype A.cue"
disc-xplorer-cli list-entries "/images/Prototype A.cue" --directory / --limit 25
disc-xplorer-cli disc-dates "/images/Prototype A.cue"
disc-xplorer-cli file-hashes "/images/Prototype A.cue" /GAME.BIN
disc-xplorer-cli file-location "/images/Prototype A.cue" /GAME.BIN
disc-xplorer-cli compare-directories "/images/Prototype A.cue" "/images/Prototype B.cue" --verify-contents
disc-xplorer-cli find-sector-difference "/images/Prototype A.cue" "/images/Prototype B.cue" --limit 4096
disc-xplorer-cli read-sector "/images/Prototype A.cue" 16 --offset 24 --length 64
disc-xplorer-cli read-subchannels "/images/Prototype A.cue" 16
```

Use `disc-xplorer-cli --help` or append `--help` to a command for its arguments. Inspection commands return one JSON object with `api_version` and either `result` or `error`. An operation error includes a machine-readable `code` and exits with a nonzero status. Command-line syntax errors are reported on standard error.

## Connect an MCP client

Configure a local stdio server. Replace the executable and root paths with absolute paths on your machine.

```json
{
  "mcpServers": {
    "disc-xplorer": {
      "command": "/absolute/path/to/disc-xplorer-cli",
      "args": ["mcp", "--root", "/absolute/path/to/disc-images"]
    }
  }
}
```

Repeat `--root` for additional directories. Each image argument must be absolute and resolve to a regular file in one of those roots. CUE references and discovered companion files are also checked. Use directories you control. This path filtering runs inside the program and assumes the filesystem stays stable during an inspection.

MCP uses the underscore equivalents of CLI command names. For example, call `compare_directories` with `image_a`, `image_b`, and `verify_contents: true`. Results use the same JSON shape as the CLI and are available in the MCP structured result. The server runs over stdio without a network listener. Your MCP client determines which results it passes to a model.

## Interpreting results

- **Directory listings and comparisons** operate one directory at a time. Use `next_offset` as the next `offset` and list subdirectories separately. Select a specific filesystem by name where an image has more than one.
- **File comparisons** report metadata matches by default. Use `--verify-contents` for SHA-256 comparisons of same-sized files. A `not_verified` entry means a read failed or the page exceeded its hash budget. The default budget is 128 MiB.
- **Sector comparisons** compare raw sector bytes at corresponding LBAs, independent of filesystem metadata. Follow `next_lba` to continue a bounded scan. A sector present in only one image is reported as a difference. CUE and NRG sector addresses are relative to the first data track.
- **Dates** retain on-disc calendar times and PVD offsets where recorded. A result with `complete: false` indicates that a directory could not be read or the scan reached its entry limit.
- **Formats** include ISO, IMG, BIN, CUE/BIN, CSO/CISO, ZSO, GCZ, CHD, NRG, WBFS, RVZ/WIA, UIF, uncompressed skeleton images, ZIP, CAB, and plain TAR. CUE sheets use quoted BIN filenames from their own directory. Supported filesystem, sector, and file-location operations vary by format. Formats the desktop app decodes entirely into memory, such as ECM and Zstandard disc images, are outside this first automation release.
