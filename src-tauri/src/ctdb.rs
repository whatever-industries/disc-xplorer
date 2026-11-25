//! Read-only CTDB v3 verification. Audio is never sent to the service.
use crate::{audio_disc::AudioDisc, audio_services};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crc32fast::Hasher;
use quick_xml::{events::Event, Reader};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const STRIDE: u64 = 10 * 588;
const COVERAGE: &str = "The check excludes hidden track-one audio and the disc edges.";
const MAX_ENTRIES: usize = 256;
const MAX_ALIGNMENT_PROBES: usize = 16;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Verified,
    VerifiedAligned,
    NoEntry,
    NoMatch,
    Unsupported,
    Unavailable,
    Cancelled,
}

#[derive(Serialize)]
pub(crate) struct Report {
    status: Status,
    message: &'static str,
    detail: Option<String>,
    matches: Vec<ReferenceMatch>,
    coverage: &'static str,
}

#[derive(Debug, Serialize)]
struct ReferenceMatch {
    id: u64,
    shift: i32,
    confidence: u64,
    crc32: String,
}

impl Report {
    fn status(status: Status, detail: Option<String>) -> Self {
        let message = match status {
            Status::Verified => "Verified",
            Status::VerifiedAligned => "Verified after alignment",
            Status::NoEntry => "No database entry",
            Status::NoMatch => "No matching reference",
            Status::Unsupported => "Unsupported layout",
            Status::Unavailable => "Service unavailable",
            Status::Cancelled => "Verification cancelled",
        };
        Self {
            status,
            message,
            detail,
            matches: Vec::new(),
            coverage: COVERAGE,
        }
    }

    pub(crate) fn message(&self) -> &str {
        self.message
    }

    pub(crate) fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!(
            "CTDB Verification\n-----------------\n{}\n{}\n",
            self.message, self.coverage
        );
        if let Some(detail) = &self.detail {
            let clean: String = detail
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let _ = writeln!(out, "Details: {clean}");
        }
        for item in &self.matches {
            let _ = writeln!(
                out,
                "Reference {} | Alignment {:+} stereo samples | Confidence {} | CRC32 {}",
                item.id, item.shift, item.confidence, item.crc32
            );
        }
        out
    }
}

#[derive(Debug)]
struct Reference {
    id: u64,
    confidence: u64,
    crc: u32,
    syndrome: Vec<u16>,
}

