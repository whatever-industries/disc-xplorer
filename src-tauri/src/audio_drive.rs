//! Experimental Redumper range reads for CD-DA in a physical drive.
use crate::audio_export::{publish_file, sync_encoded_file, CheckedRead, Format, TempDir};
use crate::disc_dump::{AudioDriveGuard, RedumperDumpState};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, State};
use tauri_plugin_shell::process::CommandEvent;

const LBA_ORIGIN: i64 = -45_150;
const FRAMES_PER_SECTOR: i64 = 588;
const BYTES_PER_FRAME: u64 = 4;
const MAX_DISC_LBA: i32 = 450_000;
const STEM: &str = "audio";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Track {
    number: u8,
    start: i32,
    end: i32,
}

#[derive(Serialize)]
pub(crate) struct AudioDisc {
    identity: String,
    tracks: Vec<Track>,
}

#[tauri::command]
pub(crate) fn physical_track_rip_available() -> bool {
    // Use only on test machines until this Redumper layout and drive access
    // are verified with real hardware on each release platform.
    std::env::var("DX_EXPERIMENTAL_TRACK_RIP").is_ok_and(|value| value == "1")
}

async fn check_drive(drive: String) -> Result<crate::DriveInfo, String> {
    if !physical_track_rip_available() {
        return Err("Direct audio ripping is not enabled on this build.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        crate::list_optical_drives()?
            .into_iter()
            .find(|d| d.raw_device_path == drive && d.has_disc)
            .ok_or_else(|| "No disc detected. Insert and refresh, or try dumping.".into())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn restore_mount(drive: crate::DriveInfo) -> Result<(), String> {
    // Redumper requests exclusive SCSI access on macOS and unmounts the disc.
    // Only restore a mount that existed before the probe or rip began.
    #[cfg(target_os = "macos")]
    if drive.mount_point.is_some() {
        tauri::async_runtime::spawn_blocking(move || {
            let status = std::process::Command::new("/usr/sbin/diskutil")
                .args(["mountDisk", &drive.raw_device_path])
                .output();
            if !status.is_ok_and(|output| output.status.success()) {
                return Err(
                    "Could not remount the disc. Eject and reinsert it to browse its files."
                        .to_owned(),
                );
            }
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = drive;
    Ok(())
}

async fn check_version(app: &AppHandle) -> Result<(), String> {
    let version = crate::get_redumper_version("internal".into(), None, app.clone()).await?;
    if known_layout(&version) {
        Ok(())
    } else {
        Err("This Redumper build is unsupported for direct audio ripping. Use Dump Disc.".into())
    }
}

fn known_layout(version: &str) -> bool {
    // These builds share the same dump bounds, TOC bytes, and sample-state layout.
    version
        .trim()
        .strip_prefix("redumper (build: b")
        .and_then(|version| version.strip_suffix(')'))
        .and_then(|build| build.parse::<u16>().ok())
        .is_some_and(|build| (760..=773).contains(&build))
}

async fn read_range(
    app: &AppHandle,
    lease: &AudioDriveGuard,
    drive: &str,
    folder: &Path,
    range: Range<i32>,
    refine_retries: Option<u32>,
) -> Result<String, String> {
    lease.check()?;
    let arguments = [
        if refine_retries.is_some() {
            "refine".into()
        } else {
            "dump".into()
        },
        "--disc-type=CD".into(),
        format!("--drive={drive}"),
        format!(
            "--image-path={}",
            folder.to_str().ok_or("Invalid temporary audio path.")?
        ),
        format!("--image-name={STEM}"),
        format!("--lba-start={}", range.start),
        format!("--lba-end={}", range.end),
        format!("--retries={}", refine_retries.unwrap_or(0)),
    ];
    let (mut rx, child) = crate::redumper_cmd("internal", None, app)?
        .args(arguments)
        .spawn()
        .map_err(|e| e.to_string())?;
    lease.set_child(child)?;
    let mut output = String::new();
    let mut exit_code = None;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes)
                if output.len() < 256 * 1024 =>
            {
                output.push_str(&String::from_utf8_lossy(&bytes));
                output.push('\n');
            }
            CommandEvent::Terminated(status) => {
                exit_code = status.code;
                break;
            }
            _ => {}
        }
    }
    if exit_code.is_some() {
        lease.clear_child();
    }
    lease.check()?;
    if exit_code != Some(0) {
        let reason = output
            .lines()
            .rev()
            .find(|line| line.trim_start().to_ascii_lowercase().starts_with("error:"));
        return Err(reason
            .unwrap_or("Redumper could not read this disc. Use Dump Disc.")
            .into());
    }
    Ok(output)
}

fn declared_length(bytes: &[u8]) -> Option<usize> {
    let count = u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?) as usize + 2;
    (count == bytes.len()).then_some(count)
}

fn parse_toc(toc: &[u8], full: &[u8]) -> Option<AudioDisc> {
    let length = declared_length(toc)?;
    let full_length = declared_length(full)?;
    if length < 20
        || (length - 4) % 8 != 0
        || full_length < 15
        || (full_length - 4) % 11 != 0
        || full.get(2..4)? != [1, 1]
    {
        return None;
    }
    let first = *toc.get(2)?;
    let last = *toc.get(3)?;
    if first != 1 || !(1..=99).contains(&last) || length != 4 + 8 * (usize::from(last) + 1) {
        return None;
    }
    let mut tracks = Vec::new();
    for (i, entry) in toc[4..].chunks_exact(8).enumerate() {
        let number = entry[2];
        let lba = i32::from_be_bytes(entry[4..8].try_into().ok()?);
        if !(0..=MAX_DISC_LBA).contains(&lba) {
            return None;
        }
        if let Some(previous) = tracks.last_mut() {
            let previous: &mut Track = previous;
            if lba <= previous.start {
                return None;
            }
            previous.end = lba;
        }
        if i == usize::from(last) {
            if number != 0xAA {
                return None;
            }
        } else {
            // Low nibble is MMC CONTROL. Reject data and four-channel audio.
            if entry[1] & 0x0c != 0 || number as usize != i + 1 {
                return None;
            }
            tracks.push(Track {
                number,
                start: lba,
                end: lba,
            });
        }
    }
    let mut hash = Sha256::new();
    hash.update(toc);
    hash.update(full);
    Some(AudioDisc {
        identity: format!("{:x}", hash.finalize()),
        tracks,
    })
}

fn read_toc(folder: &Path) -> Result<AudioDisc, String> {
    let read = |extension| -> Option<Vec<u8>> {
        let path = folder.join(format!("{STEM}.{extension}"));
        let meta = fs::symlink_metadata(&path).ok()?;
        (meta.is_file() && meta.len() <= 65_537)
            .then(|| fs::read(path).ok())
            .flatten()
    };
    let info = read("toc")
        .zip(read("fulltoc"))
        .and_then(|(toc, full)| parse_toc(&toc, &full));
    info.ok_or_else(|| "Direct ripping needs a single-session audio CD. Use Dump Disc.".into())
}

fn read_offset(log: &str) -> Option<i64> {
    let configuration = log
        .lines()
        .find_map(|line| line.trim().strip_prefix("configuration: "))?;
    if configuration.starts_with("GENERIC ")
        && log.contains("warning: drive read offset not found in the database")
    {
        return None;
    }
    let (offset, _) = configuration
        .split_once("(read offset: ")?
        .1
        .split_once(',')?;
    offset
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|offset| (-4 * FRAMES_PER_SECTOR..=4 * FRAMES_PER_SECTOR).contains(offset))
}

fn drive_range(track: &Track, offset: i64) -> Range<i32> {
    let first = i64::from(track.start) * FRAMES_PER_SECTOR + offset;
    let last = i64::from(track.end) * FRAMES_PER_SECTOR + offset;
    first.div_euclid(FRAMES_PER_SECTOR) as i32
        ..(last + FRAMES_PER_SECTOR - 1).div_euclid(FRAMES_PER_SECTOR) as i32
}

fn samples_verified(
    folder: &Path,
    track: &Track,
    lease: impl Fn() -> Result<(), String>,
) -> Result<bool, String> {
    let first = ((i64::from(track.start) - LBA_ORIGIN) * FRAMES_PER_SECTOR) as u64;
    let frames = i64::from(track.end - track.start) as u64 * FRAMES_PER_SECTOR as u64;
    let mut state = File::open(folder.join(format!("{STEM}.state"))).map_err(|e| e.to_string())?;
    state
        .seek(SeekFrom::Start(first))
        .map_err(|e| e.to_string())?;
    let mut left = frames;
    let mut buf = [0u8; 16384];
    while left > 0 {
        lease()?;
        let count = left.min(buf.len() as u64) as usize;
        // Redumper stores a byte per stereo frame. Only SUCCESS (4)
        // validates both the read and C2 status. Sparse holes are ERROR_SKIP (0).
        if state.read_exact(&mut buf[..count]).is_err() || buf[..count].iter().any(|&v| v != 4) {
            return Ok(false);
        }
        left -= count as u64;
    }
    Ok(true)
}

fn open_pcm(folder: &Path, track: &Track) -> Result<(impl Read, u64), String> {
    let mut pcm = File::open(folder.join(format!("{STEM}.scram"))).map_err(|e| e.to_string())?;
    let start =
        ((i64::from(track.start) - LBA_ORIGIN) * FRAMES_PER_SECTOR) as u64 * BYTES_PER_FRAME;
    let bytes = (i64::from(track.end - track.start) * FRAMES_PER_SECTOR) as u64 * BYTES_PER_FRAME;
    if pcm.metadata().map_err(|e| e.to_string())?.len() < start + bytes {
        return Err("The audio read ended before the track was complete.".into());
    }
    pcm.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    Ok((pcm.take(bytes), bytes))
}

async fn probe_audio(
    app: &AppHandle,
    lease: &AudioDriveGuard,
    drive: &str,
) -> Result<(AudioDisc, i64), String> {
    let probe = TempDir::create(&std::env::temp_dir())?;
    let log = read_range(app, lease, drive, &probe.0, 0..0, None).await?;
    let disc = read_toc(&probe.0)?;
    let offset =
        read_offset(&log).ok_or("The drive read offset could not be determined. Use Dump Disc.")?;
    Ok((disc, offset))
}

async fn read_audio_track(
    app: &AppHandle,
    lease: &AudioDriveGuard,
    drive: &str,
    identity: &str,
    number: u8,
    retries: u32,
) -> Result<(TempDir, Track), String> {
    let (disc, offset) = probe_audio(app, lease, drive).await?;
    if disc.identity != identity {
        return Err("The disc changed. Reopen it before ripping.".into());
    }
    let track = disc
        .tracks
        .into_iter()
        .find(|t| t.number == number)
        .ok_or("The selected audio track does not exist.")?;
    let work = TempDir::create(&std::env::temp_dir())?;
    let range = drive_range(&track, offset);
    let log = read_range(app, lease, drive, &work.0, range.clone(), None).await?;
    if read_toc(&work.0)?.identity != identity || read_offset(&log) != Some(offset) {
        return Err("The disc or drive changed during the read.".into());
    }
    // Redumper only applies retries during refine, even if --retries is passed
    // during dump. Refine the same range only when samples need recovery.
    let mut verified = samples_verified(&work.0, &track, || lease.check())?;
    if !verified && retries > 0 {
        let log = read_range(app, lease, drive, &work.0, range, Some(retries)).await?;
        if read_offset(&log) != Some(offset) {
            return Err("The disc or drive changed during the read.".into());
        }
        verified = samples_verified(&work.0, &track, || lease.check())?;
    }
    if !verified {
        return Err(
            "Some audio samples could not be verified. Use Dump Disc to recover them.".into(),
        );
    }
    Ok((work, track))
}

#[tauri::command]
pub(crate) async fn inspect_physical_audio(
    drive: String,
    app: AppHandle,
    state: State<'_, RedumperDumpState>,
) -> Result<AudioDisc, String> {
    let lease = state.reserve_audio(drive.clone())?;
    let mounted = check_drive(drive.clone()).await?;
    check_version(&app).await?;
    let result = probe_audio(&app, &lease, &drive).await;
    let mount = restore_mount(mounted).await;
    let (disc, _) = result?;
    mount?;
    lease.check()?;
    Ok(disc)
}

#[tauri::command]
pub(crate) async fn rip_physical_audio(
    drive: String,
    identity: String,
    track: u8,
    destination: String,
    format: Format,
    retries: u32,
    app: AppHandle,
    state: State<'_, RedumperDumpState>,
) -> Result<(), String> {
    if retries > 10000 {
        return Err("Sector retries must be between 0 and 10000.".into());
    }
    let lease = state.reserve_audio(drive.clone())?;
    let mounted = check_drive(drive.clone()).await?;
    check_version(&app).await?;
    let result = read_audio_track(&app, &lease, &drive, &identity, track, retries).await;
    let mount = restore_mount(mounted).await;
    let (work, track) = result?;
    mount?;
    tauri::async_runtime::spawn_blocking(move || {
        let destination = PathBuf::from(destination);
        if destination
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|ext| !ext.eq_ignore_ascii_case(format.extension()))
        {
            return Err("The destination extension does not match the audio format.".into());
        }
        let parent = destination
            .parent()
            .filter(|p| p.is_absolute())
            .ok_or("Choose an audio output folder.")?;
        let leaf = destination.file_name().ok_or("Choose an audio filename.")?;
        let parent = parent.canonicalize().map_err(|error| error.to_string())?;
        let destination = parent.join(leaf);
        let output = TempDir::create(&parent)?;
        let staged = output.0.join(format!("track.{}", format.extension()));
        let (pcm, bytes) = open_pcm(&work.0, &track)?;
        format.encode(CheckedRead(pcm, || lease.check()), bytes, &staged)?;
        sync_encoded_file(&staged).map_err(|e| e.to_string())?;
        lease.publish(|| publish_file(&staged, &destination).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_physical_audio(state: State<'_, RedumperDumpState>) -> Result<(), String> {
    state.cancel_audio()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn two_tracks() -> ([u8; 28], [u8; 15]) {
        let mut toc = [0u8; 28];
        toc[..4].copy_from_slice(&[0, 26, 1, 2]);
        for (descriptor, number, lba) in [(4, 1, 0i32), (12, 2, 12345), (20, 0xAA, 39368)] {
            toc[descriptor + 1] = 0x10; // Position ADR, audio CONTROL.
            toc[descriptor + 2] = number;
            toc[descriptor + 4..descriptor + 8].copy_from_slice(&lba.to_be_bytes());
        }
        let mut full = [0u8; 15];
        full[..5].copy_from_slice(&[0, 13, 1, 1, 1]);
        (toc, full)
    }

    #[test]
    fn single_session_audio_toc_sets_index01_and_lead_out_bounds() {
        let (mut toc, mut full) = two_tracks();
        let disc = parse_toc(&toc, &full).unwrap();
        assert_eq!(
            disc.tracks,
            [
                Track {
                    number: 1,
                    start: 0,
                    end: 12345
                },
                Track {
                    number: 2,
                    start: 12345,
                    end: 39368
                },
            ]
        );
        full[3] = 2;
        assert!(parse_toc(&toc, &full).is_none());
        full[3] = 1;
        toc[13] = 0x14;
        assert!(parse_toc(&toc, &full).is_none());
        toc[13] = 0x18;
        assert!(parse_toc(&toc, &full).is_none());
        toc[13] = 0x10;
        toc[27] += 1;
        assert_ne!(disc.identity, parse_toc(&toc, &full).unwrap().identity);
        assert!(parse_toc(&toc[..20], &full).is_none());
    }

    #[test]
    fn bounded_reads_cover_only_sectors_needed_for_the_drive_offset() {
        let t = Track {
            number: 1,
            start: 0,
            end: 1200,
        };
        assert_eq!(drive_range(&t, 0), 0..1200);
        assert_eq!(drive_range(&t, 6), 0..1201);
        assert_eq!(drive_range(&t, -12), -1..1200);
        assert_eq!(drive_range(&t, -588), -1..1199);
        assert_eq!(drive_range(&t, 589), 1..1202);
        assert_eq!(
            read_offset("configuration: GENERIC (read offset: +6, C2 shift: 0)"),
            Some(6)
        );
        assert_eq!(
            read_offset("configuration: GENERIC (read offset: -9223372036854775808, C2 shift: 0)"),
            None
        );
    }

    #[test]
    fn only_reviewed_redumper_layouts_enable_the_experiment() {
        assert!(known_layout("redumper (build: b760)"));
        assert!(known_layout("redumper (build: b773)"));
        assert!(!known_layout("redumper (build: b759)"));
        assert!(!known_layout("redumper (build: b774)"));
        assert!(!known_layout("modified b773"));
    }

    #[test]
    fn an_unknown_generic_drive_offset_is_not_assumed_to_be_zero() {
        assert_eq!(read_offset("warning: drive read offset not found in the database\n  configuration: GENERIC (read offset: +0, C2 shift: 0)"), None);
        assert_eq!(
            read_offset("  configuration: GENERIC (read offset: +0, C2 shift: 0)"),
            Some(0)
        );
    }

    #[test]
    fn verified_pcm_is_exactly_one_track_and_sparse_error_state_is_rejected() {
        let t = Track {
            number: 1,
            start: 5,
            end: 7,
        };
        let dir = TempDir::create(&std::env::temp_dir()).unwrap();
        let state_path = dir.0.join("audio.state");
        let mut state = File::create(&state_path).unwrap();
        let first = ((5 - LBA_ORIGIN) * FRAMES_PER_SECTOR) as u64;
        let frames = 2 * FRAMES_PER_SECTOR as usize;
        state.seek(SeekFrom::Start(first)).unwrap();
        state.write_all(&vec![4; frames]).unwrap();
        state.flush().unwrap();

        let mut pcm = File::create(dir.0.join("audio.scram")).unwrap();
        pcm.seek(SeekFrom::Start(first * 4 - 4)).unwrap();
        let audio: Vec<_> = (0..frames * 4).map(|byte| (byte % 253) as u8).collect();
        pcm.write_all(b"BEG!").unwrap();
        pcm.write_all(&audio).unwrap();
        pcm.write_all(b"END!").unwrap();
        pcm.flush().unwrap();

        assert!(samples_verified(&dir.0, &t, || Ok(())).unwrap());
        let (mut selected, bytes) = open_pcm(&dir.0, &t).unwrap();
        let mut data = Vec::new();
        selected.read_to_end(&mut data).unwrap();
        assert_eq!(bytes, audio.len() as u64);
        assert_eq!(data, audio);
        state
            .seek(SeekFrom::Start(first + frames as u64 - 1))
            .unwrap();
        state.write_all(&[0]).unwrap();
        assert!(!samples_verified(&dir.0, &t, || Ok(())).unwrap());
        assert!(samples_verified(&dir.0, &t, || Err("cancelled".into())).is_err());
    }

    #[test]
    fn finishing_a_rip_does_not_overwrite_an_existing_file() {
        let dir = TempDir::create(&std::env::temp_dir()).unwrap();
        let staged = dir.0.join("staged.wav");
        let dest = dir.0.join("Track 01.wav");
        fs::write(&staged, b"staged").unwrap();
        fs::write(&dest, b"original").unwrap();
        assert_eq!(
            publish_file(&staged, &dest).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&dest).unwrap(), b"original");
    }
}
