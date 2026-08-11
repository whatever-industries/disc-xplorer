// CD-TEXT: the disc and track names some audio CDs carry alongside the audio.
//
// The data is a flat run of 18-byte packs, stored in the R-W subchannel of the
// lead-in and handed back verbatim by a drive's READ TOC/PMA/ATIP command with
// format 5, by macOS's `drutil cdtext`, and by the binary file a cue sheet's
// CDTEXTFILE line points at. One decoder serves all of them.
//
// Pack layout:
//   [0]      pack type — 0x80 title, 0x81 performer, … (see PackType)
//   [1]      track number; 0 is the disc itself
//   [2]      sequence number, counting up across the whole block
//   [3]      bit 7 unused, bits 6-4 block number, bits 3-0 character position
//   [4..16]  12 bytes of text
//   [16..18] CRC-16/CCITT over bytes 0..16, stored complemented
//
// A field is not one pack: the 12-byte payloads of consecutive packs of the same
// type are concatenated into one stream, and NUL bytes divide it into one string
// per track, starting at the track number of the first pack. A string that is a
// single NUL means "same as the previous track", which is how compilations avoid
// repeating one performer 20 times.
//
// Blocks 0..7 hold alternative languages; block 0 is the primary one and the
// only one read here.

use std::collections::BTreeMap;
use std::path::Path;

const PACK_SIZE: usize = 18;
const PAYLOAD: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackType {
    Title,
    Performer,
    Songwriter,
    Composer,
    Arranger,
    Message,
    DiscId,
    Genre,
    UpcIsrc,
    SizeInfo,
}

impl PackType {
    fn from_byte(b: u8) -> Option<PackType> {
        Some(match b {
            0x80 => PackType::Title,
            0x81 => PackType::Performer,
            0x82 => PackType::Songwriter,
            0x83 => PackType::Composer,
            0x84 => PackType::Arranger,
            0x85 => PackType::Message,
            0x86 => PackType::DiscId,
            0x87 => PackType::Genre,
            0x8E => PackType::UpcIsrc,
            0x8F => PackType::SizeInfo,
            _ => return None,
        })
    }

    /// Only the text fields are worth splitting per track; GENRE and SIZE_INFO
    /// are binary, and UPC/ISRC is an identifier rather than a name.
    fn is_text(self) -> bool {
        matches!(
            self,
            PackType::Title
                | PackType::Performer
                | PackType::Songwriter
                | PackType::Composer
                | PackType::Arranger
                | PackType::Message
        )
    }
}

/// The names attached to one track, or to the disc when track 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Names {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub songwriter: Option<String>,
    pub composer: Option<String>,
    pub arranger: Option<String>,
    pub message: Option<String>,
}