fn parse_references(xml: &[u8], frames: u64) -> Result<(bool, Vec<Reference>), String> {
    let mut reader = Reader::from_reader(xml);
    let mut found = false;
    let mut root = false;
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"ctdb" => root = true,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"entry" => {
                found = true;
                if entries.len() >= MAX_ENTRIES {
                    break;
                }
                let mut attrs = HashMap::new();
                for attr in e.attributes() {
                    let attr = attr.map_err(|e| e.to_string())?;
                    let key = std::str::from_utf8(attr.key.as_ref())
                        .map_err(|e| e.to_string())?
                        .to_owned();
                    attrs.insert(
                        key,
                        attr.unescape_value()
                            .map_err(|e| e.to_string())?
                            .into_owned(),
                    );
                }
                let parse = || -> Option<Reference> {
                    if attrs.get("stride")?.parse::<u64>().ok()? != STRIDE {
                        return None;
                    }
                    let toc: Vec<_> = attrs
                        .get("toc")?
                        .split(':')
                        .map(str::parse::<u64>)
                        .collect::<Result<_, _>>()
                        .ok()?;
                    // v1 verifies all-audio layouts only. The reference may have a different stored HTOA.
                    if toc.len() < 2
                        || toc.windows(2).any(|p| p[0] >= p[1])
                        || (toc.last()? - toc.first()?).checked_mul(588)? != frames
                    {
                        return None;
                    }
                    let npar = attrs.get("npar")?.parse::<usize>().ok()?;
                    if npar < 8 {
                        return None;
                    }
                    let code = STANDARD.decode(attrs.get("syndrome")?).ok()?;
                    let length = npar.min(16) * 2;
                    let syndrome = code
                        .get(..length)?
                        .chunks_exact(2)
                        .map(|word| u16::from_le_bytes([word[0], word[1]]))
                        .collect();
                    Some(Reference {
                        id: attrs.get("id")?.parse().ok()?,
                        confidence: attrs.get("confidence")?.parse().ok()?,
                        crc: u32::from_str_radix(attrs.get("crc32")?, 16).ok()?,
                        syndrome,
                    })
                };
                if let Some(entry) = parse().filter(|entry| ids.insert(entry.id)) {
                    entries.push(entry);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root {
        return Err("Invalid CTDB response.".into());
    }
    Ok((found, entries))
}

async fn references(disc: &AudioDisc) -> Result<(bool, Vec<Reference>), String> {
    let toc = disc
        .toc
        .starts
        .iter()
        .chain(std::iter::once(&disc.toc.end))
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(":");
    let response = audio_services::client(false)?
        .get("https://db.cue.tools/lookup2.php")
        .query(&[
            ("version", "3"),
            ("ctdb", "1"),
            ("fuzzy", "0"),
            ("metadata", "none"),
            ("toc", &toc),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok((false, Vec::new()));
    }
    let bytes = audio_services::body(
        response.error_for_status().map_err(|e| e.to_string())?,
        512 * 1024,
    )
    .await?;
    parse_references(&bytes, disc.frames())
}

struct Windows {
    left: Vec<u8>,
    center: Option<Hasher>,
    right: Vec<u8>,
    width: u64,
}

impl Windows {
    fn load(disc: &AudioDisc, check: &impl Fn() -> bool) -> Result<Self, String> {
        let frames = disc.frames();
        let width = (frames / STRIDE)
            .checked_sub(2)
            .filter(|n| *n > 0)
            .ok_or("Audio is too short for CTDB verification.")?
            * STRIDE;
        let mut left = Vec::new();
        let mut right = Vec::new();
        let common_start = 2 * STRIDE - 1;
        let common_end = 1 + width;
        if common_end <= common_start {
            disc.pcm(0..frames)
                .and_then(|mut r| r.read_to_end(&mut left))
                .map_err(|e| e.to_string())?;
            return Ok(Self {
                left,
                center: None,
                right,
                width,
            });
        }
        let mut input = disc.pcm(0..frames).map_err(|e| e.to_string())?;
        left.resize(common_start as usize * 4, 0);
        input.read_exact(&mut left).map_err(|e| e.to_string())?;
        let mut center = Hasher::new();
        let mut remaining = (common_end - common_start) * 4;
        let mut chunk = vec![0; 256 * 1024];
        while remaining > 0 {
            if !check() {
                return Err("Verification cancelled".into());
            }
            let n = remaining.min(chunk.len() as u64) as usize;
            input
                .read_exact(&mut chunk[..n])
                .map_err(|e| e.to_string())?;
            center.update(&chunk[..n]);
            remaining -= n as u64;
        }
        input.read_to_end(&mut right).map_err(|e| e.to_string())?;
        Ok(Self {
            left,
            center: Some(center),
            right,
            width,
        })
    }

    fn crc(&self, start: u64) -> u32 {
        let begin = start as usize * 4;
        let Some(center) = &self.center else {
            return crc32fast::hash(&self.left[begin..(start + self.width) as usize * 4]);
        };
        let mut hash = Hasher::new();
        hash.update(&self.left[begin..]);
        hash.combine(center);
        hash.update(&self.right[..(start - 1) as usize * 4]);
        hash.finalize()
    }
}

// CTDB's verification syndrome is the first Reed-Solomon column over the
// same window as its CRC. The first (left) word of each stride is a 16-bit
// symbol. Evaluate that column at the roots alpha^0 through alpha^15 in
// GF(2^16), using the polynomial published in CUETools.Parity/Galois.cs.
fn syndrome(
    disc: &AudioDisc,
    start: u64,
    width: u64,
    check: &impl Fn() -> bool,
) -> Result<[u16; 16], String> {
    let mut input = disc.pcm(start..start + width).map_err(|e| e.to_string())?;
    let mut stride = vec![0; STRIDE as usize * 4];
    let mut result = [0; 16];
    for _ in 0..width / STRIDE {
        if !check() {
            return Err("Verification cancelled".into());
        }
        input.read_exact(&mut stride).map_err(|e| e.to_string())?;
        let word = u16::from_le_bytes([stride[0], stride[1]]);
        for (power, part) in result.iter_mut().enumerate() {
            let mut product = *part;
            for _ in 0..power {
                product = (product << 1) ^ if product & 0x8000 == 0 { 0 } else { 0x100b };
            }
            *part = product ^ word;
        }
    }
    Ok(result)
}

fn verify(
    disc: &AudioDisc,
    entries: &[Reference],
    check: &impl Fn() -> bool,
) -> Result<Vec<ReferenceMatch>, String> {
    let windows = Windows::load(disc, check)?;
    let mut by_crc: HashMap<u32, Vec<&Reference>> = HashMap::new();
    for entry in entries {
        by_crc.entry(entry.crc).or_default().push(entry);
    }
    let mut matches = Vec::new();
    // Try zero first, then increasingly distant shifts. This gives unambiguous
    // output for silence and repeating audio that match at multiple offsets.
    let shifts = std::iter::once(0).chain((1..STRIDE as i32).flat_map(|n| [n, -n]));
    let mut probes = 0;
    for shift in shifts {
        if !check() {
            return Err("Verification cancelled".into());
        }
        let start = (STRIDE as i32 + shift) as u64;
        let crc = windows.crc(start);
        let Some(candidates) = by_crc.get_mut(&crc) else {
            continue;
        };
        // Corrupt references or highly repetitive PCM can produce many identical
        // CRCs. Bound full audio re-reads and report an inconclusive result.
        if probes == MAX_ALIGNMENT_PROBES {
            if matches.is_empty() {
                return Err("Too many possible CTDB alignments.".into());
            }
            break;
        }
        probes += 1;
        let code = syndrome(disc, start, windows.width, check)?;
        candidates.retain(|r| {
            if r.syndrome == code[..r.syndrome.len()] {
                matches.push(ReferenceMatch {
                    id: r.id,
                    shift,
                    confidence: r.confidence,
                    crc32: format!("{crc:08x}"),
                });
                false
            } else {
                true
            }
        });
        if candidates.is_empty() {
            by_crc.remove(&crc);
            if by_crc.is_empty() {
                break;
            }
        }
    }
    matches.sort_by_key(|entry| {
        (
            entry.shift != 0,
            std::cmp::Reverse(entry.confidence),
            entry.shift.abs(),
            entry.id,
        )
    });
    disc.check_files().map_err(|e| e.to_string())?;
    Ok(matches)
}

pub(crate) async fn check_image(
    path: &Path,
    running: impl Fn() -> bool + Send + 'static,
) -> Report {
    if !running() {
        return Report::status(Status::Cancelled, None);
    }
    let disc = match AudioDisc::load(path) {
        Ok(disc) if disc.frames() >= STRIDE * 3 => disc,
        Ok(_) => {
            return Report::status(
                Status::Unsupported,
                Some("Audio is too short for CTDB verification.".into()),
            )
        }
        Err(error) => return Report::status(Status::Unsupported, Some(error)),
    };
    let entries = tokio::select! {
        entries = references(&disc) => entries,
        _ = async {
            while running() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        } => return Report::status(Status::Cancelled, None),
    };
    if !running() {
        return Report::status(Status::Cancelled, None);
    }
    let entries = match entries {
        Ok((false, _)) => return Report::status(Status::NoEntry, None),
        Ok((true, entries)) if entries.is_empty() => {
            return Report::status(
                Status::NoMatch,
                Some("No compatible reference for this layout.".into()),
            )
        }
        Ok((true, entries)) => entries,
        Err(error) => return Report::status(Status::Unavailable, Some(error)),
    };
    tauri::async_runtime::spawn_blocking(move || match verify(&disc, &entries, &running) {
        Ok(matches) => {
            let status = match matches.first() {
                Some(entry) if entry.shift == 0 => Status::Verified,
                Some(_) => Status::VerifiedAligned,
                None => Status::NoMatch,
            };
            Report {
                matches,
                ..Report::status(status, None)
            }
        }
        Err(_) if !running() => Report::status(Status::Cancelled, None),
        Err(error) => Report::status(Status::Unsupported, Some(error)),
    })
    .await
    .unwrap_or_else(|e| Report::status(Status::Unavailable, Some(e.to_string())))
}

#[derive(Default)]
pub(crate) struct Jobs(Mutex<HashMap<String, Arc<AtomicBool>>>);

#[derive(Serialize)]
pub(crate) struct View {
    #[serde(flatten)]
    report: Report,
    text: String,
}

#[tauri::command]
pub(crate) async fn verify_audio_rip(
    image_path: String,
    request_id: String,
    jobs: tauri::State<'_, Jobs>,
) -> Result<View, String> {
    {
        let mut jobs = jobs.0.lock().unwrap();
        if request_id.len() > 80 || jobs.len() >= 4 || jobs.contains_key(&request_id) {
            return Err("A verification is already running.".into());
        }
        jobs.insert(request_id.clone(), Arc::new(AtomicBool::new(true)));
    }
    let running = jobs.0.lock().unwrap()[&request_id].clone();
    let report = check_image(Path::new(&image_path), move || {
        running.load(Ordering::Relaxed)
    })
    .await;
    jobs.0.lock().unwrap().remove(&request_id);
    let name = Path::new(&image_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = format!("Disc Xplorer | {name}\n\n{}", report.text());
    Ok(View { report, text })
}

#[tauri::command]
pub(crate) fn cancel_audio_verification(request_id: String, jobs: tauri::State<'_, Jobs>) {
    if let Some(running) = jobs.0.lock().unwrap().get(&request_id) {
        running.store(false, Ordering::Relaxed);
    }
}

#[tauri::command]
pub(crate) fn save_audio_verification_report(path: String, text: String) -> Result<(), String> {
    let path = Path::new(&path);
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        || text.len() > 64 * 1024
    {
        return Err("Choose a text report destination.".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: [u16; 16] = [
        0xec4e, 0x21ea, 0xc4d8, 0x000f, 0x7c81, 0xaa29, 0xd6ab, 0xc069, 0xd981, 0x4a35, 0x39b4,
        0x6b93, 0xe55a, 0x53f0, 0xf885, 0xa982,
    ];

    #[tokio::test]
    #[ignore = "uses CTDB's public lookup for a documented commercial album TOC"]
    async fn public_ctdb_lookup_returns_reference_syndromes() {
        let temp = crate::audio_export::TempDir::create(&std::env::temp_dir()).unwrap();
        let bin = temp.0.join("layout.bin");
        std::fs::File::create(bin)
            .unwrap()
            .set_len(95312 * 2352)
            .unwrap();
        let cue = temp.0.join("layout.cue");
        let mut text = String::from("FILE \"layout.bin\" BINARY\n");
        for (index, lba) in [0, 15213, 32164, 46442, 63264, 80339]
            .into_iter()
            .enumerate()
        {
            use std::fmt::Write as _;
            let _ = writeln!(
                text,
                "  TRACK {:02} AUDIO\n    INDEX 01 {}",
                index + 1,
                crate::bincue::sectors_to_msf(lba)
            );
        }
        fs::write(&cue, text).unwrap();
        let (found, entries) = references(&AudioDisc::load(&cue).unwrap()).await.unwrap();
        assert!(found && !entries.is_empty());
    }

    #[test]
    fn matches_cuetools_syndrome_at_zero_and_shifted_and_rejects_changed_pcm() {
        let temp = crate::audio_export::TempDir::create(&std::env::temp_dir()).unwrap();
        let cue = temp.0.join("album.cue");
        let bin = temp.0.join("album.bin");
        fs::write(
            &cue,
            "FILE \"album.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let mut pcm = Vec::new();
        for frame in 0..7 * STRIDE as u32 {
            let word = (frame.wrapping_mul(1664525) ^ (frame >> 5)) as u16;
            pcm.extend_from_slice(&word.to_le_bytes());
            pcm.extend_from_slice(&(!word).to_le_bytes());
        }
        fs::write(&bin, &pcm).unwrap();
        let disc = AudioDisc::load(&cue).unwrap();
        let windows = Windows::load(&disc, &|| true).unwrap();
        let base = STRIDE as usize * 4;
        let end = 6 * base;
        let crc = crc32fast::hash(&pcm[base..end]);
        assert_eq!(crc, 0x7612f4a0);
        assert_eq!(windows.crc(STRIDE), crc);
        // The parity vector was calculated using the CUETools Galois16 encode table
        // and Parity2Syndrome, independent of the verification implementation.
        assert_eq!(
            syndrome(&disc, STRIDE, 5 * STRIDE, &|| true).unwrap(),
            REFERENCE
        );
        for shift in [-42_i32, 0, 11] {
            let mut shifted = pcm.clone();
            shifted.rotate_right((shift * 4).rem_euclid(pcm.len() as i32) as usize);
            fs::write(&bin, &shifted).unwrap();
            let entries = [Reference {
                id: 17,
                confidence: 4,
                crc,
                syndrome: REFERENCE.to_vec(),
            }];
            let matched = verify(&AudioDisc::load(&cue).unwrap(), &entries, &|| true).unwrap();
            assert_eq!(matched.len(), 1);
            assert_eq!(matched[0].shift, shift);
        }
        pcm[base * 3 + 206] ^= 1; // interior sample, outside the sparse syndrome column
        fs::write(&bin, &pcm).unwrap();
        let entries = [Reference {
            id: 17,
            confidence: 4,
            crc,
            syndrome: REFERENCE.to_vec(),
        }];
        let changed = AudioDisc::load(&cue).unwrap();
        assert!(verify(&changed, &entries, &|| true).unwrap().is_empty());
        assert!(verify(&changed, &entries, &|| false).is_err());
    }

    #[test]
    fn accepts_version_three_references_and_distinguishes_empty_responses() {
        let bytes: Vec<_> = REFERENCE.into_iter().flat_map(u16::to_le_bytes).collect();
        let syndrome = STANDARD.encode(bytes);
        let xml = format!(
            r#"<ctdb xmlns="http://db.cuetools.net/ns/mmd-1.0#"><entry id="17" confidence="4" crc32="1234abcd" stride="5880" npar="16" syndrome="{syndrome}" toc="1:71"/></ctdb>"#
        );
        let (found, references) = parse_references(xml.as_bytes(), 70 * 588).unwrap();
        assert!(found);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].syndrome, REFERENCE);
        assert!(parse_references(b"<ctdb/>", 70 * 588).is_ok_and(|value| !value.0));
        assert!(parse_references(b"<html/>", 70 * 588).is_err());
        assert!(parse_references(xml.as_bytes(), 71 * 588)
            .is_ok_and(|value| value.0 && value.1.is_empty()));
    }
}
