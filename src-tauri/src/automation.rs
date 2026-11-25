//! Read-only operations shared by the headless CLI and its MCP adapter.
use super::{
    cdtext, disc_date_report, disc_info, entry_tools, ext_of, extract_quoted, find_diff_flat,
    flat_info, list_disc_contents, read_sector_impl, resolve_cue_file, subchannel, DiscEntry,
};
use clap::Args;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const API_VERSION: u8 = 1;
const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;
const DEFAULT_WINDOW: u64 = 4_096;
const MAX_WINDOW: u64 = 65_536;
const DEFAULT_HASH_BYTES: u64 = 128 * 1024 * 1024;
const MAX_HASH_BYTES: u64 = 1024 * 1024 * 1024;
const IMAGE_FORMATS: &[&str] = &[
    "iso", "img", "bin", "cue", "cso", "ciso", "zso", "gcz", "chd", "nrg", "wbfs", "rvz", "wia",
    "uif", "skeleton", "zip", "cab", "tar",
];

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn input(message: impl Into<String>) -> Self {
        Self::new("invalid_argument", message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

type Result<T> = std::result::Result<T, ApiError>;

pub fn success(result: Value) -> Value {
    json!({ "api_version": API_VERSION, "result": result })
}

pub fn failure(error: ApiError) -> Value {
    json!({ "api_version": API_VERSION, "error": error })
}

/// Scope for input paths. CLI paths are selected directly by the caller.
#[derive(Debug, Clone, Default)]
pub struct Inspection {
    roots: Vec<ApprovedRoot>,
}

#[derive(Debug, Clone)]
struct ApprovedRoot {
    requested: PathBuf,
    resolved: PathBuf,
}

impl Inspection {
    pub fn scoped(roots: Vec<PathBuf>) -> Result<Self> {
        if roots.is_empty() {
            return Err(ApiError::input("Pass at least one --root directory."));
        }
        let roots = roots
            .into_iter()
            .map(|root| {
                let requested = std::path::absolute(&root)
                    .map_err(|error| ApiError::new("invalid_root", error.to_string()))?;
                let resolved = requested.canonicalize().map_err(|error| {
                    ApiError::new("invalid_root", format!("{}: {error}", root.display()))
                })?;
                if !resolved.is_dir() {
                    return Err(ApiError::new(
                        "invalid_root",
                        "Each --root must be a directory.",
                    ));
                }
                Ok(ApprovedRoot {
                    requested,
                    resolved,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { roots })
    }

    fn checked_file(&self, path: &Path) -> Result<PathBuf> {
        let outside = || {
            ApiError::new(
                "outside_root",
                "The input or a companion is outside the approved roots.",
            )
        };
        if !self.roots.is_empty()
            && (path.components().any(|part| part == Component::ParentDir)
                || !self.roots.iter().any(|root| {
                    path.starts_with(&root.requested) || path.starts_with(&root.resolved)
                }))
        {
            return Err(outside());
        }
        let canonical = path.canonicalize().map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound {
                "file_not_found"
            } else {
                "file_unavailable"
            };
            ApiError::new(
                code,
                "The requested input or companion file is unavailable.",
            )
        })?;
        if !self.roots.is_empty()
            && !self
                .roots
                .iter()
                .any(|root| canonical.starts_with(&root.resolved))
        {
            return Err(outside());
        }
        if !canonical
            .metadata()
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Err(ApiError::new(
                "file_unavailable",
                "The input must be a regular file.",
            ));
        }
        Ok(canonical)
    }

    fn check_sidecars(&self, image: &Path) -> Result<()> {
        if self.roots.is_empty() {
            return Ok(());
        }
        let stem = image
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let folder = image
            .parent()
            .ok_or_else(|| ApiError::input("Invalid image path."))?;
        let entries =
            fs::read_dir(folder).map_err(|e| ApiError::new("file_unavailable", e.to_string()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| ApiError::new("file_unavailable", e.to_string()))?
                .path();
            if matches!(
                ext_of(&path).as_str(),
                "sub" | "lsd" | "sbi" | "key" | "dkey" | "ird"
            ) && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(stem))
            {
                self.checked_file(&path)?;
            }
        }
        Ok(())
    }

    fn check_cue(&self, cue: &Path) -> Result<()> {
        let invalid = || {
            ApiError::new(
                "unsupported_layout",
                "This interface requires a CUE with local, quoted BIN filenames.",
            )
        };
        if !cue
            .metadata()
            .is_ok_and(|metadata| metadata.len() <= 1024 * 1024)
        {
            return Err(ApiError::new(
                "limit_exceeded",
                "The CUE exceeds the 1 MiB inspection limit.",
            ));
        }
        let text = fs::read_to_string(cue)
            .map_err(|e| ApiError::new("file_unavailable", e.to_string()))?;
        let parent = cue.parent().ok_or_else(invalid)?;
        let mut bins = BTreeSet::new();
        for line in text.lines() {
            let line = line.trim_start();
            let Some(directive) = line.split_whitespace().next() else {
                continue;
            };
            let bin = directive.eq_ignore_ascii_case("FILE");
            if !bin && !directive.to_ascii_uppercase().starts_with("CDTEXTFILE") {
                continue;
            }
            let name = extract_quoted(line).ok_or_else(invalid)?;
            let path = Path::new(name);
            let mut components = path.components();
            if name.contains(['/', '\\', ':'])
                || !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || (bin && ext_of(path) != "bin")
            {
                return Err(invalid());
            }
            if bin {
                let bin = resolve_cue_file(parent, name);
                if !bins.contains(&bin) {
                    if bins.len() >= 99 {
                        return Err(ApiError::new(
                            "limit_exceeded",
                            "A CUE may reference up to 99 BIN files.",
                        ));
                    }
                    self.checked_file(&bin)?;
                    self.check_sidecars(&bin)?;
                    bins.insert(bin);
                }
            } else {
                let cdtext = parent.join(path);
                // Optional CD-TEXT references can legitimately be missing.
                match fs::symlink_metadata(&cdtext) {
                    Ok(_) => {
                        let cdtext = self.checked_file(&cdtext)?;
                        if !cdtext
                            .metadata()
                            .is_ok_and(|metadata| metadata.len() <= 1024 * 1024)
                        {
                            return Err(ApiError::new(
                                "limit_exceeded",
                                "CD-TEXT exceeds the 1 MiB inspection limit.",
                            ));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(ApiError::new("file_unavailable", error.to_string())),
                }
            }
        }
        if bins.is_empty() {
            return Err(invalid());
        }
        Ok(())
    }

    fn image(&self, path: &Path) -> Result<String> {
        if !self.roots.is_empty() && !path.is_absolute() {
            return Err(ApiError::input("Use an absolute image path with MCP."));
        }
        let path = self.checked_file(path)?;
        let text = path
            .to_str()
            .ok_or_else(|| ApiError::input("The image path must be UTF-8."))?;
        let ext = ext_of(&path);
        if !IMAGE_FORMATS.contains(&ext.as_str()) {
            return Err(ApiError::new(
                "unsupported_image",
                "This image format is unavailable in the inspection API.",
            ));
        }
        self.check_sidecars(&path)?;
        if ext == "cue" {
            self.check_cue(&path)?;
        }
        Ok(text.into())
    }

    pub fn inspect(&self, args: ImageArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let info =
            disc_info::collect_info(&image).map_err(|e| ApiError::new("inspection_failed", e))?;
        let path = Path::new(&image);
        let text = (ext_of(path) == "cue").then(|| {
            let last = info
                .tracks
                .iter()
                .filter_map(|track| u8::try_from(track.number).ok())
                .max();
            cdtext::from_cue(path, last)
        });
        Ok(json!({ "format": ext_of(path), "disc_info": info, "cd_text": text }))
    }

    pub fn list(&self, args: ListArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let directory = dir(args.directory.as_deref())?;
        let limit = page_limit(args.limit)?;
        let mut entries = list_disc_contents(
            image,
            directory.clone(),
            args.filesystem,
            args.resource_forks.unwrap_or(false),
        )
        .map_err(|e| ApiError::new("directory_unavailable", e))?;
        entries.retain(|entry| args.include_deleted.unwrap_or(false) || !entry.deleted);
        entries.sort_by(|a, b| (&a.name, a.deleted, a.lba).cmp(&(&b.name, b.deleted, b.lba)));
        let offset = args.offset.unwrap_or(0);
        let end = offset.saturating_add(limit);
        let next_offset = (end < entries.len()).then_some(end);
        let entries: Vec<_> = entries.into_iter().skip(offset).take(limit).collect();
        Ok(json!({ "directory": directory, "entries": entries, "next_offset": next_offset }))
    }

    pub fn dates(&self, args: DatesArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let report = disc_date_report(image, args.filesystem)
            .map_err(|e| ApiError::new("date_scan_failed", e))?;
        let complete = !report.scan_limit_reached && report.unreadable_directories == 0;
        Ok(json!({ "report": report, "complete": complete,
            "time_basis": "On-disc calendar times. PVD offsets are shown where recorded." }))
    }

    pub fn hashes(&self, args: HashArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let file = file(&args.file)?;
        let limit = hash_limit(args.max_bytes)?;
        let hashes =
            entry_tools::hash_file_for_automation(image, file.clone(), args.filesystem, limit)
                .map_err(|e| {
                    let code = if e.contains("hash byte limit") {
                        "limit_exceeded"
                    } else {
                        "hash_failed"
                    };
                    ApiError::new(code, e)
                })?;
        Ok(json!({ "file": file, "hashes": hashes }))
    }

    pub fn location(&self, args: FileArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let file = file(&args.file)?;
        let location = entry_tools::locate_file(image, file.clone(), args.filesystem)
            .map_err(|e| ApiError::new("location_unavailable", e))?;
        Ok(json!({ "file": file, "location": location }))
    }

    pub fn sector(&self, args: SectorArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let basis = sector_basis(&image)?;
        let length = args.length.unwrap_or(256);
        if length > 4096 {
            return Err(ApiError::input("Sector reads return at most 4096 bytes."));
        }
        let read_error = |e| ApiError::new("sector_unavailable", e);
        let first = read_sector_impl(&image, 0).map_err(read_error)?;
        if args.lba >= first.total_sectors {
            return Err(ApiError::new(
                "sector_unavailable",
                "The LBA is beyond the image.",
            ));
        }
        let sector = if args.lba == 0 {
            first
        } else {
            read_sector_impl(&image, args.lba).map_err(read_error)?
        };
        let offset = args.offset.unwrap_or(0);
        let remaining = sector
            .bytes
            .get(offset..)
            .ok_or_else(|| ApiError::input("The byte offset is beyond this sector."))?;
        let bytes = &remaining[..length.min(remaining.len())];
        let hex: String = bytes
            .iter()
            .flat_map(|b| {
                const DIGITS: &[u8] = b"0123456789abcdef";
                [
                    DIGITS[(*b >> 4) as usize] as char,
                    DIGITS[(*b & 15) as usize] as char,
                ]
            })
            .collect();
        Ok(
            json!({ "lba": args.lba, "lba_basis": basis, "sector_size": sector.sector_size,
            "user_data_offset": sector.user_data_offset, "total_sectors": sector.total_sectors,
            "offset": offset, "bytes_returned": bytes.len(), "hex": hex }),
        )
    }

    pub fn subchannels(&self, args: SubchannelArgs) -> Result<Value> {
        let image = self.image(&args.image)?;
        let basis = sector_basis(&image)?;
        let report = subchannel::inspect_record(Path::new(&image), args.lba, args.disc_lba)
            .map_err(|e| ApiError::new("subchannel_unavailable", e))?;
        Ok(json!({ "lba": args.lba, "lba_basis": basis, "report": report }))
    }

    pub fn sector_difference(&self, args: SectorDiffArgs) -> Result<Value> {
        let a = self.image(&args.image_a)?;
        let b = self.image(&args.image_b)?;
        let basis_a = sector_basis(&a)?;
        let basis_b = sector_basis(&b)?;
        let limit = args.limit.unwrap_or(DEFAULT_WINDOW);
        if !(1..=MAX_WINDOW).contains(&limit) {
            return Err(ApiError::input(
                "Sector comparison limit must be between 1 and 65536.",
            ));
        }
        let read_error = |e| ApiError::new("sector_unavailable", e);
        let total_a = read_sector_impl(&a, 0).map_err(read_error)?.total_sectors;
        let total_b = read_sector_impl(&b, 0).map_err(read_error)?.total_sectors;
        let start = args.from_lba.unwrap_or(0);
        let end = start.saturating_add(limit).min(total_a.max(total_b));
        let shared = total_a.min(total_b);
        let common_end = end.min(shared);
        let mut difference = None;
        if start < common_end {
            difference = diff_common_sectors(&a, &b, start, common_end).map_err(read_error)?;
        }
        let tail = start.max(shared);
        if difference.is_none() && tail < end {
            difference = Some(tail);
        }
        let kind = difference.map(|lba| {
            if lba >= total_a {
                "only_in_b"
            } else if lba >= total_b {
                "only_in_a"
            } else {
                "different_bytes"
            }
        });
        let next_lba = (difference.is_none() && end < total_a.max(total_b)).then_some(end);
        Ok(
            json!({ "first_difference_lba": difference, "difference_kind": kind,
            "total_sectors_a": total_a, "total_sectors_b": total_b,
            "lba_basis_a": basis_a, "lba_basis_b": basis_b,
            "sectors_checked": difference.map_or(end.saturating_sub(start), |lba| lba - start + 1),
            "next_lba": next_lba }),
        )
    }

    pub fn compare_directories(&self, args: CompareArgs) -> Result<Value> {
        let image_a = self.image(&args.image_a)?;
        let image_b = self.image(&args.image_b)?;
        let directory_a = dir(args.directory_a.as_deref())?;
        let directory_b = dir(args.directory_b.as_deref().or(args.directory_a.as_deref()))?;
        let limit = page_limit(args.limit)?;
        let mut budget = hash_limit(args.max_bytes)?;
        let verify_contents = args.verify_contents.unwrap_or(false);
        let a = directory_map(&image_a, &directory_a, &args.filesystem_a)?;
        let mut b = directory_map(&image_b, &directory_b, &args.filesystem_b)?;
        let mut names: BTreeMap<String, (Option<DiscEntry>, Option<DiscEntry>)> = a
            .into_iter()
            .map(|(name, entry)| {
                let other = b.remove(&name);
                (name, (Some(entry), other))
            })
            .collect();
        names.extend(
            b.into_iter()
                .map(|(name, entry)| (name, (None, Some(entry)))),
        );
        let mut rows = Vec::new();
        let offset = args.offset.unwrap_or(0);
        let end = offset.saturating_add(limit);
        let next_offset = (end < names.len()).then_some(end);
        for (name, (a, b)) in names.into_iter().skip(offset).take(limit) {
            let mut issue = None;
            let status = match (&a, &b) {
                (Some(_), None) => "only_in_a",
                (None, Some(_)) => "only_in_b",
                (Some(a), Some(b)) if a.is_dir != b.is_dir => "different_type",
                (Some(a), Some(_)) if a.is_dir => "directory",
                (Some(a), Some(b)) if a.size_bytes != b.size_bytes => "different_size",
                (Some(a), Some(b)) if !verify_contents => {
                    if a.modified == b.modified {
                        "same_metadata"
                    } else {
                        "different_metadata"
                    }
                }
                (Some(a), Some(b)) => {
                    let left = u64::from(a.size_bytes);
                    let right = u64::from(b.size_bytes);
                    if left + right > budget {
                        issue =
                            Some("Hash budget exceeded. Compare this file separately.".to_owned());
                        "not_verified"
                    } else {
                        budget -= left + right;
                        let hash_a = entry_tools::hash_file_for_automation(
                            image_a.clone(),
                            join(&directory_a, &name),
                            args.filesystem_a.clone(),
                            left,
                        );
                        let hash_b = entry_tools::hash_file_for_automation(
                            image_b.clone(),
                            join(&directory_b, &name),
                            args.filesystem_b.clone(),
                            right,
                        );
                        match (hash_a, hash_b) {
                            (Ok(a), Ok(b)) if a.sha256 == b.sha256 => "same_contents",
                            (Ok(_), Ok(_)) => "different_contents",
                            (Err(error), _) | (_, Err(error)) => {
                                issue = Some(error);
                                "not_verified"
                            }
                        }
                    }
                }
                (None, None) => unreachable!("Every name came from at least one directory"),
            };
            rows.push(json!({ "name": name, "status": status, "entry_a": a, "entry_b": b, "issue": issue }));
        }
        Ok(
            json!({ "directory_a": directory_a, "directory_b": directory_b,
            "entries": rows, "next_offset": next_offset, "verified_contents_requested": verify_contents }),
        )
    }
}

fn sector_basis(image: &str) -> Result<&'static str> {
    let extension = ext_of(Path::new(image));
    if matches!(extension.as_str(), "zip" | "cab" | "tar") {
        return Err(ApiError::new(
            "unsupported_layout",
            "This archive does not expose disc sectors.",
        ));
    }
    if matches!(extension.as_str(), "cue" | "nrg") {
        Ok("first_data_track")
    } else {
        Ok("image")
    }
}

fn diff_common_sectors(
    a: &str,
    b: &str,
    start: u64,
    end: u64,
) -> std::result::Result<Option<u64>, String> {
    if let (Some(a), Some(b)) = (flat_info(a), flat_info(b)) {
        if a.sector_size == b.sector_size {
            return find_diff_flat(&a, &b, start, end - 1, true);
        }
    }
    for lba in start..end {
        if read_sector_impl(a, lba)?.bytes != read_sector_impl(b, lba)?.bytes {
            return Ok(Some(lba));
        }
    }
    Ok(None)
}

fn page_limit(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if (1..=MAX_PAGE).contains(&limit) {
        Ok(limit)
    } else {
        Err(ApiError::input("Page size must be between 1 and 200."))
    }
}

fn hash_limit(limit: Option<u64>) -> Result<u64> {
    let limit = limit.unwrap_or(DEFAULT_HASH_BYTES);
    if limit <= MAX_HASH_BYTES {
        Ok(limit)
    } else {
        Err(ApiError::input(
            "The hash byte budget must be at most 1 GiB.",
        ))
    }
}

fn dir(directory: Option<&str>) -> Result<String> {
    let directory = directory.unwrap_or("/");
    if directory == "/" {
        return Ok(directory.into());
    }
    internal_path(directory.trim_end_matches('/'))
}

fn file(path: &str) -> Result<String> {
    internal_path(path)
}

fn internal_path(path: &str) -> Result<String> {
    if path.starts_with('/')
        && path.len() > 1
        && !path.contains(['\\', '\0'])
        && path[1..]
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
    {
        Ok(path.into())
    } else {
        Err(ApiError::input(
            "Use an absolute path inside the image, such as /FOLDER/FILE.BIN.",
        ))
    }
}

fn join(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

fn directory_map(
    image: &str,
    directory: &str,
    filesystem: &Option<String>,
) -> Result<BTreeMap<String, DiscEntry>> {
    let entries = list_disc_contents(image.into(), directory.into(), filesystem.clone(), false)
        .map_err(|error| ApiError::new("directory_unavailable", error))?;
    let mut map = BTreeMap::new();
    for entry in entries.into_iter().filter(|entry| !entry.deleted) {
        if map.insert(entry.name.clone(), entry).is_some() {
            return Err(ApiError::new(
                "ambiguous_entry",
                "The directory contains multiple live entries with the same name.",
            ));
        }
    }
    Ok(map)
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageArgs {
    /// Path to an image. MCP requires absolute paths.
    pub image: PathBuf,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DatesArgs {
    pub image: PathBuf,
    /// Filesystem name as returned by inspect-image.
    #[arg(long)]
    pub filesystem: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    pub image: PathBuf,
    /// Directory inside the image, default /.
    #[arg(long)]
    pub directory: Option<String>,
    #[arg(long)]
    pub filesystem: Option<String>,
    /// Offset from next_offset on the previous page.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Page size, default 50, maximum 200.
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub include_deleted: Option<bool>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub resource_forks: Option<bool>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    pub image: PathBuf,
    /// Absolute path inside the image.
    pub file: String,
    #[arg(long)]
    pub filesystem: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HashArgs {
    pub image: PathBuf,
    /// Absolute path inside the image.
    pub file: String,
    #[arg(long)]
    pub filesystem: Option<String>,
    /// Maximum decoded bytes, default 128 MiB.
    #[arg(long)]
    pub max_bytes: Option<u64>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectorArgs {
    pub image: PathBuf,
    pub lba: u64,
    /// Offset within the sector, default zero.
    #[arg(long)]
    pub offset: Option<usize>,
    /// Number of raw bytes to return as hex, default 256. Zero returns metadata.
    #[arg(long)]
    pub length: Option<usize>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubchannelArgs {
    pub image: PathBuf,
    pub lba: u64,
    /// Inspect a disc-absolute sidecar record directly, including negative pregap LBAs.
    #[arg(long, allow_hyphen_values = true)]
    pub disc_lba: Option<i64>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SectorDiffArgs {
    pub image_a: PathBuf,
    pub image_b: PathBuf,
    /// First sector to compare, inclusive, default zero.
    #[arg(long)]
    pub from_lba: Option<u64>,
    /// Maximum sectors to examine, default 4096.
    #[arg(long)]
    pub limit: Option<u64>,
}

#[derive(Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareArgs {
    pub image_a: PathBuf,
    pub image_b: PathBuf,
    #[arg(long)]
    pub directory_a: Option<String>,
    #[arg(long)]
    pub directory_b: Option<String>,
    #[arg(long)]
    pub filesystem_a: Option<String>,
    #[arg(long)]
    pub filesystem_b: Option<String>,
    /// Offset from next_offset on the previous page.
    #[arg(long)]
    pub offset: Option<usize>,
    #[arg(long)]
    pub limit: Option<usize>,
    /// Hash same-sized files to verify their content.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub verify_contents: Option<bool>,
    /// Combined hash budget in bytes for this page, default 128 MiB.
    #[arg(long)]
    pub max_bytes: Option<u64>,
}