impl Names {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.performer.is_none()
            && self.songwriter.is_none()
            && self.composer.is_none()
            && self.arranger.is_none()
            && self.message.is_none()
    }

    fn set(&mut self, kind: PackType, value: String) {
        let slot = match kind {
            PackType::Title => &mut self.title,
            PackType::Performer => &mut self.performer,
            PackType::Songwriter => &mut self.songwriter,
            PackType::Composer => &mut self.composer,
            PackType::Arranger => &mut self.arranger,
            PackType::Message => &mut self.message,
            _ => return,
        };
        // The first block wins; later language blocks must not overwrite it.
        if slot.is_none() && !value.is_empty() {
            *slot = Some(value);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CdText {
    /// Names for the disc as a whole (track 0).
    pub disc: Names,
    /// Names by track number.
    pub tracks: BTreeMap<u8, Names>,
}

impl CdText {
    pub fn is_empty(&self) -> bool {
        self.disc.is_empty() && self.tracks.values().all(Names::is_empty)
    }

    /// The name to show for a track, falling back to nothing rather than to a
    /// placeholder — the caller already has "Track NN" for that.
    pub fn track_title(&self, track: u8) -> Option<&str> {
        self.tracks.get(&track)?.title.as_deref()
    }
}

/// CRC-16/CCITT-FALSE over the first 16 bytes, stored complemented.
fn pack_crc(pack: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in &pack[..16] {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    !crc
}

/// Text is Latin-1 unless the SIZE_INFO pack says otherwise; the alternative in
/// practice is MS-JIS on Japanese discs, which is not handled here. Decoding
/// byte-by-byte keeps a wrong guess to mangled accents rather than lost data.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Some sources prefix the packs with the 4-byte READ TOC response header (a
/// two-byte length followed by two reserved bytes). Detect that by which
/// interpretation divides evenly into 18-byte packs.
fn pack_bytes(data: &[u8]) -> &[u8] {
    if data.len() % PACK_SIZE == 0 {
        data
    } else if data.len() >= 4 && (data.len() - 4) % PACK_SIZE == 0 {
        &data[4..]
    } else {
        // Neither divides evenly — use as much as forms whole packs.
        let usable = data.len() - data.len() % PACK_SIZE;
        &data[..usable]
    }
}

/// Decode a run of CD-TEXT packs. Packs failing their CRC are skipped rather
/// than failing the parse: a disc that yields most of its names is worth more
/// than no names at all, and this data is read from the lead-in where errors
/// are common.
///
/// `last_track` is the highest track number on the disc, when the caller knows
/// it. The final pack is padded with NULs to fill its 12 bytes, and an empty
/// field otherwise means "same as the previous track" — so without the track
/// count, padding is indistinguishable from a real trailing repeat. Given the
/// count, fields past the last track are dropped as the padding they are;
/// without it, trailing empties are discarded, which loses a genuine repeat on
/// the last track but never invents tracks that do not exist.
pub fn parse_packs(data: &[u8], last_track: Option<u8>) -> CdText {
    let data = pack_bytes(data);
    let mut out = CdText::default();

    // Assemble each text type separately: its packs are contiguous in sequence
    // order, and the payloads join into one NUL-separated stream.
    for kind in [
        PackType::Title,
        PackType::Performer,
        PackType::Songwriter,
        PackType::Composer,
        PackType::Arranger,
        PackType::Message,
    ] {
        let mut stream: Vec<u8> = Vec::new();
        let mut first_track: Option<u8> = None;

        for pack in data.chunks_exact(PACK_SIZE) {
            if PackType::from_byte(pack[0]) != Some(kind) {
                continue;
            }
            // Block 0 only; later blocks repeat the same fields in another
            // language and would otherwise be appended as extra tracks.
            if (pack[3] >> 4) & 0x07 != 0 {
                continue;
            }
            let stored = u16::from_be_bytes([pack[16], pack[17]]);
            if stored != 0 && stored != pack_crc(pack) {
                continue;
            }
            first_track.get_or_insert(pack[1]);
            stream.extend_from_slice(&pack[4..4 + PAYLOAD]);
        }

        // Without a known track count, drop the trailing NUL padding: it would
        // otherwise read as a run of empty fields repeating the last real one.
        if last_track.is_none() {
            while stream.last() == Some(&0) {
                stream.pop();
            }
        }
        if !kind.is_text() || stream.is_empty() {
            continue;
        }
        let mut track = first_track.unwrap_or(0);
        let mut previous: Option<String> = None;
        for field in stream.split(|&b| b == 0) {
            // Past the last track on the disc, what remains is padding.
            if track > last_track.unwrap_or(99) {
                break;
            }
            let text = if field.is_empty() {
                // A lone NUL repeats the previous track's value. An empty run at
                // the very end is padding, which `previous` being None catches.
                match &previous {
                    Some(p) => p.clone(),
                    None => {
                        track = track.wrapping_add(1);
                        continue;
                    }
                }
            } else {
                latin1(field)
            };

            if track == 0 {
                out.disc.set(kind, text.clone());
            } else {
                out.tracks.entry(track).or_default().set(kind, text.clone());
            }
            previous = Some(text);
            track = track.wrapping_add(1);
        }
    }

    // Padding at the end of the stream can invent empty trailing tracks.
    out.tracks.retain(|_, n| !n.is_empty());
    out
}

/// Read whatever CD-TEXT a cue sheet describes.
///
/// Two forms exist and a cue may use either or both. `CDTEXTFILE "name.cdt"`
/// names a binary file of the same packs a drive returns, which is the disc's
/// own data. EAC and friends also write the names inline as `TITLE` and
/// `PERFORMER` lines — before the first `TRACK` for the disc, and under each
/// `TRACK` for that track. The binary file wins where both exist, since it is
/// the disc's own copy; inline lines fill anything it does not cover.
pub fn from_cue(cue_path: &Path, last_track: Option<u8>) -> CdText {
    let Ok(text) = std::fs::read_to_string(cue_path) else {
        return CdText::default();
    };
    let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));

    let mut out = CdText::default();

    // The binary sidecar first, so its values take the slots.
    for line in text.lines() {
        let t = line.trim();
        if !t.to_ascii_uppercase().starts_with("CDTEXTFILE") {
            continue;
        }
        let Some(name) = quoted(t) else { continue };
        // Cue sheets name the sidecar relative to themselves.
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            out = parse_packs(&bytes, last_track);
        }
        break;
    }

    // Then the inline lines, which only fill gaps.
    let mut track: Option<u8> = None;
    for line in text.lines() {
        let t = line.trim();
        let upper = t.to_ascii_uppercase();
        if upper.starts_with("TRACK ") {
            track = upper.split_whitespace().nth(1).and_then(|n| n.parse().ok());
            continue;
        }
        // FILE lines also carry a quoted name; they are not text fields.
        let kind = if upper.starts_with("TITLE") {
            PackType::Title
        } else if upper.starts_with("PERFORMER") {
            PackType::Performer
        } else if upper.starts_with("SONGWRITER") {
            PackType::Songwriter
        } else {
            continue;
        };
        let Some(value) = quoted(t) else { continue };
        if value.is_empty() {
            continue;
        }
        match track {
            None => out.disc.set(kind, value.to_string()),
            Some(n) => out.tracks.entry(n).or_default().set(kind, value.to_string()),
        }
    }

    out.tracks.retain(|_, n| !n.is_empty());
    out
}

/// CD-TEXT from a disc in a drive.
///
/// macOS ships `drutil cdtext`, which asks the drive and prints what it gets.
/// Nothing documents its exact output, and no disc here carries CD-TEXT to check
/// against, so the parser below reads labels rather than a fixed layout: it
/// takes `Label: Value` pairs, switches track when a line announces one, and
/// ignores everything it does not recognise. A shape it does not expect yields
/// no names, never wrong ones.
///
/// Linux and Windows would need a SCSI READ TOC/PMA/ATIP with format 5, whose
/// reply is the same pack stream `parse_packs` already reads — deliberately not
/// done here, as it needs unsafe ioctl bindings that cannot be tested from macOS.
#[cfg(target_os = "macos")]
pub fn from_drive(last_track: Option<u8>) -> CdText {
    let Ok(out) = std::process::Command::new("drutil").arg("cdtext").output() else {
        return CdText::default();
    };
    if !out.status.success() {
        return CdText::default();
    }
    parse_drutil(&String::from_utf8_lossy(&out.stdout), last_track)
}

#[cfg(not(target_os = "macos"))]
pub fn from_drive(_last_track: Option<u8>) -> CdText {
    CdText::default()
}

/// Map a field label to its pack type, tolerating the several names each field
/// goes by ("Performer" and "Artist" mean the same thing).
fn label_to_kind(label: &str) -> Option<PackType> {
    let l = label.trim().to_ascii_lowercase();
    let l = l.trim_start_matches("disc ").trim_start_matches("album ").trim();
    Some(match l {
        "title" | "name" | "song" => PackType::Title,
        "performer" | "artist" | "album artist" => PackType::Performer,
        "songwriter" | "writer" => PackType::Songwriter,
        "composer" => PackType::Composer,
        "arranger" => PackType::Arranger,
        "message" | "comment" => PackType::Message,
        _ => return None,
    })
}

/// A line that announces which track the following fields belong to, e.g.
/// "Track 3", "Track 3:", "Track: 3". Returns the number.
fn track_marker(line: &str) -> Option<u8> {
    let l = line.trim().trim_end_matches(':');
    let rest = l.strip_prefix("Track").or_else(|| l.strip_prefix("track"))?;
    rest.trim().trim_start_matches(':').trim().parse().ok()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_drutil(text: &str, last_track: Option<u8>) -> CdText {
    let mut out = CdText::default();
    // Fields before any track marker describe the disc.
    let mut track: Option<u8> = None;

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(n) = track_marker(t) {
            track = Some(n);
            continue;
        }
        let Some((label, value)) = t.split_once(':') else { continue };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        // "Track 3 Title: ..." names its own track rather than relying on a
        // preceding marker; split that number off so the rest is a plain label.
        let raw = label.trim();
        let (explicit, field) = match raw.strip_prefix("Track ").or_else(|| raw.strip_prefix("track ")) {
            Some(rest) => {
                let mut it = rest.trim().splitn(2, char::is_whitespace);
                match (it.next().and_then(|n| n.parse::<u8>().ok()), it.next()) {
                    (Some(n), Some(f)) => (Some(n), f.trim()),
                    _ => (None, raw),
                }
            }
            None => (None, raw),
        };
        let Some(kind) = label_to_kind(field) else { continue };

        match explicit.or(track) {
            None | Some(0) => out.disc.set(kind, value.to_string()),
            Some(n) => {
                if last_track.is_none_or(|last| n <= last) {
                    out.tracks.entry(n).or_default().set(kind, value.to_string());
                }
            }
        }
    }

    out.tracks.retain(|_, n| !n.is_empty());
    out
}

fn quoted(line: &str) -> Option<&str> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(&line[start..end])
}

/// Build the 18-byte packs for one text field. Used by the tests, and the only
/// practical way to exercise the decoder without a disc that carries CD-TEXT.
#[cfg(test)]
pub fn build_packs(kind: u8, first_track: u8, values: &[&str]) -> Vec<u8> {
    let mut stream = Vec::new();
    for v in values {
        stream.extend_from_slice(v.as_bytes());
        stream.push(0);
    }
    while stream.len() % PAYLOAD != 0 {
        stream.push(0);
    }

    let mut out = Vec::new();
    let mut track = first_track;
    for (seq, chunk) in stream.chunks(PAYLOAD).enumerate() {
        let mut pack = vec![kind, track, seq as u8, 0];
        pack.extend_from_slice(chunk);
        let crc = {
            let mut p = pack.clone();
            p.extend_from_slice(&[0, 0]);
            pack_crc(&p)
        };
        pack.extend_from_slice(&crc.to_be_bytes());
        out.extend_from_slice(&pack);

        // A pack is labelled with the track its first complete field belongs to,
        // so advance past every field this chunk closed. Only the first pack's
        // number is actually read back, but real encoders label them all.
        track = track.wrapping_add(chunk.iter().filter(|&&b| b == 0).count() as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_disc_and_track_titles() {
        let mut data = build_packs(0x80, 0, &["Blind Man's Zoo", "Eat for Two", "Please Forgive Us"]);
        data.extend_from_slice(&build_packs(0x81, 0, &["10,000 Maniacs"]));
        let t = parse_packs(&data, Some(2));

        assert_eq!(t.disc.title.as_deref(), Some("Blind Man's Zoo"));
        assert_eq!(t.disc.performer.as_deref(), Some("10,000 Maniacs"));
        assert_eq!(t.track_title(1), Some("Eat for Two"));
        assert_eq!(t.track_title(2), Some("Please Forgive Us"));
        assert_eq!(t.track_title(3), None);
    }

    // A field longer than 12 bytes spans packs, which is the case that a naive
    // one-pack-per-field reader gets wrong.
    #[test]
    fn joins_a_title_that_spans_several_packs() {
        let long = "A Title Considerably Longer Than Twelve Bytes";
        assert!(long.len() > PAYLOAD * 2);
        let t = parse_packs(&build_packs(0x80, 1, &[long, "Short"]), Some(2));
        assert_eq!(t.track_title(1), Some(long));
        assert_eq!(t.track_title(2), Some("Short"));
    }

    // A lone NUL means "same as the previous track" — how a single-artist album
    // avoids repeating the performer on every track.
    #[test]
    fn an_empty_field_repeats_the_previous_track() {
        let t = parse_packs(&build_packs(0x81, 1, &["One Artist", "", ""]), Some(3));
        assert_eq!(t.tracks[&1].performer.as_deref(), Some("One Artist"));
        assert_eq!(t.tracks[&2].performer.as_deref(), Some("One Artist"));
        assert_eq!(t.tracks[&3].performer.as_deref(), Some("One Artist"));
    }

    #[test]
    fn skips_packs_that_fail_their_crc() {
        // Long enough to occupy several packs, so there is a later one to damage.
        let data_ok = build_packs(0x80, 0, &["A Disc Title Long Enough To Span Packs", "Track One"]);
        assert!(data_ok.len() > PACK_SIZE * 2, "need several packs to make this meaningful");
        assert!(parse_packs(&data_ok, Some(1)).disc.title.is_some());

        // Corrupt a later pack's payload without fixing its CRC.
        let mut damaged = data_ok.clone();
        damaged[PACK_SIZE + 4] ^= 0xFF;
        let t = parse_packs(&damaged, Some(1));
        // The damaged pack is dropped rather than poisoning the whole read, so
        // something still comes back — just not the full text.
        assert_ne!(t, parse_packs(&data_ok, Some(1)), "the corrupt pack should have been skipped");
        assert!(!t.is_empty(), "one bad pack must not lose every name");
    }

    #[test]
    fn accepts_the_four_byte_read_toc_header() {
        let packs = build_packs(0x80, 0, &["Headered"]);
        let mut with_header = vec![0x00, (packs.len() + 2) as u8, 0x00, 0x00];
        with_header.extend_from_slice(&packs);
        assert_eq!(parse_packs(&with_header, None).disc.title.as_deref(), Some("Headered"));
    }

    #[test]
    fn ignores_alternative_language_blocks() {
        let mut data = build_packs(0x80, 0, &["English Title"]);
        // Same field in block 1: byte 3 carries the block number in bits 6-4.
        let mut other = build_packs(0x80, 0, &["Titre Francais"]);
        for pack in other.chunks_mut(PACK_SIZE) {
            pack[3] |= 1 << 4;
            let crc = pack_crc(pack);
            pack[16..18].copy_from_slice(&crc.to_be_bytes());
        }
        data.extend_from_slice(&other);
        assert_eq!(parse_packs(&data, None).disc.title.as_deref(), Some("English Title"));
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dx_cdtext_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // EAC and friends write the names straight into the cue.
    #[test]
    fn reads_names_written_inline_in_a_cue() {
        let d = temp_dir("inline");
        let cue = d.join("disc.cue");
        std::fs::write(&cue, concat!(
            "PERFORMER \"10,000 Maniacs\"\n",
            "TITLE \"Blind Man's Zoo\"\n",
            "FILE \"disc.bin\" BINARY\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Eat for Two\"\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Please Forgive Us\"\n",
            "    PERFORMER \"Guest Artist\"\n",
            "    INDEX 01 03:00:00\n",
        )).unwrap();

        let t = from_cue(&cue, Some(2));
        assert_eq!(t.disc.title.as_deref(), Some("Blind Man's Zoo"));
        assert_eq!(t.disc.performer.as_deref(), Some("10,000 Maniacs"));
        assert_eq!(t.track_title(1), Some("Eat for Two"));
        assert_eq!(t.track_title(2), Some("Please Forgive Us"));
        assert_eq!(t.tracks[&2].performer.as_deref(), Some("Guest Artist"));
        // FILE carries a quoted name too and must not be mistaken for a field.
        assert_ne!(t.disc.title.as_deref(), Some("disc.bin"));
        let _ = std::fs::remove_dir_all(&d);
    }

    // CDTEXTFILE names a binary sidecar of the same packs a drive returns.
    #[test]
    fn reads_the_binary_sidecar_and_prefers_it_over_inline() {
        let d = temp_dir("sidecar");
        std::fs::write(d.join("disc.cdt"),
                       build_packs(0x80, 0, &["Disc From Sidecar", "Track From Sidecar"])).unwrap();
        let cue = d.join("disc.cue");
        std::fs::write(&cue, concat!(
            "CDTEXTFILE \"disc.cdt\"\n",
            "TITLE \"Disc From Inline\"\n",
            "PERFORMER \"Only Inline Has This\"\n",
            "FILE \"disc.bin\" BINARY\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
        )).unwrap();

        let t = from_cue(&cue, Some(1));
        // The disc's own data wins where the two disagree...
        assert_eq!(t.disc.title.as_deref(), Some("Disc From Sidecar"));
        assert_eq!(t.track_title(1), Some("Track From Sidecar"));
        // ...and inline still fills what the sidecar does not carry.
        assert_eq!(t.disc.performer.as_deref(), Some("Only Inline Has This"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cue_with_no_cdtext_yields_nothing() {
        let d = temp_dir("bare");
        let cue = d.join("disc.cue");
        std::fs::write(&cue, concat!(
            "FILE \"disc.bin\" BINARY\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
        )).unwrap();
        assert!(from_cue(&cue, Some(1)).is_empty());
        // A missing CDTEXTFILE must not fail the read either.
        std::fs::write(&cue, "CDTEXTFILE \"absent.cdt\"\nFILE \"disc.bin\" BINARY\n").unwrap();
        assert!(from_cue(&cue, Some(1)).is_empty());
        assert!(from_cue(&d.join("no-such.cue"), None).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    // The drutil parser is a best guess: its output format is undocumented and
    // no disc here carries CD-TEXT. These do not prove it reads real output —
    // they only pin that it copes with several plausible shapes and, crucially,
    // that anything unrecognised yields nothing rather than nonsense.
    #[test]
    fn drutil_parser_copes_with_several_plausible_layouts() {
        let indented = "\
CD-Text:
  Title: Blind Man's Zoo
  Performer: 10,000 Maniacs
  Track 1:
    Title: Eat for Two
  Track 2:
    Title: Please Forgive Us
    Performer: Guest Artist
";
        let flat = "\
Disc Title: Blind Man's Zoo
Disc Performer: 10,000 Maniacs
Track 1 Title: Eat for Two
Track 2 Title: Please Forgive Us
Track 2 Performer: Guest Artist
";
        for (name, text) in [("indented", indented), ("flat", flat)] {
            let t = parse_drutil(text, Some(2));
            assert_eq!(t.disc.title.as_deref(), Some("Blind Man's Zoo"), "{name}");
            assert_eq!(t.disc.performer.as_deref(), Some("10,000 Maniacs"), "{name}");
            assert_eq!(t.track_title(1), Some("Eat for Two"), "{name}");
            assert_eq!(t.track_title(2), Some("Please Forgive Us"), "{name}");
            assert_eq!(t.tracks[&2].performer.as_deref(), Some("Guest Artist"), "{name}");
        }
    }

    #[test]
    fn drutil_parser_yields_nothing_rather_than_guessing() {
        // What the command actually prints with no disc, or with a disc that
        // carries no CD-TEXT, is not known — so anything unrecognised must be
        // silently empty rather than half-parsed.
        for text in [
            "",
            "No CD-Text found.\n",
            "Type: CD-ROM\nSessions: 1\n",
            "drutil: no media inserted\n",
            ":::\n",
        ] {
            assert!(parse_drutil(text, None).is_empty(), "should be empty for {text:?}");
        }
        // Track numbers beyond the disc are dropped when the count is known.
        let t = parse_drutil("Track 1 Title: Real\nTrack 9 Title: Bogus\n", Some(1));
        assert_eq!(t.track_title(1), Some("Real"));
        assert_eq!(t.track_title(9), None);
    }

    #[test]
    fn empty_and_malformed_input_yields_nothing() {
        assert!(parse_packs(&[], None).is_empty());
        assert!(parse_packs(&[0u8; 7], None).is_empty());
        assert!(parse_packs(&[0xFFu8; PACK_SIZE], None).is_empty());
    }
}
