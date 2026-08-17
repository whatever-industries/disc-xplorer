import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";

const SUPPORT_URL = "https://whatever-industries.blogspot.com/p/support.html";

const IS_SECTOR_VIEW_WINDOW = getCurrentWindow().label.startsWith("sv");

// Build of the redumper binary bundled as a sidecar. Known at compile time, so
// we display it without probing the binary at runtime. Bump this whenever the
// bundled redumper in src-tauri/binaries/ is updated.
const REDUMPER_INTERNAL_VERSION = "redumper (build: b720)";
import { open, save, confirm } from "@tauri-apps/plugin-dialog";
import { downloadDir } from "@tauri-apps/api/path";
import { SectorView } from "./SectorView";
import iconDark from "./assets/icon_dark.png";
import iconLight from "./assets/icon_light.png";
import "./App.css";

interface DiscEntry {
  name: string;
  is_dir: boolean;
  lba: number;
  size: number;
  size_bytes: number;
  modified: string;
  deleted?: boolean;
  // CD-ROM XA streaming file: offer the "as XA" extraction for it.
  is_xa?: boolean;
}

interface DateReport {
  pvd_created: string;
  pvd_modified: string;
  latest_path: string;
  latest_date: string;
  entries_scanned: number;
}

// Detection returns filesystems and alternative views of them in one list.
// Joliet and Rock Ridge are name views of the same ISO 9660 tree, Path Table is
// an index of it, and El Torito is a boot image rather than a tree — extracting
// the list verbatim would write the ISO files several times over under different
// names, and fail outright on the index.
//
// `name` is the folder to write into; `pass` is what save_directory wants, which
// for ISO 9660 is the namespace to read the names through.
const ISO_VIEWS = ["Joliet", "Rock Ridge", "Path Table", "El Torito"];

function distinctFilesystems(list: string[]): { name: string; pass: string }[] {
  const out: { name: string; pass: string }[] = [];
  // A UDF-bridge disc — most video and data DVDs — carries UDF and ISO 9660 as
  // two descriptions of the same files, with ISO 9660 present for compatibility.
  // Taking both would write the whole disc twice, so UDF stands for both.
  const udf = list.find((fs) => fs.startsWith("UDF"));
  if (udf) {
    out.push({ name: udf, pass: udf });
  } else if (list.includes("ISO 9660")) {
    // Richest naming wins: Rock Ridge keeps POSIX names, Joliet long Unicode ones.
    const pass = list.includes("Rock Ridge") ? "Rock Ridge"
      : list.includes("Joliet") ? "Joliet"
      : "ISO 9660";
    out.push({ name: "ISO 9660", pass });
  }
  // HFS alongside ISO 9660 is a Mac/PC hybrid and XDVDFS alongside it is an Xbox
  // game partition next to a DVD-Video zone — those really are separate content,
  // so unlike the views above they each get their own extraction.
  for (const fs of list) {
    if (fs === "ISO 9660" || fs.startsWith("UDF") || ISO_VIEWS.includes(fs)) continue;
    out.push({ name: fs, pass: fs });
  }
  return out;
}

// Inline SVG icons, replacing the pictographic emoji this UI used to draw.
//
// Those came from the host's colour-emoji font, and on Fedora 44 rendering one
// crashes WebKitGTK's Skia backend in its COLRv1 gradient path — so every file
// listing killed the renderer and left a white window (issue #11). Drawing them
// ourselves removes the dependency on whatever font the system happens to ship,
// and gets the same icons on all three platforms rather than three different
// sets. Sized in `em` so they inherit the font-size the emoji were sized by,
// and stroked in `currentColor` so they follow all four themes for free.
//
// Symbols that default to text presentation — ✕, ⚙, ⚠ — are left alone: they
// come from a normal text font and never reach the COLRv1 code.
type IconName =
  | "folder" | "file" | "disc" | "disc-data" | "music" | "filesystem"
  | "calendar" | "search" | "volume" | "muted" | "repeat" | "download"
  | "file-image" | "file-video" | "file-audio" | "file-text" | "file-web"
  | "file-archive" | "file-exec" | "file-disc" | "file-font" | "export-list" | "warning" | "arrow-up" | "index" | "play" | "pause";

const tile = (fill: string) => (
  <rect x="1.7" y="1.7" width="12.6" height="12.6" rx="3" fill={fill} />
);

const ICON_PATHS: Record<IconName, React.ReactNode> = {
  folder: <>
    <path fill="#D2952F" d="M1.6 4.3a1 1 0 0 1 1-1h3.2l1.4 1.7h6.2a1 1 0 0 1 1 1v1.4H1.6Z" />
    <path fill="#EFB759" d="M1.6 6.2h12.8v6.1a1 1 0 0 1-1 1H2.6a1 1 0 0 1-1-1Z" />
  </>,
  // The unknown type is the tile itself with a corner turned down, rather than a
  // page drawn inside a tile — that read as a document sitting on a card and
  // meant nothing. Same square footprint as every other type, neutral grey so a
  // file we cannot place still looks different from the ones we can.
  file: <>
    <path fill="#7A8593" d="M4.7 1.7H9.8L14.3 6.2V11.3A3 3 0 0 1 11.3 14.3H4.7A3 3 0 0 1 1.7 11.3V4.7A3 3 0 0 1 4.7 1.7Z" />
    <path fill="#AEB8C4" d="M9.8 1.7 14.3 6.2H11a1.2 1.2 0 0 1-1.2-1.2Z" />
  </>,
  disc: <>
    <circle cx="8" cy="8" r="6.1" fill="#C3D0DF" />
    <path fill="#8FB9E8" d="M8 1.9a6.1 6.1 0 0 1 5.3 3.1l-2.1 1.2A3.7 3.7 0 0 0 8 4.3Z" />
    <circle cx="8" cy="8" r="1.9" fill="#4A5768" />
    <circle cx="8" cy="8" r="0.7" fill="#EDF1F6" />
  </>,
  "disc-data": <>
    <circle cx="8" cy="8" r="6.1" fill="#C3D0DF" />
    <path fill="#B39BE6" d="M8 1.9a6.1 6.1 0 0 1 5.3 3.1l-2.1 1.2A3.7 3.7 0 0 0 8 4.3Z" />
    <circle cx="8" cy="8" r="1.9" fill="#4A5768" />
    <circle cx="8" cy="8" r="0.7" fill="#EDF1F6" />
  </>,
  music: <>
    <path fill="#6AA9F0" d="M6.1 11.8V4.2l6.2-1.3v7.5h-1.5V4.6l-3.2.7v6.5Z" />
    <ellipse cx="4.7" cy="12" rx="1.9" ry="1.55" fill="#6AA9F0" />
    <ellipse cx="10.9" cy="10.7" rx="1.9" ry="1.55" fill="#6AA9F0" />
  </>,
  // Drawn rather than set as ▶ and ⏸: those are unrelated characters from
  // possibly different fonts, so their relative size is whatever the platform
  // decides — the pause read visibly smaller than the play beside it. Matched
  // here to the same 9-unit height and drawn in currentColor so the playing
  // row keeps its accent.
  play: <path fill="currentColor" d="M5.6 3.4 12.6 8l-7 4.6Z" />,
  pause: <>
    <rect x="5" y="3.5" width="2.3" height="9" rx="0.7" fill="currentColor" />
    <rect x="8.7" y="3.5" width="2.3" height="9" rx="0.7" fill="currentColor" />
  </>,
  // The Path Table is metadata about ISO 9660, not a filesystem beside it — a
  // list rather than the stack of layers the real ones carry.
  index: <>
    <circle cx="3.3" cy="4.2" r="1.1" fill="#8AB4F8" />
    <circle cx="3.3" cy="8" r="1.1" fill="#8AB4F8" />
    <circle cx="3.3" cy="11.8" r="1.1" fill="#8AB4F8" />
    <path fill="none" stroke="#5B8DEF" strokeWidth="1.5" strokeLinecap="round" d="M6.5 4.2h6.2M6.5 8h6.2M6.5 11.8h4.1" />
  </>,
  filesystem: <>
    <path fill="#8AB4F8" d="M8 2.1 14.4 5.2 8 8.3 1.6 5.2Z" />
    <path fill="#5B8DEF" d="m3 7.5-1.4.7L8 11.3l6.4-3.1-1.4-.7L8 10Z" />
    <path fill="#3E6FD0" d="m3 10.3-1.4.7L8 14.1l6.4-3.1-1.4-.7L8 12.8Z" />
  </>,
  // Kept as a plain outline in currentColor: it sits on a coloured toolbar
  // button, so it takes that button's own text colour and stays legible on
  // every theme rather than fighting the background with fixed fills.
  // Drawn to an exactly square 11.6 x 11.6 extent, centred in the box, so it
  // matches the other toolbar glyphs rather than being fractionally taller.
  calendar: <>
    <rect x="2.2" y="4.2" width="11.6" height="9.6" rx="1.1" fill="none" stroke="currentColor" strokeWidth="1.5" />
    <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" d="M2.2 7.4h11.6M5.6 2.2v3.4M10.4 2.2v3.4" />
  </>,
  "arrow-up": <>
    <path fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" d="M8 13.4V2.9M3.4 7.5 8 2.9l4.6 4.6" />
  </>,
  search: <>
    <circle cx="6.8" cy="6.8" r="4.8" fill="none" stroke="currentColor" strokeWidth="1.9" />
    <path fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" d="m10.5 10.5 3.6 3.6" />
  </>,
  volume: <>
    <path fill="#C3D0DF" d="M3.1 6.1h2.3l3.3-2.9v9.6L5.4 9.9H3.1Z" />
    <path fill="none" stroke="#6AA9F0" strokeWidth="1.4" strokeLinecap="round" d="M11 5.9a3 3 0 0 1 0 4.2M12.9 4.1a5.6 5.6 0 0 1 0 7.8" />
  </>,
  muted: <>
    <path fill="#C3D0DF" d="M3.1 6.1h2.3l3.3-2.9v9.6L5.4 9.9H3.1Z" />
    <path fill="none" stroke="#DE5A53" strokeWidth="1.6" strokeLinecap="round" d="m11.1 6.3 3.3 3.4M14.4 6.3l-3.3 3.4" />
  </>,
  repeat: <>
    <path fill="none" stroke="#6AA9F0" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" d="M3 6.7V6a1.9 1.9 0 0 1 1.9-1.9h6.4M13 9.3V10a1.9 1.9 0 0 1-1.9 1.9H4.7" />
    <path fill="#6AA9F0" d="m10.4 1.8 3 2.3-3 2.3ZM5.6 14.2l-3-2.3 3-2.3Z" />
  </>,
  download: <>
    <path fill="#6AA9F0" d="M7.1 2.2h1.8v4.9h2.5L8 11.2 4.6 7.1h2.5Z" />
    <rect x="2.8" y="12.3" width="10.4" height="1.7" rx="0.85" fill="#93A3B6" />
  </>,
  // A known type fills the whole box: a rounded tile in its colour with a white
  // symbol knocked out. The document silhouette these started as is tall and
  // narrow, so whatever mark went inside it only got a few pixels and turned to
  // mush at 14px. Unknown files keep the plain page above, which usefully makes
  // "I do not know what this is" look different from every type we do know.
  "file-image": <>{tile("#3E9E6B")}
    <circle cx="6" cy="6.2" r="1.35" fill="#fff" />
    <path fill="#fff" d="M3.5 11.8 6.3 8.3l1.9 2.2 1.5-1.7 2.8 3Z" />
  </>,
  "file-video": <>{tile("#C94F38")}
    <path fill="#fff" d="M6.2 4.9 11.4 8 6.2 11.1Z" />
  </>,
  "file-audio": <>{tile("#7B5AC6")}
    <path fill="#fff" d="M7.4 10.5V4.8l3.8-.85v1.9L8.9 6.4v4.1Z" />
    <ellipse cx="6.2" cy="10.7" rx="1.7" ry="1.4" fill="#fff" />
  </>,
  "file-text": <>{tile("#66768B")}
    <path stroke="#fff" strokeWidth="1.35" strokeLinecap="round" d="M4.6 5.7h6.8M4.6 8h6.8M4.6 10.3h4.4" />
  </>,
  "file-web": <>{tile("#2C6FC2")}
    <circle cx="8" cy="8" r="3.6" fill="none" stroke="#fff" strokeWidth="1.2" />
    <path fill="none" stroke="#fff" strokeWidth="1.2" d="M4.4 8h7.2M8 4.4c1.9 2.1 1.9 5.1 0 7.2-1.9-2.1-1.9-5.1 0-7.2Z" />
  </>,
  "file-archive": <>{tile("#BE8720")}
    <path fill="#fff" d="M7.2 3.3h1.6v1.6H7.2Zm0 3h1.6v1.6H7.2Z" />
    <rect x="6.5" y="8.9" width="3" height="3.6" rx="0.8" fill="#fff" />
    <rect x="7.45" y="10.1" width="1.1" height="1.3" rx="0.35" fill="#BE8720" />
  </>,
  "file-exec": <>{tile("#515F76")}
    <path fill="none" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" d="m4.7 5.5 2.4 2.5-2.4 2.5" />
    <path stroke="#fff" strokeWidth="1.5" strokeLinecap="round" d="M8.3 10.5h3" />
  </>,
  "file-disc": <>{tile("#2B8996")}
    <circle cx="8" cy="8" r="3.6" fill="#fff" />
    <circle cx="8" cy="8" r="1.15" fill="#2B8996" />
  </>,
  // The damaged-sector report. This was a "✕", which reads as close or dismiss
  // everywhere else in the interface — and being red made it look like a
  // destructive action rather than "there are unreadable sectors on this disc".
  warning: <>
    <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" d="M8 2.3 14.7 13.5H1.3Z" />
    <path fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" d="M8 6.5v3" />
    <circle cx="8" cy="11.7" r="0.85" fill="currentColor" />
  </>,
  // A list with an arrow leaving it. This was three plain horizontal rules,
  // which is the universal hamburger-menu glyph — it read as a menu or a
  // settings button rather than "write this listing out to a file".
  "export-list": <>
    <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" d="M2.2 3.6h7.2M2.2 8h7.2M2.2 12.4h4.6" />
    <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" d="M12.4 3.6v8.9M10.1 10.2l2.3 2.3 2.3-2.3" />
  </>,
  "file-font": <>{tile("#966334")}
    <path fill="#fff" d="M8 3.5 11.9 12.5H10.15l-.72-1.75H6.57l-.72 1.75H4.1Zm-.85 5.6h1.7L8 7Z" />
  </>,
};

// Which icon a filename gets. Extension-based, like the double-click behaviour
// that already keys off PREVIEW_EXTS and NESTED_IMAGE_EXTS. Deliberately not
// the host's registered-application icon: that needs separate Windows, macOS
// and Linux implementations, and Linux has no dependable answer — the same
// class of host dependency that caused the white window on Fedora.
const FILE_ICON_BY_EXT: Record<string, IconName> = {};
for (const [icon, exts] of [
  ["file-image", ["jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "pcx", "tga", "svg", "heic", "psd", "pict", "pic"]],
  ["file-video", ["mp4", "m4v", "mov", "avi", "mkv", "webm", "mpg", "mpeg", "m2v", "wmv", "flv", "ogv", "3gp", "vob", "str", "rm", "asf"]],
  ["file-audio", ["mp3", "wav", "flac", "ogg", "aac", "m4a", "wma", "aif", "aiff", "au", "mid", "midi", "voc", "mod", "xm", "s3m", "it", "xa", "cda", "snd"]],
  ["file-web", ["html", "htm", "xml", "json", "css", "js", "shtml", "asp", "php"]],
  ["file-text", ["txt", "rtf", "md", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub", "csv", "log", "ini", "cfg", "nfo", "pdf", "diz", "me", "1st", "inf", "reg"]],
  ["file-archive", ["zip", "tar", "tgz", "tbz", "txz", "gz", "bz2", "xz", "7z", "rar", "cab", "lha", "lzh", "arj", "ace", "vpk", "pak", "wad", "z"]],
  ["file-exec", ["exe", "com", "bat", "cmd", "dll", "sh", "app", "msi", "scr", "drv", "sys", "ocx", "vxd", "386", "so", "dylib"]],
  ["file-disc", ["iso", "img", "chd", "cdi", "nrg", "mdx", "mds", "cue", "gdi", "ccd", "wbfs", "cso", "ciso", "ecm", "uif", "wux", "wud", "gcz", "wua", "rvz", "wia", "nds", "toc", "b5t", "b6t", "daa"]],
  ["file-font", ["ttf", "otf", "fon", "fnt", "pfb", "pfm", "ffil"]],
] as [IconName, string[]][]) {
  for (const e of exts) FILE_ICON_BY_EXT[e] = icon;
}

function fileIcon(name: string): IconName {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "file";
  // Disc filenames often carry a version suffix like "FOO.EXE;1".
  const ext = name.slice(dot + 1).toLowerCase().split(";")[0];
  return FILE_ICON_BY_EXT[ext] ?? "file";
}

// Icons drawn in currentColor rather than fixed colours: they inherit whatever
// they sit on, so the light-theme darkening below must leave them alone or it
// turns white glyphs grey against a coloured button.
const FOLLOWS_TEXT: IconName[] = ["calendar", "search", "export-list", "warning", "arrow-up", "play", "pause"];

function Icon({ name, className }: { name: IconName; className?: string }) {
  const classes = [
    "dx-icon",
    ...(FOLLOWS_TEXT.includes(name) ? ["dx-icon--follows-text"] : []),
    ...(className ? [className] : []),
  ].join(" ");
  return (
    <svg
      className={classes}
      viewBox="0 0 16 16"
      width="1em"
      height="1em"
      aria-hidden="true"
      focusable="false"
    >
      {ICON_PATHS[name]}
    </svg>
  );
}

// The filesystem to open a disc into. Path Table is an index rather than a tree
// — it lists directories and serves nothing below the root — so it is never
// where a disc should land, even on the rare disc that detects it first.
function firstBrowsableFs(detected: string[]): string {
  return detected.find((f) => f !== "Path Table") ?? detected[0] ?? "";
}

// The one filesystem to extract when the sidebar selection names one.
//
// "Path Table" is an index of the ISO 9660 tree rather than a tree of its own,
// and save_directory refuses it, telling the caller to use the ISO 9660 view.
// Someone who selects it and asks to extract means the files it indexes, so
// read those instead of failing. Everything else is taken at face value.
function scopedTarget(fs: string, detected: string[]): { name: string; pass: string } {
  if (fs !== "Path Table") return { name: fs, pass: fs };
  return distinctFilesystems(detected).find((t) => t.name === "ISO 9660")
    ?? { name: "ISO 9660", pass: "ISO 9660" };
}

interface CdTextNames {
  title?: string;
  performer?: string;
  songwriter?: string;
  composer?: string;
  arranger?: string;
  message?: string;
}

interface CdText {
  disc: CdTextNames;
  tracks: Record<string, CdTextNames>;
}

// Track titles come off the disc and go straight into filenames, so strip what
// a filesystem cannot take. Windows is the strict one; keeping to its rules
// means a rip is portable rather than only working where it was made.
function safeFileName(name: string): string {
  return name
    .replace(/[/\\:*?"<>|]/g, "-")
    // Control characters, and trailing dots or spaces, which Windows silently
    // drops and then cannot open.
    .replace(/[\x00-\x1f]/g, "")
    .replace(/[. ]+$/, "")
    .trim()
    .slice(0, 120);
}

interface AudioEntry {
  track_number: number;
  name: string;
  start_lba: number;
  num_sectors: number;
  size_bytes: number;
  format: string;
  is_data: boolean;
}

interface Ps3IsoInfo {
  is_ps3: boolean;
  encrypted: boolean;
  has_key: boolean;
  key_path: string | null;
}

interface WiiuConvInfo {
  is_wiiu: boolean;
  is_wux: boolean;  // compressed — repackage to raw .wud/.iso
  is_raw: boolean;  // raw (.wud/.iso) — can compress to .wux
  has_key: boolean; // sibling .key present (file-tree extraction available)
}

// Job kinds the runner dispatches on. "toiso"/"toraw"/"tocso" are the generic
// container conversions; the other three predate them and have their own
// commands because they do more than copy bytes.
// Job kinds handled by the one generic `convert_image` command, and the target
// each passes to it.
const CONVERT_TARGET: Partial<Record<ConvKind, string>> = {
  toiso: "raw", toraw: "raw", tocso: "cso", merge: "merge", split: "split", chdcue: "chdcue", chdsplit: "chdsplit",
};

type ConvKind = "ps3" | "wiiu" | "wux" | "toiso" | "toraw" | "tocso" | "merge" | "split" | "chdcue" | "chdsplit";

interface BatchItem {
  path: string;
  name: string;
  kind: ConvKind;
  op: string;
  encrypt: boolean;
  out_path: string;
  key_path: string;
  problem: string | null;
  conflict: boolean;
  out_size: number;
}

interface BatchPlan {
  items: BatchItem[];
  bytes_needed: number;
  free_space: number;
  conflicts: number;
  missing_keys: number;
}

interface ConvJob {
  kind: ConvKind;
  inPath: string;
  outPath: string;
  keyPath: string;
  encrypt: boolean;
  name: string;
  status: "pending" | "running" | "done" | "error";
  done: number;
  total: number;
  error?: string;
  verify?: boolean; // wux compression: run round-trip verification afterwards
}

interface DriveInfo {
  name: string;
  device_path: string;
  raw_device_path: string;
  has_disc: boolean;
  volume_name: string | null;
  mount_point: string | null;
}

type NodeType = "root" | "session" | "data_track" | "audio_track" | "filesystem" | "dir";
type ViewMode = "filesystem" | "audio" | "empty-drive";

interface TreeNode {
  name: string;
  path: string;
  nodeType: NodeType;
  children: TreeNode[] | null;
  expanded: boolean;
  /** Which filesystem a folder belongs to. A Mac/PC hybrid can carry the same
   *  path in HFS, ISO 9660 and Joliet, and the path alone cannot tell them
   *  apart — selecting one highlighted all three, and navigating used whichever
   *  filesystem happened to be active rather than the one clicked. */
  fs?: string;
}

interface TrackInfo {
  number: number;
  is_data: boolean;
  mode: string;
  start_lba: number;
  num_sectors: number;
  session: number;
  bin_path: string;
}

interface MountResult {
  mount_point: string;
  device: string;
}

interface EmulatedDrive {
  slot: string;
  device: string;
  image_path: string;
}

interface ColWidths {
  name: number;
  lba: number;
  size: number;
  modified: number;
  save: number;
}

function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

// Elapsed/total for the player bar. Plain m:ss — the frame precision that
// formatDuration gives a track listing is noise on a moving clock.
function fmtTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

function formatDuration(sectors: number): string {
  if (sectors === 0) return "—";
  const totalSeconds = Math.floor(sectors / 75);
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  const f = sectors % 75;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(f).padStart(2, "0")}`;
}

// Formats worth double-click previewing in the OS default app. Deliberately a
// whitelist: opening an executable would run it, not preview it.
const PREVIEW_EXTS = [
  // pictures
  "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "ico", "pcx", "tga", "svg", "heic", "psd",
  // video
  "mp4", "m4v", "mov", "avi", "mkv", "webm", "mpg", "mpeg", "m2v", "wmv", "flv", "ogv", "3gp", "vob",
  // audio
  "mp3", "wav", "flac", "ogg", "aac", "m4a", "wma", "aif", "aiff", "au", "mid", "midi", "voc", "mod", "xm", "s3m", "it",
  // documents
  "pdf", "txt", "rtf", "md", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub", "csv", "html", "htm", "xml", "json", "log", "ini", "cfg", "nfo",
  // fonts
  "ttf", "otf",
];

function isPreviewable(name: string): boolean {
  const dot = name.lastIndexOf(".");
  return dot >= 0 && PREVIEW_EXTS.includes(name.slice(dot + 1).toLowerCase());
}

// Self-contained single-file disc-image formats that can be opened in Disc
// Xplorer straight off another disc. Multi-file formats (cue/mds/ccd/gdi…)
// are excluded — their data lives in sibling files we'd have to extract too.
const NESTED_IMAGE_EXTS = ["iso", "img", "bin", "chd", "cdi", "nrg", "mdx", "wbfs", "cso", "ciso", "ecm", "uif", "wux", "wud", "gcz", "wua", "rvz", "wia", "zip", "tar", "tgz", "nds", "cab", "vpk", "fatx", "skeleton"];

function isNestedImage(name: string): boolean {
  const dot = name.lastIndexOf(".");
  return dot >= 0 && NESTED_IMAGE_EXTS.includes(name.slice(dot + 1).toLowerCase());
}

function isMountable(path: string, platform: string): boolean {
  const lower = path.toLowerCase();
  if (lower.endsWith(".iso") || lower.endsWith(".img")) return true;
  if (platform === "macos" && (lower.endsWith(".dmg") || lower.endsWith(".cdr"))) return true;
  if (platform === "linux" && (
    lower.endsWith(".cue") || lower.endsWith(".mds") || lower.endsWith(".mdx") ||
    lower.endsWith(".nrg") || lower.endsWith(".ccd") ||
    lower.endsWith(".toc") || lower.endsWith(".b5t") || lower.endsWith(".b6t") || lower.endsWith(".bwt") ||
    lower.endsWith(".c2d") || lower.endsWith(".pdi") || lower.endsWith(".gi") ||
    lower.endsWith(".daa")
  )) return true;
  return false;
}

// Ask each newly revealed folder whether it has subfolders of its own, so its
// twisty tells the truth instead of appearing on spec and vanishing when the
// folder turns out to be empty. A folder with none gets `children: []`, which is
// what distinguishes "known to be empty" from "not looked at yet" (null).
//
// This costs one listing per folder revealed, so it is skipped for levels wider
// than the cap — a couple of hundred round trips on every expand would be worse
// than an arrow that is briefly optimistic.
const SUBFOLDER_PROBE_LIMIT = 40;

async function probeSubfolders(imgPath: string, nodes: TreeNode[], showForks: boolean, filesystem: string | null): Promise<TreeNode[]> {
  const pending = nodes.filter((n) => n.nodeType === "dir" && n.children === null);
  if (pending.length === 0 || pending.length > SUBFOLDER_PROBE_LIMIT) return nodes;
  return Promise.all(nodes.map(async (n) => {
    if (n.nodeType !== "dir" || n.children !== null) return n;
    try {
      const r = await invoke<DiscEntry[]>("list_disc_contents", {
        imagePath: imgPath, dirPath: n.path, filesystem, showResourceForks: showForks,
      });
      return {
        ...n,
        children: r.filter((e) => e.is_dir).map((e): TreeNode => ({
          name: e.name,
          path: n.path === "/" ? `/${e.name}` : `${n.path}/${e.name}`,
          nodeType: "dir",
          children: null,
          expanded: false,
          fs: n.fs,
        })),
      };
    } catch {
      // Unreadable folder: leave it as unknown rather than claiming it is empty.
      return n;
    }
  }));
}

function TreeItem({
  node, imagePath, selectedPath, selectedFs, onSelect, onToggle, onNodeContextMenu, depth, volumeLabel,
}: {
  node: TreeNode; imagePath: string; selectedPath: string; selectedFs: string;
  onSelect: (path: string, fs?: string) => void; onToggle: (path: string, fs?: string) => void;
  onNodeContextMenu: (node: TreeNode, e: React.MouseEvent) => void; depth: number;
  volumeLabel: string;
}) {
  const isAudio = node.nodeType === "audio_track";
  const isSession = node.nodeType === "session";
  const isDataTrack = node.nodeType === "data_track";
  const isFilesystem = node.nodeType === "filesystem";

  const isPathTableEntry = node.path.startsWith("__pt_");
  const iconName: IconName = isSession || isDataTrack ? "disc-data"
    : isAudio ? "music"
    : isFilesystem ? (node.name === "Path Table" ? "index" : "filesystem")
    : node.nodeType === "dir" ? "folder"
    : "disc";
  const icon = <Icon name={iconName} />;

  const alwaysExpanded = isSession;
  const noArrow = isAudio || alwaysExpanded;

  function handleClick() {
    onSelect(node.path, node.fs);
  }

  // A folder whose children have not been listed yet might still have some, so
  // it gets an arrow on spec; once listed and found empty, the arrow goes away.
  // A filesystem gets a twisty once it has been opened and has something under
  // it — before that, clicking the node itself is what loads its tree, so an
  // optimistic twisty there would expand to nothing. Folders keep the optimistic
  // one, since listing them is exactly what expanding does.
  const canToggle = !noArrow && (isFilesystem
    ? node.children !== null && node.children.length > 0
    : node.children === null || node.children.length > 0);

  function handleArrowClick(e: React.MouseEvent) {
    // Toggling is not navigating: clicking the twisty must not also move the
    // file list, or closing a folder would immediately reopen it.
    e.stopPropagation();
    if (canToggle) onToggle(node.path, node.fs);
  }

  return (
    <div>
      <div
        className={[
          "tree-item",
          // A qualified folder also has to be in the filesystem being browsed, or
          // a hybrid disc's three /MUSIC folders light up together. Nodes with no
          // `fs` come from trees that have only one filesystem — a mounted image
          // or a physical disc — where the path alone is the identity, and
          // comparing against a stale activeFilesystem would stop them
          // highlighting at all.
          node.path === selectedPath && (node.fs === undefined || node.fs === selectedFs)
            ? "tree-item--selected" : "",
          isAudio ? "tree-item--audio" : "",
          isSession ? "tree-item--session" : "",
          isFilesystem ? "tree-item--filesystem" : "",
          isPathTableEntry ? "tree-item--index" : "",
        ].filter(Boolean).join(" ")}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={handleClick}
        onContextMenu={(e) => onNodeContextMenu(node, e)}
        title={isPathTableEntry ? `Path table entry — go to ${node.name} in ISO 9660` : undefined}
      >
        <span
          className={`tree-arrow${canToggle ? " tree-arrow--active" : ""}`}
          onClick={handleArrowClick}
          role={canToggle ? "button" : undefined}
          title={canToggle ? (node.expanded ? "Collapse" : "Expand") : undefined}
        >
          {canToggle ? (node.expanded ? "▼︎" : "▶︎") : " "}
        </span>
        <span className="tree-icon">{icon}</span>
        {/* The root node names the disc rather than the image file: the disc's own
            volume label when it has one, otherwise the file name. The file name is
            still shown in the path bar above the tree, so nothing is lost. */}
        <span className="tree-label" title={node.nodeType === "root" ? node.name : undefined}>
          {node.nodeType === "root" && volumeLabel ? volumeLabel : node.name}
        </span>
      </div>
      {(alwaysExpanded || node.expanded) && node.children && (
        <div>
          {node.children.map((child) => (
            <TreeItem key={child.path} node={child} imagePath={imagePath}
              selectedPath={selectedPath} selectedFs={selectedFs} onSelect={onSelect} onToggle={onToggle}
              onNodeContextMenu={onNodeContextMenu} depth={depth + 1} volumeLabel={volumeLabel} />
          ))}
        </div>
      )}
    </div>
  );
}

function App() {
  const [imagePath, setImagePath] = useState<string | null>(null);
  const [sourceImagePath, setSourceImagePath] = useState<string | null>(null);
  const [imageName, setImageName] = useState<string>("");
  const [currentPath, setCurrentPath] = useState("/");
  const [entries, setEntries] = useState<DiscEntry[]>([]);
  const [audioEntries, setAudioEntries] = useState<AudioEntry[]>([]);
  const [viewMode, setViewMode] = useState<ViewMode>("filesystem");
  const [emptyDriveName, setEmptyDriveName] = useState<string | null>(null);
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [cueTracks, setCueTracks] = useState<TrackInfo[]>([]);
  const [activeFilesystem, setActiveFilesystem] = useState<string>("");
  // Every filesystem on the disc, not just the one being browsed. A cue disc
  // with audio opens in audio view with no filesystem selected, so "does this
  // disc have files as well as tracks" cannot be answered from activeFilesystem.
  const [discFilesystems, setDiscFilesystems] = useState<string[]>([]);
  // CD-TEXT, when the disc carries it. Most do not, so this is usually empty.
  const [cdText, setCdText] = useState<CdText | null>(null);
  const [sidebarPath, setSidebarPath] = useState<string>("");
  // Contiguous LBA ranges (inclusive) of unreadable/missing sectors, for flagging
  // files located in damaged areas (e.g. partial dumps). Fetched async per image.
  const [damagedRanges, setDamagedRanges] = useState<[number, number][]>([]);
  const [damagedTotal, setDamagedTotal] = useState<number>(0);
  const [showDamagedReport, setShowDamagedReport] = useState(false);
  const [damagedFiles, setDamagedFiles] = useState<{ path: string; size: number; lba: number }[] | null>(null);
  // In-app audio playback: the WAV blob URL + which track it belongs to.
  const [audioUrl, setAudioUrl] = useState<string | null>(null);
  // Play straight into the next audio track when one ends, so a disc can be
  // listened to end-to-end like a CD player.
  const [autoAdvance, setAutoAdvance] = useState(
    () => localStorage.getItem("audioAutoAdvance") !== "false"
  );
  const [audioVolume, setAudioVolume] = useState(() => {
    const v = Number(localStorage.getItem("audioVolume"));
    return Number.isFinite(v) && v >= 0 && v <= 1 ? v : 1;
  });
  const audioElRef = useRef<HTMLAudioElement | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [audioPos, setAudioPos] = useState(0);
  const [audioDur, setAudioDur] = useState(0);
  const [playingTrack, setPlayingTrack] = useState<number | null>(null);
  const [audioLoading, setAudioLoading] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warn, setWarn] = useState<string | null>(null);
  const [statusText, setStatusText] = useState("No disc loaded");
  // The disc's own name (the label a CD shows when mounted). Belongs to the
  // filesystem, not the image file, so it changes with the selected view on a
  // hybrid disc — and is empty on discs that carry no label.
  const [volumeLabel, setVolumeLabel] = useState("");
  // 3DO discs only: whether the disc's RSA signature verifies against the retail
  // key. Empty for every other disc.
  const [signatureStatus, setSignatureStatus] = useState("");
  // Read from the bundle rather than hard-coded, so it can't drift from the
  // released version. Shown in the status bar and the window title.
  const [appVersion, setAppVersion] = useState("");
  // The brand in the status bar wears a green "$" in place of its last letter
  // until it has been followed once. Keyed by version, so a new release asks
  // again — and only once, rather than nagging every launch.
  const [supportSeen, setSupportSeen] = useState(true);
  const [mountedDevice, setMountedDevice] = useState<string | null>(null);
  const [physicalDiscActive, setPhysicalDiscActive] = useState(false);
  const [drives, setDrives] = useState<DriveInfo[]>([]);
  const [showDriveMenu, setShowDriveMenu] = useState(false);
  const [showDumpDriveMenu, setShowDumpDriveMenu] = useState(false);
  const [loadingDrives, setLoadingDrives] = useState(false);
  // Starting guesses only — measureColumns below replaces them with the real
  // width of the widest value each column can hold, in whatever font the platform
  // actually resolved. Hardcoded pixel widths cannot be right on macOS, Windows
  // and Linux at once, and guessing them from screenshots got Modified truncated
  // and Size bloated in turn.
  const [colWidths, setColWidths] = useState<ColWidths>({
    name: 280, lba: 76, size: 108, modified: 152,
    save: localStorage.getItem("showSelectBoxes") === "1" ? 60 : 34,
  });
  const [theme, setTheme] = useState<"system" | "light" | "dark">(() => {
    const stored = localStorage.getItem("theme") as "system" | "light" | "dark" | null;
    const t = stored || "system";
    if (t !== "system") document.documentElement.setAttribute("data-theme", t);
    return t;
  });
  const isDark = theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const appIcon = isDark ? iconDark : iconLight;
  const [showSettings, setShowSettings] = useState(false);
  const [showLicenses, setShowLicenses] = useState(false);
  const [audioFormat, setAudioFormat] = useState<"wav" | "flac" | "mp3">("wav");
  // Multi-select is a power feature: a checkbox on every row for something most
  // people never batch. Off by default, and the column narrows to just the save
  // arrow when it is off.
  const [showSelect, setShowSelect] = useState(
    () => localStorage.getItem("showSelectBoxes") === "1"
  );
  // HFS records no dependable encoding field, so detection can only guess from
  // the names themselves; this lets the user settle it when the guess is wrong.
  const [hfsEncoding, setHfsEncoding] = useState<"auto" | "roman" | "shift-jis">(
    () => (localStorage.getItem("hfsEncoding") as "auto" | "roman" | "shift-jis") || "auto"
  );
  // Gap handling follows Exact Audio Copy's three modes and its default, so a
  // rip from Disc Xplorer matches what people expect from a ripper.
  const [gapMode, setGapMode] = useState<"previous" | "next" | "leave-out">(
    () => (localStorage.getItem("audioGapMode") as "previous" | "next" | "leave-out") || "previous"
  );
  const [defaultDownloadPath, setDefaultDownloadPath] = useState<string>("");
  const [wiiuKeyPath, setWiiuKeyPath] = useState<string>("");
  const [redumperSource, setRedumperSource] = useState<"internal" | "external">("internal");
  const [redumperExternalPath, setRedumperExternalPath] = useState<string>("");
  const [redumperVersion, setRedumperVersion] = useState<string>("");
  const [showDumpModal, setShowDumpModal] = useState(false);
  const [dumpDrive, setDumpDrive] = useState<string>("");
  const [dumpOutputPath, setDumpOutputPath] = useState<string>("");
  const [dumpCreateSubfolder, setDumpCreateSubfolder] = useState(true);
  const [dumpSubfolder, setDumpSubfolder] = useState<string>("");
  const [dumpRunning, setDumpRunning] = useState(false);
  const [dumpLog, setDumpLog] = useState<string[]>([]);
  const dumpLogRef = useRef<HTMLDivElement>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const [ps3Info, setPs3Info] = useState<Ps3IsoInfo | null>(null);
  const [wiiuConvInfo, setWiiuConvInfo] = useState<WiiuConvInfo | null>(null);
  const [wiiuMenuOpen, setWiiuMenuOpen] = useState(false);
  const [wuxVerify, setWuxVerify] = useState(false);
  // Pending Wii U batch drop awaiting a target-format choice (null = no prompt).
  const [wiiuBatchPaths, setWiiuBatchPaths] = useState<string[] | null>(null);
  const [wiiuBatchVerify, setWiiuBatchVerify] = useState(false);
  const [showConvModal, setShowConvModal] = useState(false);
  // Batch conversion: folders, the plan the backend works out before anything
  // runs, and a log the user can hand back with a bug report.
  const [showBatch, setShowBatch] = useState(false);
  // The drag-drop listener is registered once, so it cannot read showBatch from
  // state without going stale. A ref keeps it current.
  const showBatchRef = useRef(false);
  const [batchDragOver, setBatchDragOver] = useState(false);
  // Holds the batch window today and the format conversions on the TODO, so the
  // toolbar does not grow a button per conversion.
  const [showTools, setShowTools] = useState(false);
  const toolsMenuRef = useRef<HTMLDivElement>(null);
  const [batchSrc, setBatchSrc] = useState(() => localStorage.getItem("batchSrc") || "");
  const [batchOut, setBatchOut] = useState(() => localStorage.getItem("batchOut") || "");
  const [batchKeys, setBatchKeys] = useState(() => localStorage.getItem("batchKeys") || "");
  const [batchRecursive, setBatchRecursive] = useState(true);
  const [batchConflict, setBatchConflict] = useState<"skip" | "rename" | "overwrite">("rename");
  const [batchTarget, setBatchTarget] = useState(() => localStorage.getItem("batchTarget") || "auto");
  const [batchPlan, setBatchPlan] = useState<BatchPlan | null>(null);
  const [batchScanning, setBatchScanning] = useState(false);
  const [batchError, setBatchError] = useState<string | null>(null);
  // The per-file log is kept for "Copy log", which is what a bug report needs,
  // but not shown: a folder of 200 images would fill the window with lines
  // nobody reads while it runs. What is shown is one line saying how it went.
  const [batchLog, setBatchLog] = useState<string[]>([]);
  const [batchSummary, setBatchSummary] = useState<{ text: string; failed: boolean } | null>(null);
  const [convJobs, setConvJobs] = useState<ConvJob[]>([]);
  const convListRef = useRef<HTMLDivElement>(null);
  const [convRunning, setConvRunning] = useState(false);
  const convCancelledRef = useRef(false);
  const [convCancelling, setConvCancelling] = useState(false);
  const [showExtractModal, setShowExtractModal] = useState(false);
  const [extractRunning, setExtractRunning] = useState(false);
  const [extractCancelling, setExtractCancelling] = useState(false);
  const [extractDone, setExtractDone] = useState(false);
  const [extractCancellable, setExtractCancellable] = useState(false);
  // Ripping a CD to FLAC takes long enough that a bare spinner reads as a hang,
  // so the audio pass names the track it is on. Blank for filesystem extraction,
  // which has no per-file reporting.
  const [extractStatus, setExtractStatus] = useState("");
  // Name of a just-saved zero-byte file, for the "empty by design" notice.
  const [emptyFileNotice, setEmptyFileNotice] = useState<string | null>(null);
  const [skipEmptyFileNotice, setSkipEmptyFileNotice] = useState(
    () => localStorage.getItem("skipEmptyFileNotice") === "true"
  );
  // "Latest Date" toolbar button (dates a disc: PVD + newest entry).
  const [latestDateEnabled, setLatestDateEnabled] = useState(
    () => localStorage.getItem("latestDateEnabled") === "true"
  );
  const [dateReport, setDateReport] = useState<DateReport | "loading" | null>(null);
  // Custom right-click menu ("Download"); replaces the webview default.
  const [ctxMenu, setCtxMenu] = useState<
    { x: number; y: number; items: { label: string; title?: string; run: () => void }[] } | null
  >(null);
  // How to write CD-XA streaming files. "ask" prompts the first time a extraction
  // actually contains some; picking "remember this choice" in that prompt stores the
  // mode here so it stops asking. Changeable in Settings, including back to "ask".
  const [xaDefault, setXaDefault] = useState<"ask" | 0 | 1 | 2>(() => {
    const v = localStorage.getItem("xaDefaultMode");
    return v === "0" || v === "1" || v === "2" ? (Number(v) as 0 | 1 | 2) : "ask";
  });
  // An extraction is waiting on that choice; holds what to run once it's made.
  const [xaPrompt, setXaPrompt] = useState<{ count: number; run: (mode: number) => void } | null>(null);
  const [xaRemember, setXaRemember] = useState(false);
  // Bulk-save selection (per current folder; keyed by entry name).
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // Stops the batch loop between items when the user cancels.
  const extractCancelRef = useRef(false);

  // How to handle Apple/Mac resource forks (ISO9660 associated files), IsoBuster-style.
  //  hide        — one row per file, forks dropped (default)
  //  list        — forks shown as separate ".[R]" rows
  //  appledouble — forks hidden from the list, but extraction writes ._NAME sidecars
  type ForkMode = "hide" | "list" | "appledouble";
  const [forkMode, setForkMode] = useState<ForkMode>(
    () => (localStorage.getItem("forkMode") as ForkMode) || "hide"
  );
  const [platform, setPlatform] = useState<string>("");
  const [showCdemuPrompt, setShowCdemuPrompt] = useState(false);
  const [cdemuInstalling, setCdemuInstalling] = useState(false);
  const [cdemuInstallMsg, setCdemuInstallMsg] = useState<string | null>(null);
  const [cdemuInstallOk, setCdemuInstallOk] = useState(false);
  const [emulatedDrives, setEmulatedDrives] = useState<EmulatedDrive[]>([]);
  const [emulating, setEmulating] = useState(false);
  const [svParams, setSvParams] = useState<{ imagePath: string; lba: number; compareImagePath?: string | null } | null>(null);

  useEffect(() => {
    if (!IS_SECTOR_VIEW_WINDOW) return;
    invoke<{ image_path: string; lba: number; compare_image_path: string | null } | null>("claim_sector_view_params").then(p => {
      if (p) setSvParams({ imagePath: p.image_path, lba: p.lba, compareImagePath: p.compare_image_path });
    });
  }, []);

  const dragRef = useRef<{ col: keyof ColWidths; startX: number; startWidth: number } | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState<number>(() => {
    const stored = Number(localStorage.getItem("sidebarWidth"));
    return Number.isFinite(stored) && stored >= 140 ? stored : 220;
  });
  const contentRef = useRef<HTMLDivElement>(null);
  const sidebarDragRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const headWrapRef = useRef<HTMLDivElement>(null);
  const driveMenuRef = useRef<HTMLDivElement>(null);
  const dumpDriveMenuRef = useRef<HTMLDivElement>(null);
  const settingsRef = useRef<HTMLDivElement>(null);
  const settingsGearRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    invoke<string>("get_platform").then(setPlatform);
  }, []);

  // Selection is per-folder: any change to the listing clears it.
  useEffect(() => {
    setSelected(new Set());
  }, [entries]);

  // Suppress the webview's default context menu everywhere; our own menu is
  // attached where right-click has meaning (tree nodes, file rows).
  useEffect(() => {
    const block = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", block);
    return () => document.removeEventListener("contextmenu", block);
  }, []);

  // Show the running version in the title bar. Read from the bundle rather than
  // hard-coded, so it can't drift from the released version. Needs the
  // core:window:allow-set-title capability — the default window permission set is
  // read-only, and without it this call is rejected.
  useEffect(() => {
    if (IS_SECTOR_VIEW_WINDOW) return;
    getVersion()
      .then((v) => {
        setAppVersion(v);
        setSupportSeen(localStorage.getItem(`supportSeen_${v}`) === "1");
      })
      .catch((err) => console.warn("Could not read app version:", err));
  }, []);

  // CD-TEXT for the loaded disc. Most discs carry none, so an empty result is
  // the norm and simply leaves tracks named "Track NN".
  useEffect(() => {
    if (!imagePath) { setCdText(null); return; }
    let stale = false;
    const numbers = cueTracks.map((t) => t.number).filter((n) => n <= 255);
    const lastTrack = numbers.length ? Math.max(...numbers) : null;
    invoke<CdText>("disc_cdtext", { imagePath, lastTrack, fromDrive: physicalDiscActive })
      .then((t) => {
        if (stale) return;
        const any = t && (Object.keys(t.tracks ?? {}).length > 0 || !!t.disc?.title);
        setCdText(any ? t : null);
      })
      .catch(() => { if (!stale) setCdText(null); });
    return () => { stale = true; };
  }, [imagePath, cueTracks, physicalDiscActive]);

  // Opened from the OS: double-clicking an associated disc image, or "Open with".
  // The launch path is collected here rather than pushed from Rust because the
  // window may not exist yet when the process starts; later opens (a second
  // double-click while we're already running) arrive as an event instead.
  useEffect(() => {
    if (IS_SECTOR_VIEW_WINDOW) return;
    let cancelled = false;
    invoke<string | null>("take_pending_open")
      .then((path) => { if (path && !cancelled) openImageAtPath(path); })
      .catch(() => {});
    const unlisten = listen<string>("open-disc-image", (e) => {
      if (e.payload) openImageAtPath(e.payload);
    });
    return () => { cancelled = true; unlisten.then((f) => f()).catch(() => {}); };
  }, []);

  useEffect(() => {
    if (xaDefault === "ask") localStorage.removeItem("xaDefaultMode");
    else localStorage.setItem("xaDefaultMode", String(xaDefault));
  }, [xaDefault]);

  useEffect(() => {
    localStorage.setItem("audioAutoAdvance", String(autoAdvance));
  }, [autoAdvance]);

  // The element is rebuilt for each track (keyed on the blob URL), so the volume
  // has to be reapplied rather than set once.
  useEffect(() => {
    localStorage.setItem("audioVolume", String(audioVolume));
    if (audioElRef.current) audioElRef.current.volume = audioVolume;
  }, [audioVolume, audioUrl]);

  useEffect(() => {
    if (!imagePath) { setSignatureStatus(""); return; }
    invoke<string>("threedo_signature_status", { imagePath, filesystem: activeFilesystem || null })
      .then(setSignatureStatus)
      .catch(() => setSignatureStatus(""));
  }, [imagePath, activeFilesystem]);

  useEffect(() => {
    if (!imagePath) { setVolumeLabel(""); return; }
    invoke<string>("disc_volume_label", { imagePath, filesystem: activeFilesystem || null })
      .then(setVolumeLabel)
      .catch(() => setVolumeLabel(""));
  }, [imagePath, activeFilesystem]);

  useEffect(() => {
    localStorage.setItem("audioGapMode", gapMode);
  }, [gapMode]);

  // Keep a ref so directory-listing/extraction callbacks read the current value
  // without being recreated; persist and reload the current directory on change.
  const forkModeRef = useRef(forkMode);
  useEffect(() => {
    forkModeRef.current = forkMode;
    localStorage.setItem("forkMode", forkMode);
    if (imagePath && viewMode === "filesystem") loadDirectory(imagePath, currentPath);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [forkMode]);

  useEffect(() => {
    localStorage.setItem("showSelectBoxes", showSelect ? "1" : "0");
    // The table is width:100% with table-layout:fixed, so the pixels the column
    // gives up are redistributed across the others — no gap where the boxes were.
    setColWidths((w) => ({ ...w, save: showSelect ? 60 : 34 }));
    // A selection left behind would keep driving "Save Selected" from a column
    // that is no longer on screen.
    if (!showSelect) setSelected(new Set());
  }, [showSelect]);

  // Fit LBA, Size and Modified to what the current listing actually holds,
  // rather than to the widest value the format allows. Sizing for a
  // hypothetical 4 GB file leaves a wide, mostly empty column on a disc whose
  // files are five digits long, and every spare pixel in a left-aligned column
  // piles up as a gap before the download arrow.
  //
  // Measuring beats arithmetic here: it follows whatever font the platform
  // resolved and whatever text scaling is in effect, neither of which a
  // hardcoded pixel width can know. Only the longest string is measured — for
  // digits and fixed-format dates, longest is widest — so this costs two
  // measurements, not one per row.
  useEffect(() => {
    const PAD = 24;      // .file-table td padding, 12px each side
    const HEADROOM = 6;  // so a slightly wider glyph never trips the ellipsis
    const probe = document.createElement("span");
    probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre;top:-9999px;left:-9999px";
    probe.style.font = `11px ${getComputedStyle(document.body).fontFamily}`;
    document.body.appendChild(probe);
    const measure = (t: string) => { probe.textContent = t; return probe.getBoundingClientRect().width; };
    const longest = (vals: string[]) => vals.reduce((a, b) => (b.length > a.length ? b : a), "");
    const fit = (vals: string[], header: string) =>
      Math.ceil(Math.max(measure(longest(vals)), measure(header)) + PAD + HEADROOM);

    const next = viewMode === "audio"
      ? {
          lba: fit(audioEntries.map((e) => e.start_lba.toLocaleString()), "Start Sector"),
          size: fit(audioEntries.map((e) => (e.is_data ? formatSize(e.size_bytes) : formatDuration(e.num_sectors))), "Duration"),
          modified: fit(audioEntries.map((e) => e.format), "Format"),
        }
      : {
          lba: fit(entries.map((e) => ((e.is_dir && e.lba === 0) || (!e.is_dir && e.size_bytes === 0) ? "—" : String(e.lba))), "LBA"),
          size: fit(entries.map((e) => (e.is_dir ? "—" : e.size_bytes.toLocaleString())), "Size"),
          modified: fit(entries.map((e) => e.modified), "Modified"),
        };
    probe.remove();
    setColWidths((c) => ({ ...c, ...next }));
  }, [entries, audioEntries, viewMode]);

  // The header sits outside the scrolling area so the scrollbar runs beside the
  // rows alone. The cost is that it no longer moves when the rows scroll
  // sideways, so mirror the body's horizontal offset onto it. Columns fit their
  // contents, so this rarely comes up — but a narrow window makes it possible.
  useEffect(() => {
    const body = contentRef.current;
    if (!body) return;
    const sync = () => {
      const head = headWrapRef.current;
      if (head) head.scrollLeft = body.scrollLeft;
    };
    body.addEventListener("scroll", sync, { passive: true });
    return () => body.removeEventListener("scroll", sync);
  }, []);

  // Push the encoding choice to the backend and re-read the listing, so a
  // correction shows immediately rather than after reopening the disc.
  useEffect(() => {
    const mode = hfsEncoding === "roman" ? 1 : hfsEncoding === "shift-jis" ? 2 : 0;
    localStorage.setItem("hfsEncoding", hfsEncoding);
    invoke("set_hfs_encoding", { mode })
      .then(() => {
        if (imagePath && viewMode === "filesystem") loadDirectory(imagePath, currentPath);
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hfsEncoding]);

  useEffect(() => {
    localStorage.setItem("theme", theme);
    if (theme === "system") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", theme);
    }
    const tauriTheme = theme === "light" ? "light" : theme === "dark" ? "dark" : null;
    getCurrentWindow().setTheme(tauriTheme).catch(() => {});
  }, [theme]);

  useEffect(() => {
    if (showSettings && !redumperVersion) {
      fetchRedumperVersion(redumperSource, redumperExternalPath);
    }
  }, [showSettings]);

  useEffect(() => {
    if (platform !== "linux") return;
    invoke<boolean>("check_cdemu_installed").then(installed => {
      if (!installed) setShowCdemuPrompt(true);
    });
  }, [platform]);

  useEffect(() => {
    function handleOutsideClick(e: MouseEvent) {
      if (driveMenuRef.current && !driveMenuRef.current.contains(e.target as Node)) {
        setShowDriveMenu(false);
      }
    }
    if (showDriveMenu) document.addEventListener("mousedown", handleOutsideClick);
    return () => document.removeEventListener("mousedown", handleOutsideClick);
  }, [showDriveMenu]);

  useEffect(() => {
    function handleOutsideClick(e: MouseEvent) {
      if (toolsMenuRef.current && !toolsMenuRef.current.contains(e.target as Node)) {
        setShowTools(false);
      }
    }
    function handleEscape(e: KeyboardEvent) {
      if (e.key === "Escape") setShowTools(false);
    }
    if (showTools) {
      document.addEventListener("mousedown", handleOutsideClick);
      document.addEventListener("keydown", handleEscape);
    }
    return () => {
      document.removeEventListener("mousedown", handleOutsideClick);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [showTools]);

  useEffect(() => {
    function handleOutsideClick(e: MouseEvent) {
      if (dumpDriveMenuRef.current && !dumpDriveMenuRef.current.contains(e.target as Node)) {
        setShowDumpDriveMenu(false);
      }
    }
    if (showDumpDriveMenu) document.addEventListener("mousedown", handleOutsideClick);
    return () => document.removeEventListener("mousedown", handleOutsideClick);
  }, [showDumpDriveMenu]);

  useEffect(() => {
    function handleOutsideClick(e: MouseEvent) {
      if (
        settingsRef.current && !settingsRef.current.contains(e.target as Node) &&
        settingsGearRef.current && !settingsGearRef.current.contains(e.target as Node)
      ) {
        setShowSettings(false);
      }
    }
    if (showSettings) document.addEventListener("mousedown", handleOutsideClick);
    return () => document.removeEventListener("mousedown", handleOutsideClick);
  }, [showSettings]);

  async function installCdemu() {
    setCdemuInstalling(true);
    setCdemuInstallMsg(null);
    try {
      const msg = await invoke<string>("install_cdemu");
      setCdemuInstallMsg(msg);
      setCdemuInstallOk(true);
    } catch (e) {
      setCdemuInstallMsg(String(e));
      setCdemuInstallOk(false);
    } finally {
      setCdemuInstalling(false);
    }
  }

  async function pickDownloadLocation() {
    const dir = await open({ directory: true, title: "Set Default Download Location" });
    if (dir) setDefaultDownloadPath(dir as string);
  }

  async function pickWiiuKey() {
    const file = await open({ filters: [{ name: "Key file", extensions: ["key"] }], title: "Select Wii U Common Key File" });
    if (file) {
      const path = file as string;
      setWiiuKeyPath(path);
      invoke("set_wiiu_key_path", { path });
    }
  }

  // Clear the "no Wii U common key" warning as soon as a key is set, via any path.
  useEffect(() => {
    if (wiiuKeyPath) {
      setWarn(w => (w && w.includes("Wii U common key")) ? null : w);
    }
  }, [wiiuKeyPath]);

  async function fetchRedumperVersion(source: string, externalPath: string) {
    // Internal binary's build is known at compile time — no need to probe it.
    if (source === "internal") {
      setRedumperVersion(REDUMPER_INTERNAL_VERSION);
      return;
    }
    setRedumperVersion("Checking…");
    try {
      const v = await invoke<string>("get_redumper_version", {
        source,
        externalPath: externalPath || null,
      });
      setRedumperVersion(v);
    } catch (e) {
      setRedumperVersion(String(e));
    }
  }

  async function pickRedumperExternal() {
    const file = await open({ title: "Select redumper binary" });
    if (file) {
      const path = file as string;
      setRedumperExternalPath(path);
      fetchRedumperVersion("external", path);
    }
  }

  function handleRedumperSourceChange(src: "internal" | "external") {
    setRedumperSource(src);
    fetchRedumperVersion(src, src === "internal" ? "" : redumperExternalPath);
  }

  async function pickDumpOutput() {
    const dir = await open({ directory: true, title: "Choose dump output folder" });
    if (dir) setDumpOutputPath(dir as string);
  }

  async function startDump() {
    if (!dumpDrive || !dumpOutputPath) return;
    const effectivePath = dumpCreateSubfolder && dumpSubfolder
      ? `${dumpOutputPath}/${dumpSubfolder}`
      : dumpOutputPath;
    setDumpRunning(true);
    setDumpLog([]);
    const isProgress = (s: string) => /^\|\s*\[/.test(s) || /\d+\s*\/\s*\d+/.test(s);
    const unlistenLog = await listen<string>("redumper-log", (e) => {
      const line = e.payload.replace(/\r/g, "");
      if (!line) return;
      setDumpLog(prev => {
        const last = prev[prev.length - 1] ?? "";
        if (isProgress(line) && isProgress(last)) return [...prev.slice(0, -1), line];
        return [...prev, line];
      });
      setTimeout(() => { dumpLogRef.current?.scrollTo(0, dumpLogRef.current.scrollHeight); }, 0);
    });
    const unlistenDone = await listen<number>("redumper-done", async (e) => {
      const code = e.payload;
      if (code === 0) {
        try {
          await invoke("organize_dump_logs", { dir: effectivePath });
        } catch { /* non-fatal */ }
      }
      setDumpLog(prev => [...prev, code === 0 ? "\nCompleted successfully." : `\nFailed (exit code ${code})`]);
      setDumpRunning(false);
      unlistenLog();
      unlistenDone();
    });
    try {
      await invoke("start_redumper_dump", {
        drive: dumpDrive,
        outputPath: effectivePath,
        source: redumperSource,
        externalPath: redumperExternalPath || null,
      });
    } catch (e) {
      setDumpLog(prev => [...prev, `Error: ${String(e)}`]);
      setDumpRunning(false);
      unlistenLog();
      unlistenDone();
    }
  }

  async function cancelDump() {
    try { await invoke("cancel_redumper_dump"); } catch { /* ignore */ }
    setDumpRunning(false);
  }

  // Drag the line between the tree and the file list. Clamped so neither side
  // can be squeezed to nothing, and remembered for next launch.
  function onSidebarResizeStart(e: React.MouseEvent) {
    e.preventDefault();
    sidebarDragRef.current = { startX: e.clientX, startWidth: sidebarWidth };
    document.body.style.cursor = "col-resize";
    function onMove(ev: MouseEvent) {
      const d = sidebarDragRef.current;
      if (!d) return;
      const max = Math.max(200, window.innerWidth - 320);
      setSidebarWidth(Math.min(max, Math.max(140, d.startWidth + ev.clientX - d.startX)));
    }
    function onUp() {
      sidebarDragRef.current = null;
      document.body.style.cursor = "";
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  useEffect(() => {
    localStorage.setItem("sidebarWidth", String(sidebarWidth));
  }, [sidebarWidth]);

  function onResizeStart(col: keyof ColWidths, e: React.MouseEvent) {
    e.preventDefault();
    dragRef.current = { col, startX: e.clientX, startWidth: colWidths[col] };
    function onMove(e: MouseEvent) {
      if (!dragRef.current) return;
      const delta = e.clientX - dragRef.current.startX;
      setColWidths((prev) => ({
        ...prev,
        [dragRef.current!.col]: Math.max(48, dragRef.current!.startWidth + delta),
      }));
    }
    function onUp() {
      dragRef.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  const navIdRef = useRef(0);

  // Fetch the damaged-sector map whenever the open image changes (async; the red-X
  // overlay appears once it resolves). Backend returns [] for healthy/non-raw images.
  useEffect(() => {
    if (!imagePath) { setDamagedRanges([]); setDamagedTotal(0); return; }
    let cancelled = false;
    invoke<{ total_sectors: number; ranges: [number, number][] }>("disc_damaged_lba_ranges", { imagePath })
      .then((r) => { if (!cancelled) { setDamagedRanges(r.ranges); setDamagedTotal(r.total_sectors); } })
      .catch(() => { if (!cancelled) { setDamagedRanges([]); setDamagedTotal(0); } });
    return () => { cancelled = true; };
  }, [imagePath]);

  // The damaged-sector map is keyed on absolute disc-sector LBAs. Only the ISO 9660
  // family exposes `entry.lba` as a real disc sector — HFS reports a CNID, UDF a
  // partition-relative block, etc. Comparing those against the sector map is
  // meaningless and produces false positives, so restrict the check to ISO views.
  // ("" = single-filesystem image, which is always ISO 9660.)
  const SECTOR_ADDRESSED_FS = new Set(["", "ISO 9660", "Joliet", "Rock Ridge", "El Torito", "Path Table"]);

  // A file is "damaged" if its sector extent overlaps any missing-sector range.
  function isDamaged(entry: DiscEntry): boolean {
    if (entry.is_dir || entry.lba <= 0 || damagedTotal <= 0) return false;
    if (!SECTOR_ADDRESSED_FS.has(activeFilesystem)) return false;
    // Zero-length files can't be damaged — nothing is ever read. Mastering
    // tools also leave junk in the extent field of 0-byte entries (nothing
    // dereferences it), so the LBA checks below would false-positive.
    if (entry.size_bytes === 0) return false;
    // Starts past the end of the dumped data — an interrupted/truncated dump
    // stops mid-disc, so a file whose extent begins beyond it is missing. Uses the
    // exact start LBA (a complete image never has a file starting past its own end),
    // so this can't false-positive the way an inflated end estimate would.
    if (entry.lba >= damagedTotal) return true;
    const sectors = Math.max(1, Math.ceil(entry.size_bytes / 2048));
    const end = entry.lba + sectors - 1;
    // Extent runs past the dumped end — a truncated dump cuts the file's tail.
    // Gated on the image showing other damage, so a complete disc where an
    // XA-inflated sector estimate overshoots the end can't false-flag.
    if (damagedRanges.length > 0 && end >= damagedTotal) return true;
    // Overlaps a missing-sector gap within the dumped range.
    for (const [s, e] of damagedRanges) {
      if (s > end) break;
      if (e >= entry.lba) return true;
    }
    return false;
  }

  // Bucket the damage map into `n` segments for a compact good/bad visualization.
  function damageBuckets(n: number): boolean[] {
    const buckets = new Array(n).fill(false);
    if (damagedTotal <= 0) return buckets;
    for (const [s, e] of damagedRanges) {
      const b0 = Math.floor((s / damagedTotal) * n);
      const b1 = Math.min(n - 1, Math.floor((e / damagedTotal) * n));
      for (let b = Math.max(0, b0); b <= b1; b++) buckets[b] = true;
    }
    return buckets;
  }

  // Walk the whole disc and collect every file that overlaps a damaged sector.
  async function buildDamagedReport() {
    if (!imagePath) return;
    setShowDamagedReport(true);
    setDamagedFiles(null);
    const fsName = activeFilesystem || null;
    const found: { path: string; size: number; lba: number }[] = [];
    const walk = async (dir: string, depth: number): Promise<void> => {
      if (depth > 64) return;
      let entries: DiscEntry[];
      try {
        entries = await invoke<DiscEntry[]>("list_disc_contents", { imagePath, dirPath: dir, filesystem: fsName, showResourceForks: forkModeRef.current === "list" });
      } catch { return; }
      for (const e of entries) {
        const p = dir === "/" ? `/${e.name}` : `${dir}/${e.name}`;
        if (e.is_dir) await walk(p, depth + 1);
        else if (isDamaged(e)) found.push({ path: p, size: e.size_bytes, lba: e.lba });
      }
    };
    await walk("/", 0);
    found.sort((a, b) => a.lba - b.lba);
    setDamagedFiles(found);
  }

  // Export a catalog of the whole disc. Format is inferred from the chosen file
  // extension (.csv / .json / .xml[DFXML] / .txt).
  async function exportFileList() {
    if (!imagePath) return;
    const dest = await save({
      defaultPath: `${imageName || "disc"}_filelist.csv`,
      filters: [
        { name: "CSV", extensions: ["csv"] },
        { name: "JSON", extensions: ["json"] },
        { name: "Text", extensions: ["txt"] },
        { name: "DFXML", extensions: ["xml"] },
      ],
    });
    if (!dest || typeof dest !== "string") return;
    const fsName = activeFilesystem || null;
    type Row = { path: string; type: "dir" | "file"; size: number; lba: number; modified: string };
    const rows: Row[] = [];
    const walk = async (dir: string, depth: number): Promise<void> => {
      if (depth > 64) return;
      let entries: DiscEntry[];
      try {
        entries = await invoke<DiscEntry[]>("list_disc_contents", { imagePath, dirPath: dir, filesystem: fsName, showResourceForks: forkModeRef.current === "list" });
      } catch { return; }
      for (const e of entries) {
        const p = dir === "/" ? `/${e.name}` : `${dir}/${e.name}`;
        rows.push({ path: p, type: e.is_dir ? "dir" : "file", size: e.is_dir ? 0 : e.size_bytes, lba: e.lba, modified: e.modified });
        if (e.is_dir) await walk(p, depth + 1);
      }
    };
    await walk("/", 0);

    const ext = dest.split(".").pop()?.toLowerCase();
    const xmlEsc = (s: string) => s.replace(/[<>&'"]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", "'": "&apos;", '"': "&quot;" }[c]!));
    const csvCell = (s: string) => /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    let content: string;
    if (ext === "json") {
      content = JSON.stringify({ image: imageName, filesystem: activeFilesystem, files: rows }, null, 2);
    } else if (ext === "xml") {
      content = `<?xml version="1.0" encoding="UTF-8"?>\n<dfxml version="1.2.0">\n  <source><image_filename>${xmlEsc(imageName)}</image_filename></source>\n  <volume>\n` +
        rows.filter((r) => r.type === "file").map((r) =>
          `    <fileobject>\n      <filename>${xmlEsc(r.path)}</filename>\n      <filesize>${r.size}</filesize>\n      <mtime>${xmlEsc(r.modified)}</mtime>\n    </fileobject>\n`).join("") +
        `  </volume>\n</dfxml>\n`;
    } else if (ext === "txt") {
      content = rows.map((r) => `${r.path}${r.type === "dir" ? "/" : ""}\t${r.type === "file" ? r.size : ""}\t${r.lba}\t${r.modified}`).join("\n") + "\n";
    } else {
      content = "path,type,size,lba,modified\n" + rows.map((r) => [r.path, r.type, String(r.size), String(r.lba), r.modified].map(csvCell).join(",")).join("\n") + "\n";
    }
    try {
      await invoke("write_text_file", { destPath: dest, content });
    } catch (e) { setError(String(e)); }
  }

  // Reveal the current location in the sidebar tree: expand the active filesystem
  // node and the chain of folders down to `dirPath`, and highlight the current
  // folder (or the filesystem node itself at the root). Driven from loadDirectory
  // so it stays in sync no matter how the user navigated (tree, list, breadcrumb,
  // Up). `currentSubdirs` is the already-loaded listing of `dirPath`, reused to
  // avoid re-fetching the deepest level.
  const syncSidebarTree = useCallback(async (
    imgPath: string, dirPath: string, fsName: string, myId: number, currentSubdirs: DiscEntry[],
  ) => {
    const fsPath = fsName ? `__fs_${fsName.toLowerCase().replace(/ /g, "_")}` : "";
    const segs = dirPath.split("/").filter(Boolean);

    const listSubdirNames = async (dp: string): Promise<string[]> => {
      try {
        const r = await invoke<DiscEntry[]>("list_disc_contents", { imagePath: imgPath, dirPath: dp, filesystem: fsName || null, showResourceForks: forkModeRef.current === "list" });
        return r.filter((e) => e.is_dir).map((e) => e.name);
      } catch { return []; }
    };

    const buildLevel = async (parentPath: string, depth: number, names: string[]): Promise<TreeNode[]> =>
      Promise.all(names.map(async (nm): Promise<TreeNode> => {
        const nodePath = parentPath === "/" ? `/${nm}` : `${parentPath}/${nm}`;
        const onPath = depth < segs.length && segs[depth] === nm;
        if (!onPath) return { name: nm, path: nodePath, nodeType: "dir", children: null, expanded: false, fs: fsName };
        if (depth + 1 === segs.length) {
          // This node is the current folder: show its subdirs (collapsed).
          const kids = currentSubdirs.filter((e) => e.is_dir)
            .map((e): TreeNode => ({ name: e.name, path: `${nodePath}/${e.name}`, nodeType: "dir", children: null, expanded: false, fs: fsName }));
          // `kids` may be empty, and that is worth recording: [] says "looked,
          // nothing there" where null would say "not looked at yet" and earn the
          // folder a twisty it does not deserve.
          return { name: nm, path: nodePath, nodeType: "dir", children: kids, expanded: kids.length > 0, fs: fsName };
        }
        // On-path ancestor: recurse.
        const children = await buildLevel(nodePath, depth + 1, await listSubdirNames(nodePath));
        return { name: nm, path: nodePath, nodeType: "dir", children, expanded: true, fs: fsName };
      }));

    // The Path Table is an index of every directory on the disc, listed as full
    // paths — not a hierarchy, and the backend serves it only at the root. Its
    // entries appear in the tree as flat leaves: no twisty, since they have
    // nothing beneath them, and a `__pt_` path so selecting one highlights it
    // without navigating into a listing that would come back empty. Built as
    // real folders instead, the "/" entry expanded into a copy of the whole
    // index, and again inside that, forever.
    const rootNames = fsName === "Path Table"
      ? []
      : segs.length === 0
        ? currentSubdirs.filter((e) => e.is_dir).map((e) => e.name)
        : await listSubdirNames("/");
    const topChildren = fsName === "Path Table"
      ? currentSubdirs.filter((e) => e.is_dir).map((e): TreeNode => ({
          name: e.name,
          path: `__pt_${e.name}`,
          nodeType: "dir",
          children: [],
          expanded: false,
          fs: fsName,
        }))
      : await probeSubfolders(
          imgPath, await buildLevel("/", 0, rootNames), forkModeRef.current === "list", fsName || null);
    if (navIdRef.current !== myId) return;

    setSidebarPath(segs.length === 0 ? fsPath : `/${segs.join("/")}`);
    if (!fsPath) return;

    // The tree above is rebuilt from scratch around wherever we just navigated,
    // which knows nothing about folders the user had opened elsewhere. Carry
    // their state over: a folder someone opened stays open until they close it,
    // rather than collapsing the moment they look at something else.
    const keepOpen = (next: TreeNode[], prev: TreeNode[] | null): TreeNode[] => {
      if (!prev) return next;
      const before = new Map(prev.map((n) => [n.path, n]));
      return next.map((n) => {
        const was = before.get(n.path);
        if (!was) return n;
        if (!n.expanded && was.expanded && was.children) {
          return { ...n, expanded: true, children: was.children };
        }
        return n.children ? { ...n, children: keepOpen(n.children, was.children) } : n;
      });
    };

    setTree((prev) => {
      let found = false;
      const swap = (nodes: TreeNode[]): TreeNode[] => nodes.map((n) => {
        if (n.nodeType === "filesystem") {
          if (n.path === fsPath) {
            found = true;
            return { ...n, expanded: true, children: keepOpen(topChildren, n.children) };
          }
          // Another filesystem the user had open is left alone rather than
          // being folded shut behind them.
          return n;
        }
        if (n.children) {
          // Open the way down to whichever filesystem we just moved into. The
          // filesystem node expands itself above, but a collapsed session or
          // data track above it still hides the whole thing — so navigating
          // looked like nothing had happened even though the listing had
          // already changed.
          const wasFound = found;
          const children = swap(n.children);
          const revealed = found && !wasFound;
          return { ...n, children, expanded: revealed ? true : n.expanded };
        }
        return n;
      });
      const next = swap(prev);
      return found ? next : prev;
    });
  }, []);

  const loadDirectory = useCallback(async (imgPath: string, dirPath: string, fsLabel = "", filesystem?: string) => {
    const myId = ++navIdRef.current;
    setError(null);
    const effectiveFs = filesystem !== undefined ? filesystem : activeFilesystem;
    if (filesystem !== undefined) setActiveFilesystem(filesystem);
    try {
      const result = await invoke<DiscEntry[]>("list_disc_contents", {
        imagePath: imgPath,
        dirPath,
        filesystem: effectiveFs || null,
        showResourceForks: forkModeRef.current === "list",
      });
      if (navIdRef.current !== myId) return;
      const sorted = result.sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setEntries(sorted);
      setAudioEntries([]);
      setViewMode("filesystem");
      setCurrentPath(dirPath);
      const dirs = sorted.filter((e) => e.is_dir).length;
      const files = sorted.filter((e) => !e.is_dir).length;
      setStatusText(`${dirs} folder${dirs !== 1 ? "s" : ""}, ${files} file${files !== 1 ? "s" : ""}${fsLabel ? ` · ${fsLabel}` : ""}`);
      syncSidebarTree(imgPath, dirPath, effectiveFs, myId, sorted);
    } catch (e) {
      if (navIdRef.current !== myId) return;
      setError(String(e));
    }
  }, [activeFilesystem, syncSidebarTree]);

  // A track's name from CD-TEXT, or null when the disc carries none. Looked up
  // rather than baked into AudioEntry, since CD-TEXT arrives after the track
  // list is built and there should be one source of truth for the name.
  function cdTextTitle(track: number): string | null {
    const t = cdText?.tracks?.[String(track)]?.title;
    return t && t.trim() ? t.trim() : null;
  }

  // What a ripped file is called: "03 - Eat for Two" when the disc says so,
  // otherwise the plain "Track 03" that extraction has always used.
  function trackFileName(track: number): string {
    const num = String(track).padStart(2, "0");
    const title = cdTextTitle(track);
    return title ? safeFileName(`${num} - ${title}`) : `Track ${num}`;
  }

  function buildAudioEntries(tracks: TrackInfo[]): AudioEntry[] {
    return tracks.map((t) => ({
      track_number: t.number,
      name: `Track ${String(t.number).padStart(2, "0")}`,
      start_lba: t.start_lba,
      num_sectors: t.num_sectors,
      size_bytes: t.is_data ? t.num_sectors * 2048 : t.num_sectors * 2352,
      format: t.is_data ? t.mode : "CD Audio",
      is_data: t.is_data,
    }));
  }

  function dirOf(p: string): string {
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    return i >= 0 ? p.slice(0, i) : "";
  }

  // Output path for a converted image. When the destination folder differs from
  // the source folder there's no name collision, so the " (encrypted)/(decrypted)"
  // suffix is dropped; writing back into the same folder keeps the suffix.
  function convOutPath(inPath: string, outDir: string, encrypt: boolean): string {
    const file = inPath.slice(Math.max(inPath.lastIndexOf("/"), inPath.lastIndexOf("\\")) + 1);
    const dot = file.lastIndexOf(".");
    const stem = dot >= 0 ? file.slice(0, dot) : file;
    const ext = dot >= 0 ? file.slice(dot) : "";
    const out = outDir.replace(/[/\\]+$/, "");
    const sep = out.includes("\\") || inPath.includes("\\") ? "\\" : "/";
    const suffix = out === dirOf(inPath) ? (encrypt ? " (encrypted)" : " (decrypted)") : "";
    return `${out}${sep}${stem}${suffix}${ext}`;
  }

  // Build conversion jobs for the given images + keys, writing to `outDir`.
  // Currently handles PS3 ISOs (.iso + .ird/.dkey/.key); other key-based formats
  // (Wii U, etc.) plug in by detecting their type and setting `kind` below.
  async function buildConversionJobs(imgPaths: string[], keyPaths: string[], outDir: string): Promise<ConvJob[]> {
    const jobs: ConvJob[] = [];
    for (const img of imgPaths) {
      const name = img.split(/[/\\]/).pop() ?? img;
      const stem = name.replace(/\.[^.]*$/, "").toLowerCase();
      const matchedKey = keyPaths.find((k) => {
        const kn = (k.split(/[/\\]/).pop() ?? "").replace(/\.[^.]*$/, "").toLowerCase();
        return kn === stem;
      }) ?? (keyPaths.length === 1 && imgPaths.length === 1 ? keyPaths[0] : undefined);
      const base: ConvJob = { kind: "ps3", inPath: img, outPath: "", keyPath: "", encrypt: false, name, status: "pending", done: 0, total: 0 };

      // PS3 detection. Future: branch on extension/probe to detect Wii U etc.
      let info: Ps3IsoInfo;
      try {
        info = await invoke<Ps3IsoInfo>("ps3_iso_info", { path: img });
      } catch (e) {
        jobs.push({ ...base, status: "error", error: String(e) });
        continue;
      }
      if (!info.is_ps3) { jobs.push({ ...base, status: "error", error: "Not a supported encrypted image" }); continue; }
      const keyPath = matchedKey ?? info.key_path ?? "";
      if (!keyPath) { jobs.push({ ...base, status: "error", error: "No matching .ird/.key/.dkey found" }); continue; }
      const encrypt = !info.encrypted;
      jobs.push({ ...base, keyPath, encrypt, outPath: convOutPath(img, outDir, encrypt) });
    }
    return jobs;
  }

  const fmtBytes = (n: number) =>
    n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : n >= 1e6 ? `${(n / 1e6).toFixed(0)} MB` : `${n} B`;

  const batchLogLine = (text: string) =>
    setBatchLog((prev) => [...prev, `${new Date().toLocaleTimeString()}  ${text}`]);

  // Everything that could go wrong is decided here, before any work starts.
  async function scanBatch(
    src = batchSrc, out = batchOut, keys = batchKeys,
    recursive = batchRecursive, conflict = batchConflict, target = batchTarget,
  ) {
    if (!src || !out) { setBatchPlan(null); return; }
    setBatchScanning(true);
    setBatchError(null);
    try {
      const plan = await invoke<BatchPlan>("plan_batch_conversion", {
        source: src, output: out, keysFolder: keys || null, recursive, onConflict: conflict, target,
      });
      setBatchPlan(plan);
    } catch (e) {
      setBatchPlan(null);
      setBatchError(String(e));
    } finally {
      setBatchScanning(false);
    }
  }

  // Put the window back to how it opened, for when you change your mind about a
  // batch. Every folder goes, not just the source — the point is a clean slate,
  // and a remembered output path is exactly the kind of thing that quietly sends
  // the next run somewhere unintended.
  function clearBatch() {
    if (convRunning) return;
    for (const [set, key] of [
      [setBatchSrc, "batchSrc"],
      [setBatchOut, "batchOut"],
      [setBatchKeys, "batchKeys"],
    ] as const) {
      set("");
      localStorage.removeItem(key);
    }
    setBatchPlan(null);
    setBatchError(null);
    setBatchSummary(null);
    setBatchLog([]);
  }

  // Accepts a folder or a single image; the planner handles both.
  function setBatchSource(path: string | undefined) {
    if (!path || convRunning) return;
    setBatchSrc(path);
    localStorage.setItem("batchSrc", path);
    void scanBatch(path, batchOut, batchKeys);
  }

  async function pickBatchFolder(which: "src" | "out" | "keys") {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    const setters = { src: setBatchSrc, out: setBatchOut, keys: setBatchKeys } as const;
    const storageKey = { src: "batchSrc", out: "batchOut", keys: "batchKeys" } as const;
    setters[which](dir);
    localStorage.setItem(storageKey[which], dir);
    void scanBatch(
      which === "src" ? dir : batchSrc,
      which === "out" ? dir : batchOut,
      which === "keys" ? dir : batchKeys,
    );
  }

  async function startBatch() {
    if (!batchPlan) return;
    const runnable = batchPlan.items.filter((i) => !i.problem);
    if (runnable.length === 0) return;

    setBatchLog([]);
    setBatchSummary(null);
    const skipped = batchPlan.items.filter((x) => x.problem);
    batchLogLine(`Starting ${runnable.length} of ${batchPlan.items.length}: ${fmtBytes(batchPlan.bytes_needed)} to write`);
    for (const i of skipped) {
      batchLogLine(`${i.name} — skipped: ${i.problem}`);
    }

    const jobs: ConvJob[] = runnable.map((i) => ({
      kind: i.kind, inPath: i.path, outPath: i.out_path, keyPath: i.key_path,
      encrypt: i.encrypt, name: i.name, status: "pending", done: 0, total: 0,
    }));
    const finished = await runConversionJobs(jobs, true);
    const done = finished ?? jobs;
    for (const j of done) {
      batchLogLine(j.status === "done"
        ? `${j.name} — ${j.outPath.split(/[/\\]/).pop()} written`
        : `${j.name} — ${j.error ?? "failed"}`);
    }
    setBatchSummary(summariseBatch(done, skipped.length));
    // The output folder has changed underneath the plan.
    void scanBatch();
  }

  // One line for the whole run. It names the first file that failed, because
  // "something failed" sends you back to the log; the count after it is there
  // so a single named failure is not mistaken for the only one.
  function summariseBatch(jobs: ConvJob[], skipped: number): { text: string; failed: boolean } {
    const cancelled = jobs.filter((j) => j.error === "Cancelled").length;
    const failures = jobs.filter((j) => j.status === "error" && j.error !== "Cancelled");
    const converted = jobs.filter((j) => j.status === "done").length;

    if (failures.length > 0) {
      const more = failures.length > 1 ? `, plus ${failures.length - 1} more` : "";
      return { text: `Batch failed at "${failures[0].name}"${more}`, failed: true };
    }
    if (cancelled > 0) {
      return { text: `Batch cancelled: ${converted} of ${jobs.length} converted`, failed: true };
    }
    const extra = skipped > 0 ? `, ${skipped} skipped` : "";
    return { text: `Batch complete: ${converted} converted${extra}`, failed: false };
  }

  // `conflictsResolved` is set by the batch window, which has already decided
  // what to do about existing files; prompting again per file would undo that.
  async function runConversionJobs(jobs: ConvJob[], conflictsResolved = false): Promise<ConvJob[]> {
    if (jobs.length === 0) return [];
    const results = jobs.map((j) => ({ ...j }));
    convCancelledRef.current = false;
    setConvCancelling(false);
    setConvJobs(jobs);
    setShowConvModal(true);
    setConvRunning(true);
    const onProgress = (e: { payload: { job: number; done: number; total: number } }) => {
      const { job, done, total } = e.payload;
      setConvJobs((prev) => prev.map((j, i) => (i === job ? { ...j, done, total } : j)));
    };
    const unlistenPs3 = await listen<{ job: number; done: number; total: number }>("ps3-progress", onProgress);
    const unlistenWiiu = await listen<{ job: number; done: number; total: number }>("wiiu-progress", onProgress);
    const unlistenConv = await listen<{ job: number; done: number; total: number }>("convert-progress", onProgress);
    for (let i = 0; i < jobs.length; i++) {
      if (jobs[i].status === "error") continue; // pre-flagged (unsupported / no key)
      if (convCancelledRef.current) {
        results[i].status = "error";
        results[i].error = "Cancelled";
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "error", error: "Cancelled" } : j)));
        continue;
      }
      // Prompt before clobbering an existing file; skip just this job if declined.
      if (!conflictsResolved && await invoke<boolean>("path_exists", { path: jobs[i].outPath })) {
        const name = jobs[i].outPath.split(/[/\\]/).pop() ?? jobs[i].outPath;
        const overwrite = await confirm(`"${name}" already exists. Overwrite it?`, {
          title: "File already exists",
          kind: "warning",
        });
        if (!overwrite) {
          results[i].status = "error";
          results[i].error = "Skipped (file exists)";
          setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "error", error: "Skipped (file exists)" } : j)));
          continue;
        }
      }
      setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "running" } : j)));
      try {
        if (jobs[i].kind === "ps3") {
          await invoke("ps3_convert", {
            inPath: jobs[i].inPath,
            outPath: jobs[i].outPath,
            keyPath: jobs[i].keyPath,
            encrypt: jobs[i].encrypt,
            job: i,
          });
        } else if (jobs[i].kind === "wiiu") {
          await invoke("wiiu_convert", {
            inPath: jobs[i].inPath,
            outPath: jobs[i].outPath,
            job: i,
          });
        } else if (jobs[i].kind in CONVERT_TARGET) {
          await invoke("convert_image", {
            inPath: jobs[i].inPath,
            outPath: jobs[i].outPath,
            target: CONVERT_TARGET[jobs[i].kind],
            overwrite: batchConflict === "overwrite",
            job: i,
          });
        } else if (jobs[i].kind === "wux") {
          await invoke("wiiu_compress_wux", {
            inPath: jobs[i].inPath,
            outPath: jobs[i].outPath,
            job: i,
            verify: jobs[i].verify ?? false,
          });
        }
        results[i].status = "done";
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "done", done: j.total || 1, total: j.total || 1 } : j)));
      } catch (e) {
        const msg = String(e).includes("__cancelled__") ? "Cancelled" : String(e);
        results[i].status = "error";
        results[i].error = msg;
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "error", error: msg } : j)));
      }
    }
    setConvRunning(false);
    unlistenPs3();
    unlistenWiiu();
    unlistenConv();
    return results;
  }

  // Follow the running job down the list. Only the index it moves to matters,
  // so this does not fire on every progress tick.
  const convRunningIndex = convJobs.findIndex((j) => j.status === "running");
  useEffect(() => {
    if (convRunningIndex < 0) return;
    convListRef.current
      ?.querySelector(`[data-job="${convRunningIndex}"]`)
      ?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [convRunningIndex]);

  // Cancel an in-progress conversion: signal the backend (it deletes the
  // partial output), then mark remaining queued jobs as cancelled.
  async function cancelConversion() {
    convCancelledRef.current = true;
    setConvCancelling(true);
    try { await invoke("conv_cancel"); } catch { /* nothing running */ }
  }

  // Run an extraction (save_file / save_directory) behind a simple busy window:
  // shows "Extracting…", briefly flashes "Finished", then auto-closes. No
  // progress bar — folder byte/file totals aren't reliable enough to be useful.
  // Flash "Finished" and dismiss.
  function finishExtraction() {
    setExtractStatus("");
    setExtractDone(true);
    setExtractRunning(false);
    window.setTimeout(() => setShowExtractModal(false), 900);
  }

  function abortExtraction(e: unknown) {
    const msg = String(e);
    if (!msg.includes("__cancelled__")) setError(msg);
    setExtractStatus("");
    setExtractRunning(false);
    setShowExtractModal(false);
  }

  async function runExtraction(
    command: "save_file" | "save_directory",
    args: Record<string, unknown>,
    cancellable: boolean, // folder saves can be cancelled between files; single files can't
  ): Promise<boolean> {
    setExtractDone(false);
    setExtractCancelling(false);
    setExtractCancellable(cancellable);
    setExtractStatus("");
    setShowExtractModal(true);
    setExtractRunning(true);
    try {
      await invoke(command, args);
      finishExtraction();
      return true;
    } catch (e) {
      abortExtraction(e);
      return false;
    }
  }

  async function cancelExtraction() {
    setExtractCancelling(true);
    extractCancelRef.current = true;
    try { await invoke("extract_cancel"); } catch { /* nothing running */ }
  }

  // Save every ticked entry into one destination folder.
  async function saveSelected() {
    if (!imagePath || selected.size === 0) return;
    const items = entries.filter((e) => selected.has(e.name));
    const base = defaultDownloadPath
      || await open({ directory: true, title: `Choose destination for ${items.length} item${items.length !== 1 ? "s" : ""}` }) as string | null;
    if (!base) return;

    // A selection can mix plain files, XA files and whole folders, so total the
    // CD-XA files across all of it and ask once for the batch rather than per item.
    let xaCount = 0;
    for (const entry of items) {
      const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;
      xaCount += entry.is_dir
        ? await countXaIn(entryPath, activeFilesystem || null)
        : (entry.is_xa ? 1 : 0);
    }
    withXaChoice(xaCount, (xaMode) => runSelected(items, base, xaMode));
  }

  async function runSelected(items: DiscEntry[], base: string, xaMode: number) {
    setExtractDone(false);
    setExtractCancelling(false);
    setExtractCancellable(true);
    setShowExtractModal(true);
    setExtractRunning(true);
    extractCancelRef.current = false;
    try {
      for (const entry of items) {
        if (extractCancelRef.current) break;
        const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;
        const args = { imagePath, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble", xaMode };
        if (entry.is_dir) {
          await invoke("save_directory", { ...args, dirPath: entryPath, destPath: `${base}/${entry.name}` });
        } else {
          await invoke("save_file", { ...args, filePath: entryPath, destPath: `${base}/${entry.name}` });
        }
      }
      if (extractCancelRef.current) {
        setShowExtractModal(false);
      } else {
        setExtractDone(true); // flash "Finished"
        window.setTimeout(() => setShowExtractModal(false), 900);
        setSelected(new Set());
      }
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("__cancelled__")) setError(msg);
      setShowExtractModal(false);
    } finally {
      setExtractRunning(false);
    }
  }

  // Dropped image(s) + key(s): prompt for an output folder, then convert.
  async function startConversionDrop(imgPaths: string[], keyPaths: string[]) {
    const outDir = await open({ directory: true, title: "Select output folder for converted image(s)" });
    if (!outDir || typeof outDir !== "string") return;
    const jobs = await buildConversionJobs(imgPaths, keyPaths, outDir);
    await runConversionJobs(jobs);
  }

  // In-app button: convert the open PS3 ISO. Prompts for an output folder;
  // writing into the source folder keeps the " (encrypted)/(decrypted)" suffix,
  // a different folder drops it (handled by convOutPath).
  async function convertCurrentPs3() {
    if (!imagePath || !ps3Info?.is_ps3 || !ps3Info.has_key) return;
    const outDir = await open({
      directory: true,
      defaultPath: dirOf(imagePath),
      title: "Select output folder for converted image",
    });
    if (!outDir || typeof outDir !== "string") return;
    const jobs = await buildConversionJobs([imagePath], [], outDir);
    await runConversionJobs(jobs);
  }

  // In-app menu: repackage the open Wii U disc image into a raw .wud or .iso
  // (byte-identical; extension only). Encryption state is preserved — no key
  // needed. Prompts for an output folder; writes "<stem>.<ext>" there. The
  // overwrite prompt in runConversionJobs guards a same-name collision.
  async function convertCurrentWiiu(targetExt: "wud" | "iso") {
    setWiiuMenuOpen(false);
    if (!imagePath || !wiiuConvInfo?.is_wiiu) return;
    const outDir = await open({
      directory: true,
      defaultPath: dirOf(imagePath),
      title: "Select output folder for converted image",
    });
    if (!outDir || typeof outDir !== "string") return;
    const name = imagePath.split(/[/\\]/).pop() ?? imagePath;
    const stem = name.replace(/\.[^.]*$/, "");
    const sep = outDir.includes("\\") || imagePath.includes("\\") ? "\\" : "/";
    const outPath = `${outDir}${sep}${stem}.${targetExt}`;
    const job: ConvJob = {
      kind: "wiiu", inPath: imagePath, outPath, keyPath: "", encrypt: false,
      name, status: "pending", done: 0, total: 0,
    };
    await runConversionJobs([job]);
  }

  // In-app menu: compress the open raw Wii U image (.wud/.iso) into a
  // deduplicated .wux. Encryption state is preserved — no key needed. Prompts
  // for an output folder; writes "<stem>.wux" there.
  async function convertCurrentWiiuWux() {
    setWiiuMenuOpen(false);
    if (!imagePath || !wiiuConvInfo?.is_raw) return;
    const outDir = await open({
      directory: true,
      defaultPath: dirOf(imagePath),
      title: "Select output folder for compressed image",
    });
    if (!outDir || typeof outDir !== "string") return;
    const name = imagePath.split(/[/\\]/).pop() ?? imagePath;
    const stem = name.replace(/\.[^.]*$/, "");
    const sep = outDir.includes("\\") || imagePath.includes("\\") ? "\\" : "/";
    const outPath = `${outDir}${sep}${stem}.wux`;
    const job: ConvJob = {
      kind: "wux", inPath: imagePath, outPath, keyPath: "", encrypt: false,
      name, status: "pending", done: 0, total: 0, verify: wuxVerify,
    };
    await runConversionJobs([job]);
  }

  async function openImageAtPath(path: string) {
    // A track .bin that belongs to a cue sheet: open the cue instead, so the
    // whole disc (every track) loads no matter which track file was picked.
    if (path.toLowerCase().endsWith(".bin")) {
      const cue = await invoke<string | null>("find_cue_for_bin", { binPath: path }).catch(() => null);
      if (cue) path = cue;
    }
    const name = path.split(/[/\\]/).pop() ?? path;
    setActiveFilesystem("");
    setImagePath(path);
    setSourceImagePath(path);
    setImageName(name);
    setError(null);
    const lowerName = name.toLowerCase();
    if ((lowerName.endsWith(".wux") || lowerName.endsWith(".wud")) && !wiiuKeyPath) {
      setWarn("No Wii U common key set — encrypted disc content will not be accessible. Add your key file in Settings (⚙).");
    } else {
      setWarn(null);
    }
    setEmptyDriveName(null);
    setMountedDevice(null);
    setPhysicalDiscActive(false);

    setPs3Info(null);
    if (lowerName.endsWith(".iso")) {
      invoke<Ps3IsoInfo>("ps3_iso_info", { path }).then((info) => {
        if (info.is_ps3) setPs3Info(info);
      }).catch(() => {});
    }

    setWiiuConvInfo(null);
    setWiiuMenuOpen(false);
    if (lowerName.endsWith(".wux") || lowerName.endsWith(".wud") || lowerName.endsWith(".iso")) {
      invoke<WiiuConvInfo>("wiiu_conv_info", { path }).then((info) => {
        if (info.is_wiiu) setWiiuConvInfo(info);
      }).catch(() => {});
    }

    const lowerPath = path.toLowerCase();
    const isCue = lowerPath.endsWith(".cue");
    const isMds = lowerPath.endsWith(".mds");
    const isGdi = lowerPath.endsWith(".gdi");
    const isCdi = lowerPath.endsWith(".cdi");

    if (isCue || isMds || isGdi || isCdi) {
      const [tracks, filesystems] = await Promise.all([
        isGdi
          ? invoke<TrackInfo[]>("get_gdi_tracks", { gdiPath: path })
          : isMds
            ? invoke<TrackInfo[]>("get_mds_tracks", { mdsPath: path })
            : isCdi
              ? invoke<TrackInfo[]>("get_cdi_tracks", { cdiPath: path })
              : invoke<TrackInfo[]>("get_cue_tracks", { cuePath: path }),
        invoke<string[]>("get_disc_filesystems", { imagePath: path }).catch(() => ["ISO 9660"]),
      ]);
      setCueTracks(tracks);
      setDiscFilesystems(filesystems);
      setSidebarPath("__root");

      const sessions = [...new Set(tracks.map((t) => t.session))].sort((a, b) => a - b);
      const multiSession = sessions.length > 1;

      const makeFsChildren = (): TreeNode[] =>
        filesystems.map((fs) => ({
          name: fs,
          path: `__fs_${fs.toLowerCase().replace(/ /g, "_")}`,
          nodeType: "filesystem" as NodeType,
          children: null,
          expanded: false,
        }));

      const makeTrackNode = (t: TrackInfo): TreeNode => t.is_data
        ? {
            name: t.mode === "CDI/PREGAP"
              ? `Track ${String(t.number).padStart(2, "0")} Pregap — CD-i`
              : `Track ${String(t.number).padStart(2, "0")} — ${t.mode}`,
            path: `__track_${t.number}`,
            nodeType: "data_track",
            children: makeFsChildren(),
            expanded: true,
          }
        : {
            name: `Track ${String(t.number).padStart(2, "0")} — ${t.mode}`,
            path: `__audio_${t.number}`,
            nodeType: "audio_track",
            children: null,
            expanded: false,
          };

      const trackNodes: TreeNode[] = multiSession
        ? sessions.map((s): TreeNode => {
            const sessionTracks = tracks.filter((t) => t.session === s);
            const hasData = sessionTracks.some((t) => t.is_data);
            return {
              name: `Session ${s} — ${hasData ? "Data" : "Audio"}`,
              path: `__session_${s}`,
              nodeType: "session",
              children: sessionTracks.map(makeTrackNode),
              expanded: true,
            };
          })
        : tracks.map(makeTrackNode);

      const rootNode: TreeNode = {
        name, path: "__root", nodeType: "root", children: trackNodes, expanded: true,
      };
      setTree([rootNode]);

      // A disc with audio stays on the track list, because the tracks are the
      // point and picking one of several data partitions for someone would be a
      // guess. With no audio there is nothing to stay for, so open the first
      // browsable filesystem — the same landing the non-track formats get.
      // Note `audio` holds every track: a data-only cue has entries here too, so
      // the audio count is what decides, not the length.
      const audio = buildAudioEntries(tracks);
      const audioCount = audio.filter((e) => !e.is_data).length;
      if (audioCount > 0) {
        navIdRef.current++;
        setAudioEntries(audio);
        setEntries([]);
        setViewMode("audio");
        setStatusText(`${audioCount} audio track${audioCount !== 1 ? "s" : ""}${audio.length > audioCount ? `, ${audio.length - audioCount} data track` : ""}`);
      } else {
        const firstFs = firstBrowsableFs(filesystems);
        if (firstFs) {
          setSidebarPath(`__fs_${firstFs.toLowerCase().replace(/ /g, "_")}`);
          await loadDirectory(path, "/", firstFs, firstFs);
        } else {
          await loadDirectory(path, "/");
        }
      }
    } else {
      setCueTracks([]);
      setSidebarPath("/");

      const filesystems = await invoke<string[]>("get_disc_filesystems", { imagePath: path }).catch(() => ["ISO 9660"]);
      setDiscFilesystems(filesystems);
      const makeFsNode = (fs: string): TreeNode => ({
        name: fs,
        path: `__fs_${fs.toLowerCase().replace(/ /g, "_")}`,
        nodeType: "filesystem" as NodeType,
        children: null,
        expanded: false,
      });

      const fsChildren = filesystems.map(makeFsNode);
      const rootNode: TreeNode = { name, path: "__root", nodeType: "root", children: fsChildren, expanded: true };
      setTree([rootNode]);
      if (filesystems.length === 0) {
        setSidebarPath("__root");
        setEntries([]);
        setViewMode("filesystem");
        setStatusText("No browsable filesystem");
        setWarn(lowerName.endsWith(".toc")
          ? "This TOC is track metadata only. Open its companion CUE, SCRAM, or sector-data image to browse files."
          : "This file is empty or has no browsable filesystem.");
        return;
      }
      const firstFs = firstBrowsableFs(filesystems) || "ISO 9660";
      const firstFsPath = `__fs_${firstFs.toLowerCase().replace(/ /g, "_")}`;
      setSidebarPath(firstFsPath);
      // loadDirectory's syncSidebarTree expands the first filesystem node with
      // its folder tree.
      await loadDirectory(path, "/", firstFs, firstFs);
    }
  }

  async function openImage() {
    const selected = await open({
      filters: [{ name: "Disc Images", extensions: ["iso", "img", "bin", "fatx", "chd", "cue", "mds", "mdx", "nrg", "ccd", "cdi", "gdi", "toc", "b5t", "b6t", "bwt", "c2d", "pdi", "gi", "daa", "cso", "ciso", "ecm", "wbfs", "wux", "wud", "gcz", "wua", "rvz", "wia", "zip", "tar", "tgz", "tbz", "txz", "nds", "srl", "cab", "vpk", "scram", "sdram", "sbram", "aif", "cif", "uif", "skeleton", "zst", "raw"] }],
    });
    if (!selected) return;
    await openImageAtPath(selected as string);
  }

  async function handleDrop(dropped: string[]) {
    const isos = dropped.filter((p) => p.toLowerCase().endsWith(".iso"));
    const keys = dropped.filter((p) => /\.(key|dkey|ird)$/i.test(p));
    // PS3 image + key dropped together → convert (decrypt/encrypt) instead of browse.
    if (isos.length > 0 && keys.length > 0) {
      await startConversionDrop(isos, keys);
      return;
    }

    // Wii U batch: .wux/.wud by extension, plus any .iso that is actually a Wii U
    // disc (content-sniffed). 2+ Wii U files → prompt once for target + folder.
    const wiiuExt = dropped.filter((p) => /\.(wux|wud)$/i.test(p));
    let wiiuIso: string[] = [];
    if (isos.length > 0) {
      const checks = await Promise.all(
        isos.map(async (p) => {
          try {
            const info = await invoke<WiiuConvInfo>("wiiu_conv_info", { path: p });
            return info.is_wiiu ? p : null;
          } catch {
            return null;
          }
        })
      );
      wiiuIso = checks.filter((p): p is string => p !== null);
    }
    const wiiuFiles = [...wiiuExt, ...wiiuIso];
    if (wiiuFiles.length >= 2) {
      setWiiuBatchVerify(false);
      setWiiuBatchPaths(wiiuFiles);
      return;
    }

    const supported = ["iso", "img", "chd", "cue", "mds", "mdx", "nrg", "ccd", "cdi", "gdi", "toc", "b5t", "b6t", "bwt", "c2d", "pdi", "gi", "daa", "cso", "ciso", "ecm", "wbfs", "wux", "wud", "gcz", "wua", "rvz", "wia", "zip", "tar", "tgz", "tbz", "txz", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "nds", "srl", "cab", "vpk", "scram", "sdram", "sbram", "aif", "cif", "uif", "skeleton", "skeleton.zst", "iso.zst", "img.zst"];
    const path = dropped.find((p) =>
      supported.some((ext) => p.toLowerCase().endsWith(`.${ext}`))
    );
    if (path) await openImageAtPath(path);
  }

  // After the user picks a target format in the batch modal: prompt for an output
  // folder, then queue every dropped Wii U image as a conversion job.
  async function runWiiuBatch(target: "wud" | "iso" | "wux") {
    const paths = wiiuBatchPaths ?? [];
    const verify = wiiuBatchVerify;
    setWiiuBatchPaths(null);
    if (paths.length === 0) return;
    const outDir = await open({
      directory: true,
      defaultPath: dirOf(paths[0]),
      title: "Select output folder for converted image(s)",
    });
    if (!outDir || typeof outDir !== "string") return;

    const sepFor = (p: string) =>
      outDir.includes("\\") || p.includes("\\") ? "\\" : "/";
    const jobs: ConvJob[] = [];
    for (const p of paths) {
      const name = p.split(/[/\\]/).pop() ?? p;
      // Compressing to .wux only makes sense from a raw source; skip .wux inputs.
      if (target === "wux" && /\.wux$/i.test(p)) {
        jobs.push({ kind: "wux", inPath: p, outPath: "", keyPath: "", encrypt: false, name, status: "error", done: 0, total: 0, error: "Already compressed (.wux)" });
        continue;
      }
      const stem = name.replace(/\.[^.]*$/, "");
      const outPath = `${outDir}${sepFor(p)}${stem}.${target}`;
      jobs.push({
        kind: target === "wux" ? "wux" : "wiiu",
        inPath: p, outPath, keyPath: "", encrypt: false,
        name, status: "pending", done: 0, total: 0,
        ...(target === "wux" ? { verify } : {}),
      });
    }
    await runConversionJobs(jobs);
  }

  useEffect(() => { showBatchRef.current = showBatch; }, [showBatch]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview().onDragDropEvent((event) => {
      // With Batch Convert open a drop means "use this as the source", not
      // "open this image": the window in front is what the gesture is aimed at.
      const toBatch = showBatchRef.current;
      if (event.payload.type === "drop") {
        setIsDragOver(false);
        setBatchDragOver(false);
        if (toBatch) setBatchSource(event.payload.paths[0]);
        else handleDrop(event.payload.paths);
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
        setBatchDragOver(false);
      } else if (toBatch) {
        setBatchDragOver(true);
      } else {
        setIsDragOver(true);
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  async function mountImage() {
    if (!sourceImagePath) return;
    try {
      const result = await invoke<MountResult>("mount_disc_image", { imagePath: sourceImagePath });
      setMountedDevice(result.device);
      setError(null);
      // Keep browsing via the source image using our own readers — macOS may not
      // be able to read the mounted filesystem (e.g. UDF 2.50 is unsupported).
      // The mount makes the device visible in Finder/Disk Utility.
      const name = imageName; // already set from the source image
      const rootNode: TreeNode = { name, path: "/", nodeType: "root", children: null, expanded: false };
      setTree([rootNode]);
      await loadDirectory(sourceImagePath, "/");
      const entries2 = await invoke<DiscEntry[]>("list_disc_contents", { imagePath: sourceImagePath, dirPath: "/", showResourceForks: forkModeRef.current === "list" });
      const subDirs = entries2.filter(e => e.is_dir).map(e => ({
        name: e.name, path: `/${e.name}`, nodeType: "dir" as NodeType, children: null, expanded: false,
      }));
      setTree([{ ...rootNode, expanded: true, children: subDirs }]);
    } catch (e) {
      setError(String(e));
    }
  }

  async function unmountImage() {
    if (!mountedDevice) return;
    try {
      await invoke("unmount_disc_image", { device: mountedDevice });
    } catch (e) {
      setError(String(e));
    }
    setMountedDevice(null);
    // Keep the disc image open in the app after unmounting.
  }

  function ejectImage() {
    setSourceImagePath(null);
    setImagePath(null);
    setImageName("");
    setEntries([]);
    setAudioEntries([]);
    setTree([]);
    setCueTracks([]);
    setDiscFilesystems([]);
    setCdText(null);
    setActiveFilesystem("");
    setSidebarPath("");
    setError(null);
    setWarn(null);
    setStatusText("No disc loaded");
    setViewMode("filesystem");
    setPs3Info(null);
    setWiiuConvInfo(null);
    setWiiuMenuOpen(false);
  }

  function isCdemuEmulatable(path: string): boolean {
    const lower = path.toLowerCase();
    return [".iso", ".img", ".cue", ".mds", ".mdx", ".nrg", ".ccd", ".cdi",
            ".gdi", ".toc", ".b5t", ".b6t", ".bwt", ".c2d", ".pdi", ".gi", ".daa"]
      .some(ext => lower.endsWith(ext));
  }

  async function emulateDrive() {
    if (!sourceImagePath) return;
    setEmulating(true);
    try {
      const drive = await invoke<EmulatedDrive>("emulate_drive", { imagePath: sourceImagePath });
      setEmulatedDrives(prev => [...prev, drive]);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setEmulating(false);
    }
  }

  async function ejectEmulatedDrive(slot: string) {
    try {
      await invoke("eject_emulated_drive", { slot });
      setEmulatedDrives(prev => prev.filter(d => d.slot !== slot));
    } catch (e) {
      setError(String(e));
    }
  }

  function unmountPhysicalDisc() {
    setPhysicalDiscActive(false);
    setImagePath(null);
    setImageName("");
    setEntries([]);
    setAudioEntries([]);
    setTree([]);
    setCueTracks([]);
    setDiscFilesystems([]);
    setCdText(null);
    setActiveFilesystem("");
    setSidebarPath("");
    setError(null);
    setStatusText("No disc loaded");
    setViewMode("filesystem");
  }

  async function ejectDisc() {
    if (!imagePath) return;
    try {
      await invoke("eject_disc", { path: imagePath });
    } catch (e) {
      setError(String(e));
    }
    unmountPhysicalDisc();
  }

  async function openDisc() {
    setLoadingDrives(true);
    try {
      const result = await invoke<DriveInfo[]>("list_optical_drives");
      setDrives(result);
      const withDisc = result.filter(d => d.has_disc);
      if (withDisc.length === 1) {
        selectDrive(withDisc[0]);
      } else {
        setShowDriveMenu(true);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoadingDrives(false);
    }
  }

  async function openDumpDriveMenu() {
    setLoadingDrives(true);
    try {
      const result = await invoke<DriveInfo[]>("list_optical_drives");
      setDrives(result);
      const withDisc = result.filter(d => d.has_disc);
      if (withDisc.length === 1) {
        selectDumpDrive(withDisc[0]);
      } else {
        setShowDumpDriveMenu(true);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoadingDrives(false);
    }
  }

  async function selectDumpDrive(drive: DriveInfo) {
    setShowDumpDriveMenu(false);
    setDumpDrive(drive.raw_device_path);
    if (!dumpOutputPath) setDumpOutputPath(await downloadDir());
    if (drive.volume_name) {
      setDumpSubfolder(drive.volume_name);
    } else {
      const now = new Date();
      const pad = (n: number, w = 2) => String(n).padStart(w, "0");
      const yy = String(now.getFullYear()).slice(2);
      const ts = `${yy}${pad(now.getMonth()+1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
      setDumpSubfolder(`dump_${ts}_${drive.raw_device_path}`);
    }
    setShowDumpModal(true);
  }

  async function selectDrive(drive: DriveInfo) {
    setShowDriveMenu(false);
    setError(null);

    if (!drive.has_disc) {
      setViewMode("empty-drive");
      setEmptyDriveName(drive.name);
      setSourceImagePath(null);
      setImagePath(null);
      setImageName("");
      setEntries([]);
      setAudioEntries([]);
      setTree([]);
      setStatusText("No disc loaded");
      setPhysicalDiscActive(false);
      return;
    }

    const name = drive.volume_name || drive.name;
    setSourceImagePath(null);
    setPhysicalDiscActive(true);
    setImagePath(drive.device_path);
    setImageName(name);
    setEmptyDriveName(null);
    setDumpDrive(drive.raw_device_path);

    const rootNode: TreeNode = { name, path: "/", nodeType: "root", children: null, expanded: false };
    setTree([rootNode]);
    await loadDirectory(drive.device_path, "/");

    try {
      const result = await invoke<DiscEntry[]>("list_disc_contents", {
        imagePath: drive.device_path, dirPath: "/", showResourceForks: forkModeRef.current === "list",
      });
      const subDirs = result
        .filter((e) => e.is_dir)
        .map((e): TreeNode => ({
          name: e.name, path: `/${e.name}`, nodeType: "dir", children: null, expanded: false,
        }));
      setTree([{ ...rootNode, expanded: true, children: subDirs }]);
    } catch {
      // Tree build failed; directory already loaded above
    }
  }

  // Rip every audio track into `dir`, assuming the extract modal is already up.
  // Returns false if it was cancelled or failed, having reported the failure.
  async function ripAudioTracks(tracks: TrackInfo[], dir: string): Promise<boolean> {
    for (let i = 0; i < tracks.length; i++) {
      if (extractCancelRef.current) return false;
      const t = tracks[i];
      const name = trackFileName(t.number);
      setExtractStatus(`${name} — ${i + 1} of ${tracks.length}`);
      try {
        await invoke("save_audio_track", {
          cuePath: imagePath,
          trackNumber: t.number,
          destPath: `${dir}/${name}.${audioFormat}`,
          format: audioFormat,
          gapMode,
        });
      } catch (e) {
        abortExtraction(e);
        return false;
      }
    }
    return true;
  }

  // "Everything on the disc": every distinct filesystem, plus every audio track.
  // A mixed-mode or Enhanced CD has both, and extracting only the files silently
  // loses the music.
  //
  // Scope follows the sidebar. Inside a filesystem — its own node, or any folder
  // within it — only that filesystem is extracted, because that is what the user
  // pointed at. At the disc, session or track level the whole disc is taken.
  //
  // `withAudio: false` is the data track's own download button: that row is
  // asking for the files on that track, not for the disc's music as well.
  async function dumpContents(opts: { withAudio?: boolean } = {}) {
    if (!imagePath) return;
    const { withAudio = true } = opts;

    const inFilesystem = sidebarPath.startsWith("__fs_") || sidebarPath.startsWith("/");
    const scoped = inFilesystem && activeFilesystem
      ? [scopedTarget(activeFilesystem, discFilesystems)]
      : null;
    const targets = scoped ?? distinctFilesystems(discFilesystems);
    // A filesystem was singled out, so the audio tracks are not part of the ask.
    const audioTracks = scoped || !withAudio ? [] : cueTracks.filter((t) => !t.is_data);

    // Nothing to walk and nothing to rip: say so rather than opening a progress
    // window that flashes "Finished" over an empty folder.
    if (targets.length === 0 && audioTracks.length === 0) {
      setError("This image has no browsable filesystem and no audio tracks to extract.");
      return;
    }

    const destPath = await open({ directory: true, title: "Choose destination for disc contents" });
    if (!destPath) return;
    // Prefer the disc's own label so extracted files land in a folder named after
    // the disc rather than after whatever the image file happens to be called.
    const volName = volumeLabel || (tree[0]?.name ?? imageName).replace(/\.[^/.]+$/, "") || "disc";
    const discDir = `${destPath}/${volName}`;

    // One filesystem keeps the flat layout; several would collide, so each gets
    // its own folder. Tracks stay clear of the disc's files, except on an audio
    // disc where there are none to collide with.
    const multi = targets.length > 1;
    const audioDir = targets.length > 0 ? `${discDir}/Audio Tracks` : discDir;

    const run = async (xaMode: number) => {
      extractCancelRef.current = false;
      setExtractDone(false);
      setExtractCancelling(false);
      setExtractCancellable(true);
      setExtractStatus("");
      setShowExtractModal(true);
      setExtractRunning(true);

      for (const t of targets) {
        if (extractCancelRef.current) { abortExtraction("__cancelled__"); return; }
        if (multi) setExtractStatus(t.name);
        try {
          await invoke("save_directory", {
            imagePath,
            dirPath: "/",
            destPath: multi ? `${discDir}/${t.name}` : discDir,
            filesystem: t.pass,
            appleDouble: forkModeRef.current === "appledouble",
            xaMode,
          });
        } catch (e) { abortExtraction(e); return; }
      }

      if (audioTracks.length > 0 && !(await ripAudioTracks(audioTracks, audioDir))) {
        if (extractCancelRef.current) abortExtraction("__cancelled__");
        return;
      }
      finishExtraction();
    };

    withXaChoice(await countXaIn("/", targets[0]?.pass ?? null), run);
  }


  async function handleTreeToggle(nodePath: string, fs?: string) {
    if (!imagePath) return;
    // Captured so the nested async helpers below keep the narrowed type.
    const imgPath = imagePath;

    if (nodePath.startsWith("__track_")) {
      function toggleExpanded(nodes: TreeNode[]): TreeNode[] {
        return nodes.map((n) => {
          if (n.path === nodePath) return { ...n, expanded: !n.expanded };
          if (n.children) return { ...n, children: toggleExpanded(n.children) };
          return n;
        });
      }
      setTree(toggleExpanded(tree));
      return;
    }

    if (nodePath.startsWith("__fs_")) {
      function toggleFs(nodes: TreeNode[]): TreeNode[] {
        return nodes.map((n) => {
          if (n.path === nodePath) return { ...n, expanded: !n.expanded };
          if (n.children) return { ...n, children: toggleFs(n.children) };
          return n;
        });
      }
      setTree(toggleFs(tree));
      return;
    }

    if (nodePath.startsWith("__")) return;

    async function expandNode(nodes: TreeNode[]): Promise<TreeNode[]> {
      return Promise.all(nodes.map(async (node) => {
        // Path alone is ambiguous on a hybrid disc: /MUSIC exists in HFS, ISO
        // 9660 and Joliet, and toggling one used to toggle all three.
        if (node.path !== nodePath || node.fs !== fs) {
          return { ...node, children: node.children ? await expandNode(node.children) : null };
        }
        if (node.expanded) return { ...node, expanded: false };
        let children = node.children;
        if (children === null) {
          // Name the filesystem: on a hybrid disc the same path can exist in one
          // and not the other, and an unnamed listing takes whichever detects first.
          const nodeFs = node.fs ?? activeFilesystem;
          const result = await invoke<DiscEntry[]>("list_disc_contents", { imagePath, dirPath: nodePath, filesystem: nodeFs || null, showResourceForks: forkModeRef.current === "list" });
          children = result
            .filter((e) => e.is_dir)
            .map((e): TreeNode => ({
              name: e.name,
              path: nodePath === "/" ? `/${e.name}` : `${nodePath}/${e.name}`,
              nodeType: "dir",
              children: null,
              expanded: false,
              fs: node.fs,
            }));
          children = await probeSubfolders(imgPath, children, forkModeRef.current === "list", nodeFs || null);
        }
        return { ...node, expanded: true, children };
      }));
    }
    setTree(await expandNode(tree));
  }

  function findNodeByPath(nodes: TreeNode[], target: string): TreeNode | null {
    for (const n of nodes) {
      if (n.path === target) return n;
      if (n.children) {
        const found = findNodeByPath(n.children, target);
        if (found) return found;
      }
    }
    return null;
  }

  function handleTreeSelect(path: string, fs?: string) {
    if (!imagePath) return;

    if (path === "__root") {
      setSidebarPath("__root");
      const audio = buildAudioEntries(cueTracks);
      const audioCount = audio.filter((e) => !e.is_data).length;
      if (audio.length > 0) {
        navIdRef.current++;
        setAudioEntries(audio);
        setEntries([]);
        setViewMode("audio");
        setCurrentPath("__root");
        setStatusText(`${audioCount} audio track${audioCount !== 1 ? "s" : ""}${audio.length > audioCount ? `, ${audio.length - audioCount} data track` : ""}`);
      } else {
        loadDirectory(imagePath, "/");
      }
      return;
    }

    if (path.startsWith("__session_")) {
      setSidebarPath(path);
      const sessionNum = parseInt(path.replace("__session_", ""), 10);
      const sessionTracks = cueTracks.filter((t) => t.session === sessionNum);

      // A data session's content is the filesystem inside it, the same as its
      // data track — go there rather than listing the one track. An audio
      // session has no filesystem to enter, so it still lists its tracks;
      // playing starts from a track, not from the session holding them.
      if (sessionTracks.some((t) => t.is_data) && !sessionTracks.some((t) => !t.is_data)) {
        const firstFs = firstBrowsableFs(discFilesystems);
        if (firstFs) {
          loadDirectory(imagePath, "/", firstFs, firstFs);
          return;
        }
      }

      navIdRef.current++;
      const audio = buildAudioEntries(sessionTracks);
      const audioCount = audio.filter((e) => !e.is_data).length;
      setAudioEntries(audio);
      setEntries([]);
      setViewMode("audio");
      setCurrentPath(path);
      setStatusText(`Session ${sessionNum} — ${audioCount} audio track${audioCount !== 1 ? "s" : ""}${audio.length > audioCount ? `, ${audio.length - audioCount} data track` : ""}`);
      return;
    }

    // Picking an audio track plays it. The list stays on the whole disc rather
    // than narrowing to the one track: a single-row listing is not somewhere to
    // be, and the point of choosing a track is to hear it.
    if (path.startsWith("__audio_")) {
      setSidebarPath(path);
      const trackNum = parseInt(path.replace("__audio_", ""), 10);
      const track = cueTracks.find((t) => t.number === trackNum && !t.is_data);
      if (track) {
        navIdRef.current++;
        const all = buildAudioEntries(cueTracks);
        const audioCount = all.filter((e) => !e.is_data).length;
        setAudioEntries(all);
        setEntries([]);
        setViewMode("audio");
        setCurrentPath("__root");
        setStatusText(`${audioCount} audio track${audioCount !== 1 ? "s" : ""}${all.length > audioCount ? `, ${all.length - audioCount} data track` : ""}`);
        const entry = buildAudioEntries([track])[0];
        if (entry) playTrack(entry);
      }
      return;
    }

    // A data track's content is the filesystem inside it, so go there rather
    // than showing a one-row list of the track itself.
    if (path.startsWith("__track_")) {
      setSidebarPath(path);
      const firstFs = firstBrowsableFs(discFilesystems);
      if (firstFs) loadDirectory(imagePath, "/", firstFs, firstFs);
      return;
    }

    // A path table maps every directory on the disc to its starting sector, so
    // following an entry is exactly what the index is for: go to that folder in
    // ISO 9660, the filesystem the table describes. The Path Table view itself
    // serves nothing below its root, so navigating within it would only ever
    // produce an empty pane.
    if (path.startsWith("__pt_")) {
      const target = path.slice("__pt_".length) || "/";
      loadDirectory(imagePath, target, "ISO 9660", "ISO 9660");
      return;
    }

    if (path.startsWith("__fs_")) {
      setSidebarPath(path);
      const fsName = findNodeByPath(tree, path)?.name ?? "";
      // loadDirectory's syncSidebarTree expands this filesystem node with its
      // folder tree and collapses sibling filesystem nodes.
      loadDirectory(imagePath, "/", fsName, fsName);
      return;
    }

    if (!path.startsWith("__")) {
      setSidebarPath(path);
      // Load the folder from the filesystem it was clicked in. Without this a
      // hybrid disc used whichever filesystem was already active, so clicking
      // Joliet's MUSIC could ask ISO 9660 for it — and asking ISO 9660 for a
      // Joliet-only path is what produced "Directory not found".
      loadDirectory(imagePath, path, fs ?? "", fs ?? (activeFilesystem || undefined));
    }
  }

  // Open the custom menu at the cursor with a Download action.
  function openDownloadMenu(
    e: React.MouseEvent,
    items: { label: string; title?: string; run: () => void }[],
  ) {
    e.preventDefault();
    e.stopPropagation();
    const x = Math.min(e.clientX, window.innerWidth - 210);
    const y = Math.min(e.clientY, window.innerHeight - (26 * items.length + 16));
    setCtxMenu({ x, y, items });
  }

  // Right-click on a sidebar node: folders and filesystem roots download.
  function handleTreeContextMenu(node: TreeNode, e: React.MouseEvent) {
    if (!imagePath) return;
    if (node.nodeType === "dir" && !node.path.startsWith("__")) {
      openDownloadMenu(e, [{ label: "Download", run: () => {
        (async () => {
          const base = defaultDownloadPath || await open({ directory: true, title: `Choose destination for "${node.name}"` }) as string | null;
          if (!base) return;
          const count = await countXaIn(node.path, activeFilesystem || null);
          withXaChoice(count, (xaMode) => runExtraction("save_directory", { imagePath, dirPath: node.path, destPath: `${base}/${node.name}`, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble", xaMode }, true));
        })();
      } }]);
    } else if (node.nodeType === "filesystem") {
      openDownloadMenu(e, [{ label: "Download", run: () => {
        (async () => {
          const base = defaultDownloadPath || await open({ directory: true, title: `Choose destination for "${node.name}"` }) as string | null;
          if (!base) return;
          const count = await countXaIn("/", node.name || null);
          withXaChoice(count, (xaMode) => runExtraction("save_directory", { imagePath, dirPath: "/", destPath: `${base}/${node.name}`, filesystem: node.name || null, appleDouble: forkModeRef.current === "appledouble", xaMode }, true));
        })();
      } }]);
    }
    // Other node kinds: default menu is suppressed globally; nothing to show.
  }

  // How many CD-XA streaming files a subtree holds (0 for filesystems that have
  // no such concept, so they are never prompted).
  function countXaIn(dirPath: string, filesystem: string | null): Promise<number> {
    if (!imagePath) return Promise.resolve(0);
    return invoke<number>("count_xa_files", { imagePath, dirPath, filesystem }).catch(() => 0);
  }

  // Ask how CD-XA files should be written before extracting anything that contains
  // them; go straight through when there are none. Every extraction entry point
  // funnels through here so no path can silently pick a sector width — which is
  // the mistake that produced differing file sizes in the first place.
  function withXaChoice(count: number, run: (mode: number) => unknown) {
    if (xaDefault !== "ask") {
      void run(xaDefault);          // already told us once
    } else if (count > 0) {
      setXaRemember(false);
      setXaPrompt({ count, run: (mode) => { void run(mode); } });
    } else {
      void run(0);                  // nothing XA here, nothing to decide
    }
  }

  // The row's download button: same as the context menu's first item, but asks when the
  // target holds CD-XA files rather than assuming a mode.
  async function saveEntryAsking(entry: DiscEntry) {
    if (!imagePath) return;
    const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;
    const count = entry.is_dir
      ? await countXaIn(entryPath, activeFilesystem || null)
      : (entry.is_xa ? 1 : 0);
    withXaChoice(count, (mode) => saveEntry(entry, mode));
  }

  // xaMode: 0 = file content, 1 = keep subheader (2336), 2 = raw sectors (2352).
  async function saveEntry(entry: DiscEntry, xaMode = 0) {
    if (!imagePath) return;
    const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;

    if (entry.is_dir) {
      const base = defaultDownloadPath || await open({ directory: true, title: `Choose destination for "${entry.name}"` }) as string | null;
      if (!base) return;
      await runExtraction("save_directory", { imagePath, dirPath: entryPath, destPath: `${base}/${entry.name}`, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble", xaMode }, true);
    } else {
      const destPath = defaultDownloadPath
        ? `${defaultDownloadPath}/${entry.name}`
        : await save({ defaultPath: entry.name });
      if (!destPath) return;
      const ok = await runExtraction("save_file", { imagePath, filePath: entryPath, destPath, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble", xaMode }, false);
      // The record on the disc holds zero bytes — tell the user the empty
      // result is by design, not a failed download.
      if (ok && !entry.is_dir && entry.size_bytes === 0 && !skipEmptyFileNotice) setEmptyFileNotice(entry.name);
    }
  }

  async function saveAudioTrack(entry: AudioEntry) {
    if (!imagePath) return;
    const ext = audioFormat;
    const base = trackFileName(entry.track_number);
    const destPath = defaultDownloadPath
      ? `${defaultDownloadPath}/${base}.${ext}`
      : await save({
          defaultPath: `${base}.${ext}`,
          filters: [{ name: ext === "flac" ? "FLAC Audio" : ext === "mp3" ? "MP3 Audio" : "WAV Audio", extensions: [ext] }],
        });
    if (!destPath) return;
    try {
      await invoke("save_audio_track", {
        cuePath: imagePath,
        trackNumber: entry.track_number,
        destPath,
        format: ext,
        gapMode,
      });
    } catch (e) { setError(String(e)); }
  }

  // Decode an audio track to WAV and load it into the player bar (autoplays).
  async function playTrack(entry: AudioEntry) {
    if (!imagePath || entry.is_data) return;
    setAudioLoading(entry.track_number);
    try {
      const buf = await invoke<ArrayBuffer>("audio_track_wav", { cuePath: imagePath, trackNumber: entry.track_number, gapMode });
      const url = URL.createObjectURL(new Blob([buf], { type: "audio/wav" }));
      setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return url; });
      setPlayingTrack(entry.track_number);
    } catch (e) {
      setError(String(e));
    } finally {
      setAudioLoading(null);
    }
  }

  // Continue to the next audio track on the disc. Driven from the full track list
  // rather than whatever the table happens to be showing, so playback keeps going
  // even when a single track was opened from the sidebar. Data tracks are skipped;
  // reaching the last track simply stops.
  function adjacentTrack(dir: 1 | -1): TrackInfo | undefined {
    if (playingTrack === null) return undefined;
    const idx = cueTracks.findIndex((t) => t.number === playingTrack);
    if (idx < 0) return undefined;
    const rest = dir > 0 ? cueTracks.slice(idx + 1) : cueTracks.slice(0, idx).reverse();
    return rest.find((t) => !t.is_data);
  }

  function stepTrack(dir: 1 | -1) {
    const t = adjacentTrack(dir);
    if (t) void playTrack(buildAudioEntries([t])[0]);
  }

  // Skip-back restarts the current track unless we're still near its start, which
  // is what every other music player does with the same button.
  function skipBack() {
    const el = audioElRef.current;
    if (el && el.currentTime > 3) { el.currentTime = 0; return; }
    if (adjacentTrack(-1)) stepTrack(-1);
    else if (el) el.currentTime = 0;
  }

  function playNextTrack() {
    if (autoAdvance) stepTrack(1);
  }

  function togglePlay() {
    const el = audioElRef.current;
    if (!el) return;
    if (el.paused) void el.play(); else el.pause();
  }

  function closePlayer() {
    setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return null; });
    setPlayingTrack(null);
    setIsPlaying(false);
    setAudioPos(0);
    setAudioDur(0);
  }

  // Stop playback when the image is closed/changed.
  useEffect(() => { closePlayer(); /* eslint-disable-next-line */ }, [imagePath]);

  function navigateUp() {
    if (!imagePath || currentPath === "/" || viewMode === "audio") return;
    const parent = currentPath.substring(0, currentPath.lastIndexOf("/")) || "/";
    loadDirectory(imagePath, parent);
  }

  const breadcrumbs = currentPath === "/" || viewMode === "audio"
    ? [{ label: imageName || "Root", path: "/" }]
    : [
        { label: imageName || "Root", path: "/" },
        ...currentPath.split("/").filter(Boolean).map((part, i, arr) => ({
          label: part,
          path: "/" + arr.slice(0, i + 1).join("/"),
        })),
      ];

  const fsCols: { key: keyof ColWidths; label: string }[] = [
    { key: "name", label: "Name" },
    { key: "size", label: "Size" },
    { key: "lba", label: "LBA" },
    { key: "modified", label: "Modified" },
    { key: "save", label: "" },
  ];

  // Every track row can be saved now — audio to a file, a data track to a
  // folder of its files — so the column follows the presence of tracks rather
  // than of audio specifically.
  const showTrackSave = audioEntries.length > 0;

  const audioCols: { key: keyof ColWidths; label: string }[] = [
    { key: "name", label: "Track" },
    { key: "size", label: "Duration" },
    { key: "lba", label: "Start Sector" },
    { key: "modified", label: "Format" },
    ...(showTrackSave ? [{ key: "save" as keyof ColWidths, label: "Save" }] : []),
  ];

  const cols = viewMode === "audio" ? audioCols : fsCols;

  // Pull the build token out of redumper's --version string for the settings
  // label, e.g. "redumper (build: b720)" → "b720". Falls back to plain
  // "Redumper" when the version is unknown (external binary / error).
  const redumperBuild = redumperVersion.match(/build[:_\s-]*([0-9a-z.]+)/i)?.[1];
  const redumperLabel = redumperBuild ? `Redumper (build: ${redumperBuild})` : "Redumper";

  if (IS_SECTOR_VIEW_WINDOW) {
    if (!svParams) return null;
    return (
      <SectorView
        imagePath={svParams.imagePath}
        initialLba={svParams.lba}
        initialCompareImagePath={svParams.compareImagePath}
        onClose={() => getCurrentWindow().close()}
      />
    );
  }

  return (
    <div className="app">
      {isDragOver && (
        <div className="drag-overlay">
          <div className="drag-overlay-inner">
            <div className="drag-overlay-icon"><Icon name="disc" /></div>
            <p>Drop disc image to open</p>
          </div>
        </div>
      )}
      <div className="toolbar">
        <div className="toolbar-left">
          <div className="tools-menu-wrap" ref={toolsMenuRef}>
            <button
              className={`btn-tools${showTools ? " btn-tools--open" : ""}`}
              title="Tools"
              aria-label="Tools"
              onClick={() => setShowTools(v => !v)}
            >
              {/* Inlined rather than imported so it inherits currentColor and
                  follows the theme, the way every other toolbar icon does.
                  The viewBox is cropped to the artwork, which the original
                  artboard padded by a quarter of its height. Sized against the
                  gear's *drawn* size rather than its box: that icon fills only
                  86% of its 24px viewBox, so 21px here matches it. */}
              <svg viewBox="21 11 375 390" width="20" height="21" aria-hidden="true"
                fill="currentColor" fillRule="nonzero" clipRule="evenodd">
                <path d="M287.288,254.025c-3.021,-3.008 -7.113,-4.688 -11.058,-5c-8.125,-0.654 -16.387,-3.787 -22.612,-10.012c-6.275,-6.263 -9.617,-14.396 -10.013,-22.613c-0.2,-4.021 -1.946,-7.992 -5.012,-11.062c-6.733,-6.742 -17.683,-6.742 -24.429,0l-10.708,10.704l-111.046,-108.296c0,0 -5.392,-4.487 -6.183,-8.562c-0.938,-4.854 -1.708,-11.2 -6.913,-15.733c-9.462,-8.246 -25.296,-21.05 -35.162,-28.021c-4.158,-2.938 -7.038,-2.312 -9.942,0.596l-7.542,7.546c-2.904,2.904 -3.538,5.779 -0.592,9.933c6.971,9.875 19.763,25.696 28.021,35.167c4.533,5.208 10.875,5.971 15.729,6.917c4.079,0.788 8.562,6.188 8.562,6.188l108.288,111.042l-10.696,10.708c-6.746,6.746 -6.746,17.692 0,24.438c3.071,3.062 7.037,4.804 11.058,5.004c8.217,0.4 16.354,3.733 22.621,10.004c6.221,6.225 9.35,14.492 10.013,22.625c0.312,3.938 1.979,8.033 5,11.042l69.054,69.067c13.496,13.492 35.379,13.496 48.863,0l13.762,-13.762c13.496,-13.5 13.492,-35.371 0,-48.871l-69.063,-69.046Z" />
                <path d="M392.308,94.492l-64.713,35.546l-29.442,-17.85l0.733,-34.433l67.012,-36.804c-31.196,-27.833 -79.079,-26.783 -109.017,3.15c-19.125,19.125 -26.446,45.571 -21.996,70.317c1.042,5.829 -0.404,11.929 -5.246,16.779l-45.154,45.158l18.846,18.387l0.108,-0.121c6.133,-6.125 14.279,-9.496 22.946,-9.496c8.658,0 16.812,3.371 22.929,9.496c2.992,3 5.321,6.496 6.929,10.279l21.9,-21.904c4.933,-4.925 11.954,-6.629 18.238,-5.017c25.233,6.458 53.087,-1.704 72.875,-21.496c16.996,-16.996 24.663,-39.775 23.05,-61.992Z" />
                <path d="M165.25,232.813l0.117,-0.113l-18.375,-18.846l-108.179,108.175c-13.854,13.854 -13.854,36.3 0,50.15c13.846,13.85 36.3,13.85 50.15,0l86.571,-86.563c-3.779,-1.612 -7.283,-3.929 -10.279,-6.933c-12.65,-12.646 -12.646,-33.221 -0.004,-45.871Z" />
              </svg>
            </button>
            {showTools && (
              <div className="tools-menu">
                <div
                  className="tools-menu-item"
                  onClick={() => { setShowTools(false); setShowBatch(true); void scanBatch(); }}
                >
                  <span>Batch Convert…</span>
                  <span className="tools-menu-hint">Encrypt, decrypt or repackage a folder of images</span>
                </div>
              </div>
            )}
          </div>
        </div>
        <div className="toolbar-center">
          {!mountedDevice && !physicalDiscActive && (
            sourceImagePath
              ? <button className="btn-open btn-close-disc" onClick={ejectImage}>Close Disc Image</button>
              : <button className="btn-open" onClick={openImage}>Open Disc Image</button>
          )}
          {mountedDevice
            ? <button className="btn-open btn-open-secondary btn-unmount" onClick={unmountImage}>Unmount Disc Image</button>
            : sourceImagePath && isMountable(sourceImagePath, platform)
              ? <button className="btn-open btn-open-secondary" onClick={mountImage}>Mount Disc Image</button>
              : null
          }
          {platform === "linux" && sourceImagePath && isCdemuEmulatable(sourceImagePath) && (
            <button className="btn-open btn-open-secondary" onClick={emulateDrive} disabled={emulating}>
              {emulating ? "Loading…" : "Emulate Drive"}
            </button>
          )}
          <div className="drive-menu-wrap" ref={driveMenuRef}>
            {physicalDiscActive
              ? <>
                  <button className="btn-open btn-open-secondary btn-unmount" onClick={unmountPhysicalDisc}>Unmount Disc</button>
                  <button className="btn-open btn-open-secondary btn-unmount btn-eject" onClick={ejectDisc} title="Eject disc">⏏</button>
                </>
              : !sourceImagePath && <button className="btn-open btn-open-secondary" onClick={openDisc}>Open Disc from Drive</button>
            }
            {showDriveMenu && (
              <div className="drive-menu">
                {loadingDrives ? (
                  <div className="drive-menu-item drive-menu-loading">Detecting drives…</div>
                ) : drives.length === 0 ? (
                  <div className="drive-menu-item drive-menu-empty">No optical drives found</div>
                ) : (
                  drives.map((d) => (
                    <div key={d.device_path} className="drive-menu-item" onClick={() => selectDrive(d)}>
                      <span className="drive-item-name">{d.name}</span>
                      <span className={`drive-item-disc ${d.has_disc ? "" : "drive-item-disc--empty"}`}>
                        {d.has_disc ? (d.volume_name || "Disc inserted") : "No disc"}
                      </span>
                    </div>
                  ))
                )}
              </div>
            )}
          </div>
          {!mountedDevice && !physicalDiscActive && !sourceImagePath && <div className="drive-menu-wrap" ref={dumpDriveMenuRef}>
            <button className="btn-open btn-open-secondary" onClick={openDumpDriveMenu}>Dump Disc from Drive</button>
            {showDumpDriveMenu && (
              <div className="drive-menu">
                {loadingDrives ? (
                  <div className="drive-menu-item drive-menu-loading">Detecting drives…</div>
                ) : drives.length === 0 ? (
                  <div className="drive-menu-item drive-menu-empty">No optical drives found</div>
                ) : (
                  drives.map((d) => (
                    <div key={d.raw_device_path} className={`drive-menu-item${!d.has_disc ? " drive-menu-item--disabled" : ""}`}
                         onClick={() => d.has_disc && selectDumpDrive(d)}>
                      <span className="drive-item-name">{d.name}</span>
                      <span className={`drive-item-disc ${d.has_disc ? "" : "drive-item-disc--empty"}`}>
                        {d.has_disc ? (d.volume_name || "Disc inserted") : "No disc"}
                      </span>
                    </div>
                  ))
                )}
              </div>
            )}
          </div>}

          {imagePath && (viewMode === "filesystem" || audioEntries.some((e) => !e.is_data)) && (
            <>
              <button className="btn-dump" onClick={() => dumpContents()} title="Extract all disc contents to a folder">
                Extract All Contents
              </button>
              {viewMode === "filesystem" && selected.size > 0 && (
                <button className="btn-dump" onClick={saveSelected} title="Save the ticked files/folders to a folder">
                  Save Selected ({selected.size})
                </button>
              )}
              {wiiuConvInfo?.is_wiiu && (
                <div className="wiiu-convert" onMouseLeave={() => setWiiuMenuOpen(false)}>
                  <button
                    className="btn-dump"
                    onClick={() => setWiiuMenuOpen((o) => !o)}
                    disabled={convRunning}
                    title="Repackage this Wii U disc image to a raw .wud or .iso (byte-identical; encryption state preserved)"
                  >
                    Convert ▾
                  </button>
                  {wiiuMenuOpen && (
                    <div className="wiiu-convert-menu">
                      <button onClick={() => convertCurrentWiiu("wud")}>Convert to .wud</button>
                      <button onClick={() => convertCurrentWiiu("iso")}>Convert to .iso</button>
                      {wiiuConvInfo.is_raw && (
                        <>
                          <button onClick={convertCurrentWiiuWux}>Compress to .wux</button>
                          <label
                            className="wiiu-convert-verify"
                            onClick={(e) => e.stopPropagation()}
                          >
                            <input
                              type="checkbox"
                              checked={wuxVerify}
                              onChange={(e) => setWuxVerify(e.target.checked)}
                            />
                            Verify after compress
                          </label>
                        </>
                      )}
                    </div>
                  )}
                </div>
              )}
              {ps3Info?.is_ps3 && (
                <button
                  className="btn-dump"
                  onClick={convertCurrentPs3}
                  disabled={!ps3Info.has_key || convRunning}
                  title={ps3Info.has_key
                    ? `${ps3Info.encrypted ? "Decrypt" : "Encrypt"} this PS3 ISO using ${ps3Info.key_path?.split(/[/\\]/).pop()}`
                    : "Place an .ird, .key or .dkey file with the same name beside this ISO to enable"}
                >
                  {ps3Info.encrypted ? "Decrypt" : "Encrypt"}
                </button>
              )}
              {physicalDiscActive && !mountedDevice && (
                <button className="btn-dump" onClick={async () => { if (!dumpOutputPath) setDumpOutputPath(await downloadDir()); setShowDumpModal(true); }} title="Dump disc to image files">
                  Dump Disc
                </button>
              )}
            </>
          )}
          {imagePath && viewMode === "filesystem" && (
            <button className="btn-icon btn-icon--up" onClick={navigateUp} disabled={currentPath === "/"} title="Up"><Icon name="arrow-up" /></button>
          )}
          {imagePath && damagedRanges.length > 0 && (
            <button className="btn-icon btn-icon--warn" onClick={buildDamagedReport} title="Damaged-sector report — files in unreadable areas"><Icon name="warning" /></button>
          )}
          {imagePath && latestDateEnabled && (
            <button
              className="btn-icon btn-icon--cal"
              title="Latest Date Finder — PVD volume dates + newest file/folder date on the whole disc"
              onClick={() => {
                setDateReport("loading");
                invoke<DateReport>("disc_date_report", { imagePath, filesystem: null })
                  .then(setDateReport)
                  .catch((e) => { setDateReport(null); setError(String(e)); });
              }}
            >
              <Icon name="calendar" />
            </button>
          )}
          {imagePath && viewMode === "filesystem" && currentPath === "/" && (
            <button className="btn-icon btn-icon--export" onClick={exportFileList} title="Export file list (CSV / JSON / TXT / DFXML)">
              <Icon name="export-list" />
            </button>
          )}
          {sourceImagePath && (
            <button
              className="btn-icon"
              onClick={() => {
                invoke("open_sector_view_window", { imagePath: sourceImagePath, lba: 0, compareImagePath: null }).catch(() => {});
              }}
              title="Sector View (opens in its own window)"
            ><Icon name="search" /></button>
          )}
        </div>
        <div className="toolbar-right">
          <button ref={settingsGearRef} className={`btn-settings${showSettings ? " btn-settings--open" : ""}`} title="Settings" onClick={() => setShowSettings(s => !s)}>
            <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
              <path fillRule="evenodd" d="M10.25,4.71L10.36,1.63L13.64,1.63L13.75,4.71A7.5,7.5,0,0,1,15.92,5.61L18.17,3.51L20.5,5.83L18.4,8.08A7.5,7.5,0,0,1,19.29,10.25L22.37,10.36L22.37,13.64L19.29,13.75A7.5,7.5,0,0,1,18.4,15.92L20.5,18.17L18.17,20.5L15.92,18.4A7.5,7.5,0,0,1,13.75,19.29L13.64,22.37L10.36,22.37L10.25,19.29A7.5,7.5,0,0,1,8.08,18.4L5.83,20.5L3.51,18.17L5.61,15.92A7.5,7.5,0,0,1,4.71,13.75L1.63,13.64L1.63,10.36L4.71,10.25A7.5,7.5,0,0,1,5.61,8.08L3.51,5.83L5.83,3.51L8.08,5.61A7.5,7.5,0,0,1,10.25,4.71ZM15.5,12A3.5,3.5,0,0,0,8.5,12A3.5,3.5,0,0,0,15.5,12Z" />
            </svg>
          </button>
        </div>
      </div>
      {showSettings && (
        <div className="settings-panel" ref={settingsRef}>
          <div className="settings-col">
            <div className="settings-row">
              <span className="settings-label">Default Download Location</span>
              <button className="btn-open btn-open-secondary settings-path-btn" onClick={pickDownloadLocation}>
                {defaultDownloadPath || "Not set — click to choose"}
              </button>
            </div>
            <div className="settings-row">
              <span className="settings-label">Theme</span>
              <div className="settings-radio-group">
                {(["system", "light", "dark"] as const).map(t => (
                  <label key={t} className="settings-radio">
                    <input type="radio" name="theme" value={t} checked={theme === t} onChange={() => setTheme(t)} />
                    {t.charAt(0).toUpperCase() + t.slice(1)}
                  </label>
                ))}
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label">Save Audio (PCM) as</span>
              <div className="settings-radio-group">
                {(["wav", "flac", "mp3"] as const).map(fmt => (
                  <label key={fmt} className="settings-radio">
                    <input type="radio" name="audioFormat" value={fmt} checked={audioFormat === fmt} onChange={() => setAudioFormat(fmt)} />
                    .{fmt}
                  </label>
                ))}
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label">Select checkboxes</span>
              <label className="settings-radio" title="Adds a checkbox to every row so several files or folders can be saved in one go. Off by default, since most extractions are a single item.">
                <input
                  type="checkbox"
                  checked={showSelect}
                  onChange={(e) => setShowSelect(e.target.checked)}
                />
                Show, for saving several items at once
              </label>
            </div>
            <div className="settings-row">
              <span className="settings-label">Mac filename encoding</span>
              <div className="settings-radio-group">
                {([
                  ["auto", "Auto", "Work it out from the names on the disc — right for almost every disc."],
                  ["roman", "Mac OS Roman", "Force the Western encoding, including accented European names."],
                  ["shift-jis", "Shift-JIS", "Force Japanese."],
                ] as const).map(([mode, label, help]) => (
                  <label key={mode} className="settings-radio" title={help}>
                    <input
                      type="radio"
                      name="hfsEncoding"
                      checked={hfsEncoding === mode}
                      onChange={() => setHfsEncoding(mode)}
                    />
                    {label}
                  </label>
                ))}
              </div>
            </div>
            <div className="settings-row settings-row--stack">
              <span className="settings-label">Gap handling</span>
              <div className="settings-radio-group settings-radio-group--stack">
                {([
                  ["previous", "Append gaps to previous track", "The gap before a track is written at the end of the track before it. Nothing is lost. This is what Exact Audio Copy does by default."],
                  ["next", "Append gaps to next track", "The gap is written at the start of the track it introduces — right when a disc hides an intro in the gap."],
                  ["leave-out", "Leave out gaps", "Gap sectors are not written at all. Tracks start clean, but that audio is discarded."],
                ] as const).map(([mode, label, help]) => (
                  <label key={mode} className="settings-radio" title={help}>
                    <input
                      type="radio"
                      name="gapMode"
                      checked={gapMode === mode}
                      onChange={() => setGapMode(mode)}
                    />
                    {label}
                  </label>
                ))}
              </div>
            </div>
          </div>
          <div className="settings-col">
            <div className="settings-row">
              <span className="settings-label" title="Apple/Mac hybrid discs store resource forks as ISO9660 associated files. Hide them, list them as separate “.[R]” entries, or preserve them on extraction as AppleDouble “._NAME” sidecars (IsoBuster-style).">Mac resource forks</span>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input type="radio" name="resourceForks" checked={forkMode === "hide"} onChange={() => setForkMode("hide")} />
                  Hide
                </label>
                <label className="settings-radio">
                  <input type="radio" name="resourceForks" checked={forkMode === "list"} onChange={() => setForkMode("list")} />
                  List as .[R]
                </label>
                <label className="settings-radio">
                  <input type="radio" name="resourceForks" checked={forkMode === "appledouble"} onChange={() => setForkMode("appledouble")} />
                  AppleDouble
                </label>
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label" title="CD-ROM XA (Mode 2) streaming files on CD-i, Video CD, CD Extra, Saturn and PlayStation discs can be written three ways. “Ask” prompts when an extraction actually contains some. “File content” is each sector's user data (playable MPEG); “Subheader” keeps it at 2336 bytes/sector (needed by XA-ADPCM audio, matches dumpsxiso); “Raw” writes whole 2352-byte sectors (matches what Windows returns).">CD-XA extraction</span>
              <div className="settings-radio-group settings-radio-group--wrap">
                {([["Ask", "ask"], ["File content", 0], ["Subheader", 1], ["Raw", 2]] as [string, "ask" | 0 | 1 | 2][]).map(([label, val]) => (
                  <label key={String(val)} className="settings-radio">
                    <input type="radio" name="xaDefault" checked={xaDefault === val} onChange={() => setXaDefault(val)} />
                    {label}
                  </label>
                ))}
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label" title="Adds a toolbar button that reports the PVD volume dates and the newest file/folder date on the disc — handy for dating a mastering.">Latest Date Finder <Icon name="calendar" /></span>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="checkbox"
                    checked={latestDateEnabled}
                    onChange={(e) => {
                      setLatestDateEnabled(e.target.checked);
                      localStorage.setItem("latestDateEnabled", String(e.target.checked));
                    }}
                  />
                  Show toolbar button
                </label>
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label">{redumperLabel}</span>
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <div className="settings-radio-group">
                  {(["internal", "external"] as const).map(src => (
                    <label key={src} className="settings-radio">
                      <input
                        type="radio"
                        name="redumperSource"
                        value={src}
                        checked={redumperSource === src}
                        onChange={() => handleRedumperSourceChange(src)}
                      />
                      {src.charAt(0).toUpperCase() + src.slice(1)}
                    </label>
                  ))}
                </div>
                {redumperSource === "external" && (
                  <button className="btn-open btn-open-secondary settings-path-btn" onClick={pickRedumperExternal}>
                    {redumperExternalPath || "Not set — click to choose"}
                  </button>
                )}
              </div>
            </div>
            <div className="settings-row">
              <span className="settings-label">Wii U Common Key</span>
              <button className="btn-open btn-open-secondary settings-path-btn" onClick={pickWiiuKey}>
                {wiiuKeyPath ? wiiuKeyPath.split("/").pop() : "Not set — click to choose"}
              </button>
            </div>
            <div className="settings-row">
              <span className="settings-label">Open Source Notices</span>
              <button className="btn-open btn-open-secondary settings-path-btn" onClick={() => setShowLicenses(true)}>
                View licenses
              </button>
            </div>
          </div>
        </div>
      )}

      {showDamagedReport && (
        <div className="modal-overlay" onClick={() => setShowDamagedReport(false)}>
          <div className="modal damaged-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Damaged sectors</span>
              <button className="modal-close" onClick={() => setShowDamagedReport(false)}>✕</button>
            </div>
            <div className="modal-body">
              <div className="damage-map" title="Disc layout — red marks unreadable/missing sectors">
                {damageBuckets(240).map((bad, i) => (
                  <span key={i} className={`damage-cell${bad ? " damage-cell--bad" : ""}`} />
                ))}
              </div>
              <div className="damage-summary">
                {damagedTotal.toLocaleString()} sectors · {damagedRanges.length.toLocaleString()} damaged range{damagedRanges.length !== 1 ? "s" : ""}
                {damagedFiles && <> · {damagedFiles.length.toLocaleString()} affected file{damagedFiles.length !== 1 ? "s" : ""}</>}
              </div>
              {damagedFiles === null ? (
                <div className="damage-summary">Scanning files…</div>
              ) : damagedFiles.length === 0 ? (
                <div className="damage-summary">No files fall in the damaged areas (the gaps are outside the filesystem's files).</div>
              ) : (
                <div className="damage-list">
                  {damagedFiles.map((f) => (
                    <div
                      key={f.path}
                      className="damage-file"
                      onClick={() => {
                        const dir = f.path.substring(0, f.path.lastIndexOf("/")) || "/";
                        setShowDamagedReport(false);
                        if (imagePath) loadDirectory(imagePath, dir);
                      }}
                      title="Go to folder"
                    >
                      <span className="damage-file-path">{f.path}</span>
                      <span className="damage-file-meta">LBA {f.lba} · {f.size.toLocaleString()} B</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {showLicenses && (
        <div className="modal-overlay" onClick={() => setShowLicenses(false)}>
          <div className="modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Open Source Notices</span>
              <button className="modal-close" onClick={() => setShowLicenses(false)}>✕</button>
            </div>
            <div className="modal-body">
              <p className="license-package">libFLAC — FLAC audio encoding</p>
              <pre className="license-text">{`Copyright (C) 2000-2009  Josh Coalson
Copyright (C) 2011-2016  Xiph.Org Foundation

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

- Redistributions of source code must retain the above copyright
  notice, this list of conditions and the following disclaimer.

- Redistributions in binary form must reproduce the above copyright
  notice, this list of conditions and the following disclaimer in the
  documentation and/or other materials provided with the distribution.

- Neither the name of the Xiph.org Foundation nor the names of its
  contributors may be used to endorse or promote products derived from
  this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
\`\`AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE FOUNDATION
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.`}</pre>
              <p className="license-package" style={{ marginTop: "16px" }}>LAME — MP3 audio encoding</p>
              <pre className="license-text">{`Copyright (c) 1999-2011 The L.A.M.E. project

LAME is licensed under the GNU Lesser General Public License (LGPL)
version 2 or later. This application is licensed under GPL v3, which
is compatible with and satisfies the requirements of the LGPL.

Source: https://lame.sourceforge.io`}</pre>
              <p className="license-package" style={{ marginTop: "16px" }}>chd-rs — CHD (Compressed Hunks of Data) decompression</p>
              <pre className="license-text">{`Copyright (c) 2022 Ronny Chan

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in
   the documentation and/or other materials provided with the
   distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived
   from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.`}</pre>
              <p className="license-package" style={{ marginTop: "16px" }}>libflac-sys — Rust bindings for libFLAC</p>
              <pre className="license-text">{`Copyright (c) 2020 Matthias Geier. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in
   the documentation and/or other materials provided with the
   distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived
   from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.`}</pre>

              <p className="license-package" style={{ marginTop: "16px" }}>redumper — disc dumping engine</p>
              <pre className="license-text">{`Copyright (c) 2020-2024 superg and contributors.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
   this list of conditions and the following disclaimer.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.

Source: https://github.com/superg/redumper`}</pre>
              <p className="license-package" style={{ marginTop: "16px" }}>SabreTools.Serialization — format documentation reference</p>
              <pre className="license-text">{`Copyright (c) Matt Nadareski and contributors.
Licensed under the GNU Lesser General Public License (LGPL) v2.1 or later.
https://github.com/SabreTools/SabreTools.Serialization

Format documentation for disc image and filesystem types in this project
was cross-referenced against SabreTools.Serialization. No source code
from this project is included; all parsers are independent implementations
derived from the underlying format specifications.`}</pre>

              <p className="license-package" style={{ marginTop: "16px" }}>Aaru (Aaru.Filesystems, Aaru.Images) — format documentation reference</p>
              <pre className="license-text">{`Copyright (c) Natalia Portillo and contributors.
Licensed under the GNU Lesser General Public License (LGPL) v2.1 or later.
https://github.com/aaru-dps/Aaru

Format documentation for disc image and filesystem types in this project
was cross-referenced against Aaru. No source code from this project is
included; all parsers are independent implementations derived from the
underlying format specifications.`}</pre>
            </div>
          </div>
        </div>
      )}

      {showCdemuPrompt && (
        <div className="modal-overlay">
          <div className="modal cdemu-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">CDemu Not Installed</span>
            </div>
            <div className="modal-body">
              <p>CDemu is required to mount certain disc image formats on Linux (.cue, .mds, .nrg, and others).</p>
              <p>Would you like to install it now?</p>
              {cdemuInstalling && <p className="cdemu-status">Installing… (a system password prompt may appear)</p>}
              {cdemuInstallMsg && (
                <p className={cdemuInstallOk ? "cdemu-status cdemu-ok" : "cdemu-status cdemu-err"}>{cdemuInstallMsg}</p>
              )}
            </div>
            <div className="modal-footer">
              {!cdemuInstallOk && (
                <button className="btn-open" onClick={installCdemu} disabled={cdemuInstalling}>
                  {cdemuInstalling ? "Installing…" : "Install"}
                </button>
              )}
              <button className="btn-open btn-open-secondary" onClick={() => setShowCdemuPrompt(false)}>
                {cdemuInstallOk ? "Done" : "Not Now"}
              </button>
            </div>
          </div>
        </div>
      )}

      {showDumpModal && (
        <div className="modal-overlay" onClick={() => { if (!dumpRunning) setShowDumpModal(false); }}>
          <div className="modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Dump Disc</span>
              {!dumpRunning && (
                <button className="modal-close" onClick={() => setShowDumpModal(false)}>✕</button>
              )}
            </div>
            <div className="modal-body">
              <div className="settings-row" style={{ marginBottom: 8 }}>
                <span className="settings-label">Drive / Device</span>
                <input
                  className="settings-input"
                  value={dumpDrive}
                  onChange={e => setDumpDrive(e.target.value)}
                  placeholder={platform === "windows" ? "D:" : "/dev/sr0"}
                  disabled={dumpRunning}
                />
              </div>
              <div className="settings-row" style={{ marginBottom: 8 }}>
                <span className="settings-label">Output Folder</span>
                <button
                  className="btn-open btn-open-secondary settings-path-btn"
                  onClick={pickDumpOutput}
                  disabled={dumpRunning}
                >
                  {dumpOutputPath || "Not set — click to choose"}
                </button>
              </div>
              <div className="settings-row" style={{ marginBottom: 8 }}>
                <span className="settings-label">
                  <label style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer" }}>
                    <input
                      type="checkbox"
                      checked={dumpCreateSubfolder}
                      onChange={e => setDumpCreateSubfolder(e.target.checked)}
                      disabled={dumpRunning}
                    />
                    Create Subfolder
                  </label>
                </span>
                <input
                  className="settings-input"
                  value={dumpSubfolder}
                  onChange={e => setDumpSubfolder(e.target.value)}
                  disabled={!dumpCreateSubfolder || dumpRunning}
                  style={{ opacity: dumpCreateSubfolder ? 1 : 0.4 }}
                />
              </div>
              {dumpLog.length > 0 && (
                <div className="dump-log" ref={dumpLogRef}>
                  {dumpLog.map((line, i) => <div key={i}>{line}</div>)}
                </div>
              )}
            </div>
            <div className="modal-footer">
              {dumpRunning ? (
                <button className="btn-open btn-open-secondary" onClick={cancelDump}>Cancel</button>
              ) : (
                <>
                  <button
                    className="btn-open"
                    onClick={startDump}
                    disabled={!dumpDrive || !dumpOutputPath || (dumpCreateSubfolder && !dumpSubfolder)}
                  >
                    Start Dump
                  </button>
                  <button className="btn-open btn-open-secondary" onClick={() => setShowDumpModal(false)}>Close</button>
                </>
              )}
            </div>
          </div>
        </div>
      )}

      {wiiuBatchPaths && (
        <div className="modal-overlay" onClick={() => setWiiuBatchPaths(null)}>
          <div className="modal conv-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Convert {wiiuBatchPaths.length} Wii U images</span>
              <button className="modal-close" onClick={() => setWiiuBatchPaths(null)}>✕</button>
            </div>
            {(() => {
              // Hide a target format if every dropped file is already in it —
              // converting a file to its own format is a no-op. So an all-.wux
              // batch hides "Compressed .wux", an all-.wud batch hides "Raw .wud", etc.
              const allWud = wiiuBatchPaths.every(p => /\.wud$/i.test(p));
              const allIso = wiiuBatchPaths.every(p => /\.iso$/i.test(p));
              const allWux = wiiuBatchPaths.every(p => /\.wux$/i.test(p));
              return (
                <div className="modal-body">
                  <div style={{ fontSize: 13, marginBottom: 12, opacity: 0.85 }}>
                    Choose the output format. You'll pick an output folder next.
                  </div>
                  <div style={{ display: "flex", gap: 8, marginBottom: allWux ? 0 : 12, justifyContent: "center" }}>
                    {!allWud && <button className="btn-open" onClick={() => runWiiuBatch("wud")}>Raw .wud</button>}
                    {!allIso && <button className="btn-open" onClick={() => runWiiuBatch("iso")}>Raw .iso</button>}
                    {!allWux && <button className="btn-open" onClick={() => runWiiuBatch("wux")}>Compressed .wux</button>}
                  </div>
                  {!allWux && (
                    <label style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 6, cursor: "pointer", fontSize: 12, opacity: 0.85 }}>
                      <input
                        type="checkbox"
                        checked={wiiuBatchVerify}
                        onChange={e => setWiiuBatchVerify(e.target.checked)}
                      />
                      Verify after compress (.wux only)
                    </label>
                  )}
                </div>
              );
            })()}
            <div className="modal-footer">
              <button className="btn-open btn-open-secondary" onClick={() => setWiiuBatchPaths(null)}>Cancel</button>
            </div>
          </div>
        </div>
      )}

      {showBatch && (
        <div className="modal-overlay" onClick={() => { if (!convRunning) setShowBatch(false); }}>
          <div className="modal batch-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Batch Convert</span>
              {!convRunning && <button className="modal-close" onClick={() => setShowBatch(false)}>✕</button>}
            </div>
            <div className="modal-body">
              {/* The window took three "Choose…" dialogs to get going. Dropping a
                  folder or a single image is the faster path, and it needs to be
                  visible or nobody will discover it. */}
              <div
                className={`batch-drop${batchDragOver ? " batch-drop--over" : ""}`}
                onClick={() => { if (!convRunning) void pickBatchFolder("src"); }}
              >
                Drop a folder, an image or a cue sheet here to convert it
              </div>

              {([
                ["Source", batchSrc, "src", "A folder, a single image, or a cue sheet"],
                ["Output folder", batchOut, "out", "Where converted images are written"],
                ["Keys folder (PS3)", batchKeys, "keys", "PS3 keys: .ird, .dkey or .key. Matched by file name, or by the title ID inside an IRD when the names differ. Wii U repackaging needs no key."],
              ] as const).map(([label, value, which, help]) => (
                <div key={which} className="batch-row" title={help}>
                  <span className="batch-label">{label}</span>
                  <span className="batch-path">{value || <em>not set</em>}</span>
                  <button className="btn-open btn-open-secondary" disabled={convRunning}
                    onClick={() => pickBatchFolder(which)}>Choose…</button>
                </div>
              ))}

              {/* Auto is what the window used to do implicitly, and stays the
                  default: every image goes to whatever its uncompressed form is.
                  The named targets are for going the other way, or for forcing
                  one output format across a mixed folder. */}
              <div className="batch-row" title="What each image is converted into">
                <span className="batch-label">Convert to</span>
                <select className="batch-select" value={batchTarget} disabled={convRunning}
                  onChange={(e) => {
                    setBatchTarget(e.target.value);
                    localStorage.setItem("batchTarget", e.target.value);
                    void scanBatch(batchSrc, batchOut, batchKeys, batchRecursive, batchConflict, e.target.value);
                  }}>
                  <option value="auto">Auto (uncompress, or PS3 decrypt)</option>
                  <option value="iso">ISO</option>
                  <option value="cso">CSO (compressed ISO)</option>
                  <option value="wux">WUX (compressed Wii U)</option>
                  <option value="merge">CUE/BIN: one BIN, tracks indexed (also extracts CHD)</option>
                  <option value="split">CUE/BIN: one BIN per track (also extracts CHD)</option>
                </select>
              </div>

              <div className="batch-row">
                <span className="batch-label">Options</span>
                <label className="settings-radio">
                  <input type="checkbox" checked={batchRecursive} disabled={convRunning}
                    onChange={(e) => { setBatchRecursive(e.target.checked); void scanBatch(batchSrc, batchOut, batchKeys, e.target.checked); }} />
                  Include subfolders
                </label>
              </div>

              <div className="batch-row">
                <span className="batch-label">If output exists</span>
                <div className="settings-radio-group">
                  {(["rename", "skip", "overwrite"] as const).map((c) => (
                    <label key={c} className="settings-radio">
                      <input type="radio" name="batchConflict" checked={batchConflict === c} disabled={convRunning}
                        onChange={() => { setBatchConflict(c); void scanBatch(batchSrc, batchOut, batchKeys, batchRecursive, c); }} />
                      {c === "rename" ? "Rename" : c === "skip" ? "Skip" : "Overwrite"}
                    </label>
                  ))}
                </div>
              </div>

              {batchError && <div className="error" style={{ margin: "8px 0" }}>{batchError}</div>}
              {batchScanning && <div className="batch-summary">Scanning…</div>}

              {batchPlan && !batchScanning && (() => {
                const runnable = batchPlan.items.filter(i => !i.problem).length;
                const short = batchPlan.free_space > 0 && batchPlan.bytes_needed > batchPlan.free_space;
                return (
                  <>
                    <div className="batch-summary">
                      Found {batchPlan.items.length} image{batchPlan.items.length === 1 ? "" : "s"} — {runnable} ready to convert
                    </div>
                    {(batchPlan.missing_keys > 0 || batchPlan.conflicts > 0 || short) && (
                      <div className="batch-warn">
                        {batchPlan.missing_keys > 0 && <div>⚠ {batchPlan.missing_keys} without a key — set a keys folder, or they will be skipped</div>}
                        {batchPlan.conflicts > 0 && <div>⚠ {batchPlan.conflicts} would replace an existing file</div>}
                        {short && <div>⚠ Needs {fmtBytes(batchPlan.bytes_needed)}, only {fmtBytes(batchPlan.free_space)} free</div>}
                      </div>
                    )}
                    <div className="batch-list">
                      {batchPlan.items.map((i) => (
                        <div key={i.path} className={`batch-item${i.problem ? " batch-item--skip" : ""}`}>
                          <span className="batch-item-op">{i.op}</span>
                          <span className="batch-item-name" title={i.path}>{i.name}</span>
                          <span className="batch-item-note">
                            {i.problem ?? (i.key_path ? `key: ${i.key_path.split(/[/\\]/).pop()}` : fmtBytes(i.out_size))}
                          </span>
                        </div>
                      ))}
                    </div>
                  </>
                );
              })()}

              {batchSummary && (
                <div className={`batch-result${batchSummary.failed ? " batch-result--bad" : ""}`}>
                  {batchSummary.text}
                </div>
              )}
            </div>
            <div className="modal-footer" style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              {batchLog.length > 0 && (
                <button className="btn-open btn-open-secondary"
                  onClick={() => navigator.clipboard.writeText(batchLog.join("\n"))}>Copy log</button>
              )}
              {!convRunning && (batchSrc || batchOut || batchKeys || batchPlan || batchSummary) && (
                <button className="btn-open btn-open-secondary" onClick={clearBatch}>Clear</button>
              )}
              {convRunning ? (
                <button className="btn-open btn-open-secondary" onClick={cancelConversion} disabled={convCancelling}>
                  {convCancelling ? "Cancelling…" : "Cancel"}
                </button>
              ) : (
                <button className="btn-open" disabled={!batchPlan || batchPlan.items.every(i => i.problem)}
                  onClick={startBatch}>Start</button>
              )}
            </div>
          </div>
        </div>
      )}

      {showConvModal && (
        <div className="modal-overlay" onClick={() => { if (!convRunning) setShowConvModal(false); }}>
          <div className="modal conv-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Image Conversion</span>
              {!convRunning && <button className="modal-close" onClick={() => setShowConvModal(false)}>✕</button>}
            </div>
            <div className="modal-body" ref={convListRef}>
              {convJobs.map((j, i) => {
                const pct = j.total > 0 ? Math.floor((j.done / j.total) * 100) : 0;
                const label = j.status === "error" ? "Failed"
                  : j.status === "done" ? "Done"
                  : j.status === "running" ? `${pct}%` : "Queued";
                return (
                  <div key={i} data-job={i} style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 12 }}>
                    <div style={{ display: "flex", justifyContent: "space-between", gap: 12, fontSize: 13 }}>
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {j.kind === "ps3" ? (j.encrypt ? "Encrypt" : "Decrypt")
                          : j.kind === "tocso" ? "Compress"
                          : j.kind === "toraw" ? "Convert"
                          : j.kind === "toiso" ? "Convert"
                          : j.kind === "merge" ? "Merge"
                          : j.kind === "split" ? "Split"
                          : j.kind === "chdcue" || j.kind === "chdsplit" ? "Extract"
                          : j.kind === "wux" ? "Compress"
                          : "Convert"}: {j.name}
                      </span>
                      <span style={{ flexShrink: 0, opacity: 0.8 }}>{label}</span>
                    </div>
                    <div style={{ height: 6, background: "rgba(127,127,127,0.3)", borderRadius: 3, overflow: "hidden" }}>
                      <div style={{
                        height: "100%",
                        width: `${j.status === "done" ? 100 : pct}%`,
                        background: j.status === "error" ? "#d9534f" : "#4caf50",
                        transition: "width 0.2s",
                      }} />
                    </div>
                    {j.status === "error" && j.error && (
                      <div style={{ fontSize: 12, color: "#d9534f" }}>{j.error}</div>
                    )}
                    {j.status === "done" && (
                      <div style={{ fontSize: 12, opacity: 0.7, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        → {j.outPath.split(/[/\\]/).pop()}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
            <div className="modal-footer">
              {convRunning ? (
                <button
                  className="btn-open btn-open-secondary"
                  onClick={cancelConversion}
                  disabled={convCancelling}
                >
                  {convCancelling ? "Cancelling…" : "Cancel"}
                </button>
              ) : (
                <button className="btn-open btn-open-secondary" onClick={() => setShowConvModal(false)}>
                  Close
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {showExtractModal && (
        <div className="modal-overlay" onClick={() => { if (!extractRunning) setShowExtractModal(false); }}>
          <div className="modal conv-modal extract-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header" style={{ display: "flex", justifyContent: "center", alignItems: "center", gap: 10, borderBottom: extractDone ? "none" : undefined }}>
              <span className="modal-title">{extractDone ? "Finished" : "Extracting"}</span>
              {!extractDone && <span className="extract-spinner" />}
            </div>
            {!extractDone && extractStatus && (
              <div className="extract-status">{extractStatus}</div>
            )}
            {extractRunning && extractCancellable && (
              <div className="modal-footer">
                <button className="btn-open btn-open-secondary" onClick={cancelExtraction} disabled={extractCancelling}>
                  {extractCancelling ? "Cancelling…" : "Cancel"}
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {xaPrompt && (
        <div className="modal-overlay" onClick={() => setXaPrompt(null)}>
          <div className="modal conv-modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header" style={{ display: "flex", justifyContent: "center", alignItems: "center" }}>
              <span className="modal-title">CD-XA files found</span>
            </div>
            <div className="modal-body" style={{ textAlign: "left" }}>
              This disc holds <strong>{xaPrompt.count.toLocaleString()}</strong> CD-ROM XA streaming
              file{xaPrompt.count !== 1 ? "s" : ""} (audio or video). They can be written three ways —
              pick one. Everything else on the disc is unaffected.
              <div className="xa-choices">
                {([
                  ["File content", "Each sector's user data — 2048 bytes from Form 1 sectors, 2324 from Form 2. Reproduces the file exactly; video comes out as a playable MPEG stream.", 0],
                  ["Keep subheader", "A flat 2336 bytes per sector, retaining the subheader and EDC. XA-ADPCM audio needs the subheader's channel and coding bytes; also what dumpsxiso produces.", 1],
                  ["Raw sectors", "Whole 2352-byte sectors, sync and header included. This is what Windows returns for a Form 2 file, so use it to byte-match a copy made through a CD drive or emulator.", 2],
                ] as [string, string, number][]).map(([label, desc, mode]) => (
                  <button key={mode} className="xa-choice" onClick={() => {
                    const p = xaPrompt;
                    if (xaRemember) setXaDefault(mode as 0 | 1 | 2);
                    setXaPrompt(null);
                    p.run(mode);
                  }}>
                    <span className="xa-choice-label">{label}{mode === 0 ? " (recommended)" : ""}</span>
                    <span className="xa-choice-desc">{desc}</span>
                  </button>
                ))}
              </div>
            </div>
            <div className="modal-footer" style={{ justifyContent: "center", alignItems: "center", gap: 16 }}>
              <button className="btn-open btn-open-secondary" onClick={() => setXaPrompt(null)}>Cancel</button>
              <label className="empty-notice-skip">
                <input type="checkbox" checked={xaRemember} onChange={(e) => setXaRemember(e.target.checked)} />
                Remember this choice
              </label>
            </div>
          </div>
        </div>
      )}

      {emptyFileNotice && (
        <div className="modal-overlay" onClick={() => setEmptyFileNotice(null)}>
          <div className="modal conv-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header" style={{ display: "flex", justifyContent: "center", alignItems: "center" }}>
              <span className="modal-title">Empty file</span>
            </div>
            <div className="modal-body">
              <strong>{emptyFileNotice}</strong> holds 0 bytes on the disc, so the saved file is empty.
              <br />
              This is how it was mastered — not a damaged or failed download.
            </div>
            <div className="modal-footer" style={{ justifyContent: "center", alignItems: "center", gap: 16 }}>
              <button className="btn-open" onClick={() => setEmptyFileNotice(null)}>CLOSE</button>
              <label className="empty-notice-skip">
                <input
                  type="checkbox"
                  checked={skipEmptyFileNotice}
                  onChange={(e) => {
                    setSkipEmptyFileNotice(e.target.checked);
                    localStorage.setItem("skipEmptyFileNotice", String(e.target.checked));
                  }}
                />
                Don't remind me again.
              </label>
            </div>
          </div>
        </div>
      )}

      {dateReport && (
        <div className="modal-overlay" onClick={() => { if (dateReport !== "loading") setDateReport(null); }}>
          <div className="modal conv-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header" style={{ display: "flex", justifyContent: "center", alignItems: "center", gap: 10 }}>
              <span className="modal-title">{dateReport === "loading" ? "Scanning dates" : "Disc dates"}</span>
              {dateReport === "loading" && <span className="extract-spinner" />}
            </div>
            {dateReport !== "loading" && (
              <>
                <div className="modal-body date-report">
                  <div><span className="date-report-label">PVD created</span>{dateReport.pvd_created || "—"}</div>
                  <div><span className="date-report-label">PVD modified</span>{dateReport.pvd_modified || "—"}</div>
                  <div><span className="date-report-label">Newest entry</span>{dateReport.latest_date || "—"}</div>
                  {dateReport.latest_path && (
                    <div><span className="date-report-label">&nbsp;</span><span className="date-report-path">{dateReport.latest_path}</span></div>
                  )}
                  <div><span className="date-report-label">Entries scanned</span>{dateReport.entries_scanned.toLocaleString()}</div>
                </div>
                <div className="modal-footer" style={{ justifyContent: "center" }}>
                  <button className="btn-open" onClick={() => setDateReport(null)}>CLOSE</button>
                </div>
              </>
            )}
          </div>
        </div>
      )}

      {ctxMenu && (
        <div
          className="context-overlay"
          onClick={() => setCtxMenu(null)}
          onContextMenu={(e) => { e.preventDefault(); setCtxMenu(null); }}
        >
          <div className="context-menu" style={{ left: ctxMenu.x, top: ctxMenu.y }} onClick={(e) => e.stopPropagation()}>
            {ctxMenu.items.map((item) => (
              <button
                key={item.label}
                className="context-menu-item"
                title={item.title}
                onClick={() => { setCtxMenu(null); item.run(); }}
              >
                <span className="context-menu-icon"><Icon name="download" /></span> {item.label}
              </button>
            ))}
          </div>
        </div>
      )}

      {emulatedDrives.length > 0 && (
        <div className="emulated-drives-bar">
          {emulatedDrives.map(drive => (
            <div key={drive.slot} className="emulated-drive-item">
              <span className="emulated-drive-device">{drive.device}</span>
              <span className="emulated-drive-name">{drive.image_path.split("/").pop()}</span>
              <button className="btn-eject-emulated" onClick={() => ejectEmulatedDrive(drive.slot)} title="Unload virtual drive">⏏</button>
            </div>
          ))}
        </div>
      )}

      {(imagePath || viewMode === "empty-drive") && (
        <div className="breadcrumb">
          {breadcrumbs.map((crumb, i) => (
            <span key={crumb.path}>
              {i > 0 && <span className="breadcrumb-sep">›</span>}
              <span
                className={`breadcrumb-item ${i === breadcrumbs.length - 1 ? "breadcrumb-item--active" : ""}`}
                onClick={() => imagePath && i < breadcrumbs.length - 1 && loadDirectory(imagePath, crumb.path)}
              >{crumb.label}</span>
            </span>
          ))}
          {signatureStatus && (() => {
            const [label, counts] = signatureStatus.split("|");
            const tip = label === "Signed"
              ? `This 3DO disc's RSA signature verifies against the retail key.${counts
                  ? ` ${counts} signed payloads (OS, boot code, misc code, splash) also verified.`
                  : ""}`
              : label === "Unsigned"
              ? "This 3DO disc carries a placeholder where its signature should be, so it was never signed."
              : "This 3DO disc has a signature, but it does not verify — the disc was altered after signing, or signed with a different key.";
            return (
              <span
                className={`breadcrumb-signed breadcrumb-signed--${label === "Signed" ? "signed" : label === "Unsigned" ? "unsigned" : "invalid"}`}
                title={tip}
              >{label}</span>
            );
          })()}
        </div>
      )}

      <div className="main">
        {imagePath && (
          <div className="sidebar" style={{ width: sidebarWidth }}>
            {tree.map((node) => (
              <TreeItem key={node.path} node={node} imagePath={imagePath}
                selectedPath={sidebarPath} selectedFs={activeFilesystem} onSelect={handleTreeSelect}
                onToggle={handleTreeToggle} onNodeContextMenu={handleTreeContextMenu} depth={0}
                volumeLabel={volumeLabel} />
            ))}
          </div>
        )}
        {imagePath && (
          <div
            className="sidebar-resizer"
            onMouseDown={onSidebarResizeStart}
            title="Drag to resize"
          />
        )}

        <div className="content-col">
        {(viewMode === "filesystem" ? entries.length > 0 : audioEntries.length > 0) && (
          <div className="table-head-wrap" ref={headWrapRef}>
            <table className={`file-table${showSelect ? "" : " file-table--nosel"}`} style={{ tableLayout: "fixed" }}>
              <colgroup>
                {/* Name is left unsized so it takes whatever is left over. With
                    table-layout:fixed a table wider than its columns hands the
                    surplus to every column in proportion, which inflated Size
                    and left a long tail of empty Modified before the download
                    arrow. Only the flexible column should grow. */}
                {cols.map((c) => (
                  <col key={c.key} style={c.key === "name" ? undefined : { width: colWidths[c.key] }} />
                ))}
              </colgroup>
              <thead>
                <tr>
                  {cols.map((c) => (
                    <th key={c.key} className={`col-${c.key}`}>
                      {c.key !== "save" && <span className="th-label">{c.label}</span>}
                      {/* Holds the width the row save buttons occupy, so the
                          select-all box lands directly above the row boxes. */}
                      {c.key === "save" && viewMode === "filesystem" && showSelect && (
                        <span className="th-save-spacer" aria-hidden="true" />
                      )}
                      {c.key === "save" && viewMode === "filesystem" && showSelect && (
                        <input
                          type="checkbox"
                          className="row-check"
                          title="Select all"
                          checked={entries.length > 0 && selected.size === entries.length}
                          onChange={(e) => setSelected(e.target.checked ? new Set(entries.map((en) => en.name)) : new Set())}
                        />
                      )}
                      {c.key !== "name" && (
                        <div className="resize-handle" onMouseDown={(e) => onResizeStart(c.key, e)} />
                      )}
                    </th>
                  ))}
                </tr>
              </thead>
            </table>
          </div>
        )}
        <div className="content" ref={contentRef}>
          {warn && <div className="warn">{warn}</div>}
          {error && <div className="error">{error}</div>}

          {!imagePath && viewMode !== "empty-drive" && (
            <div className="empty-state">
              <img src={appIcon} className="empty-icon" style={{ width: 240, height: 240, opacity: 0.85, marginBottom: 24, borderRadius: 40, userSelect: "none", pointerEvents: "none", WebkitUserSelect: "none" }} />
            </div>
          )}

          {viewMode === "empty-drive" && emptyDriveName && (
            <div className="empty-state">
              <div className="empty-icon"><Icon name="disc-data" /></div>
              <p>Optical disc drive is empty</p>
              <span className="empty-drive-name">{emptyDriveName}</span>
            </div>
          )}

          {(viewMode === "filesystem" ? entries.length > 0 : audioEntries.length > 0) && (
            <table className={`file-table${showSelect ? "" : " file-table--nosel"}`} style={{ tableLayout: "fixed" }}>
              <colgroup>
                {/* Name is left unsized so it takes whatever is left over. With
                    table-layout:fixed a table wider than its columns hands the
                    surplus to every column in proportion, which inflated Size
                    and left a long tail of empty Modified before the download
                    arrow. Only the flexible column should grow. */}
                {cols.map((c) => (
                  <col key={c.key} style={c.key === "name" ? undefined : { width: colWidths[c.key] }} />
                ))}
              </colgroup>
              <tbody>
                {viewMode === "audio"
                  ? audioEntries.map((entry) => (
                      <tr
                        key={entry.track_number}
                        className={entry.is_data ? "row-data" : "row-audio"}
                        onContextMenu={(e) => { if (!entry.is_data) openDownloadMenu(e, [
                          { label: "Download", run: () => saveAudioTrack(entry) },
                          { label: "Copy name", run: () => { navigator.clipboard?.writeText(cdTextTitle(entry.track_number) ?? entry.name); } },
                        ]); }}
                        onDoubleClick={() => entry.is_data && imagePath && loadDirectory(imagePath, "/")}
                      >
                        <td className="col-name">
                          {entry.is_data ? (
                            <span className="entry-icon"><Icon name="disc" /></span>
                          ) : (
                            <button
                              className={`btn-play${playingTrack === entry.track_number ? " btn-play--active" : ""}`}
                              title={audioLoading === entry.track_number ? "Loading…"
                                : playingTrack === entry.track_number && isPlaying ? "Pause" : "Play"}
                              // The row for the track already loaded is the same
                              // control as the transport's: pressing it again
                              // pauses rather than restarting the track.
                              onClick={() => (playingTrack === entry.track_number ? togglePlay() : playTrack(entry))}
                              disabled={audioLoading !== null}
                            >{audioLoading === entry.track_number ? "…"
                              : <Icon name={playingTrack === entry.track_number && isPlaying ? "pause" : "play"} />}</button>
                          )}
                          {entry.is_data ? entry.name : (cdTextTitle(entry.track_number) ?? entry.name)}
                        </td>
                        <td className="col-size">{entry.is_data ? formatSize(entry.size_bytes) : formatDuration(entry.num_sectors)}</td>
                        <td className="col-lba">{entry.start_lba.toLocaleString()}</td>
                        <td className="col-modified">{entry.format}</td>
                        {showTrackSave && (
                          <td className="col-save">
                            {entry.is_data
                              ? <button className="btn-save" title="Extract this track's files to a folder" onClick={() => dumpContents({ withAudio: false })}><Icon name="download" /></button>
                              : <button className="btn-save" title="Save as WAV" onClick={() => saveAudioTrack(entry)}><Icon name="download" /></button>}
                          </td>
                        )}
                      </tr>
                    ))
                  : entries.map((entry) => (
                      <tr
                        key={`${entry.lba}-${entry.name}`}
                        className={entry.is_dir ? "row-dir" : "row-file"}
                        onContextMenu={(e) => openDownloadMenu(e, [
                          { label: "Download", run: () => saveEntry(entry) },
                          { label: "Copy name", run: () => { navigator.clipboard?.writeText(entry.name); } },
                          ...(entry.is_xa ? [{
                            label: "Download as XA",
                            title: "Keep the 8-byte subheader and EDC (2336 bytes/sector). XA-ADPCM audio needs the subheader's channel and coding bytes; this also matches dumpsxiso.",
                            run: () => saveEntry(entry, 1),
                          }, {
                            label: "Download raw",
                            title: "Whole 2352-byte sectors, sync and header included. This is what Windows hands back for a Form 2 file, so use it to byte-match a copy made through a CD drive or emulator.",
                            run: () => saveEntry(entry, 2),
                          }] : []),
                        ])}
                        onDoubleClick={() => {
                          if (!imagePath) return;
                          const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;
                          if (entry.is_dir) {
                            loadDirectory(imagePath, entryPath);
                          } else if (isNestedImage(entry.name)) {
                            setStatusText(`Extracting ${entry.name}…`);
                            invoke<string>("extract_nested_image", { imagePath, filePath: entryPath, filesystem: activeFilesystem || null })
                              .then((tempPath) => openImageAtPath(tempPath))
                              .catch((e) => setStatusText(String(e)));
                          } else if (isPreviewable(entry.name)) {
                            setStatusText(`Opening ${entry.name}…`);
                            invoke("open_file_preview", { imagePath, filePath: entryPath, filesystem: activeFilesystem || null })
                              .then(() => setStatusText(`Opened ${entry.name} (read-only preview)`))
                              .catch((e) => setStatusText(String(e)));
                          }
                        }}
                      >
                        <td className="col-name">
                          <span className="entry-icon"><Icon name={entry.is_dir ? "folder" : fileIcon(entry.name)} /></span>
                          {entry.name}
                          {isDamaged(entry) && (
                            <span className="entry-damaged" title="Located in unreadable/missing sectors — may be incomplete or corrupt when extracted">
                              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
                                <path d="M2.5 2.5 L13.5 13.5 M13.5 2.5 L2.5 13.5" stroke="#ff3b30" strokeWidth="2.5" strokeLinecap="round" fill="none" />
                              </svg>
                            </span>
                          )}
                          {entry.deleted && (
                            <span className="entry-deleted" title="Deleted entry — directory record remains; contents recovered on a best-effort basis">
                              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
                                <path d="M2.5 4 H13.5 M6 4 V2.5 h4 V4 M4 4 l0.8 9.2 a1.2 1.2 0 0 0 1.2 1.1 h4 a1.2 1.2 0 0 0 1.2-1.1 L12 4 M6.4 6.5 v5.5 M9.6 6.5 v5.5" stroke="#9a9aa0" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" fill="none" />
                              </svg>
                            </span>
                          )}
                        </td>
                        {/* A zero-byte file's extent is never read, and mastering
                            tools leave filler there — the number is noise. */}
                        <td className="col-size">{entry.is_dir ? "—" : entry.size_bytes.toLocaleString()}</td>
                        <td className="col-lba">{(entry.is_dir && entry.lba === 0) || (!entry.is_dir && entry.size_bytes === 0) ? "—" : entry.lba}</td>
                        <td className="col-modified">{entry.modified}</td>
                        <td className="col-save">
                          <button className="btn-save" title={entry.is_dir ? "Save folder" : "Save file"} onClick={() => saveEntryAsking(entry)}><Icon name="download" /></button>
                          {showSelect && <input
                            type="checkbox"
                            className="row-check"
                            checked={selected.has(entry.name)}
                            onChange={() => setSelected((prev) => {
                              const s = new Set(prev);
                              if (s.has(entry.name)) s.delete(entry.name); else s.add(entry.name);
                              return s;
                            })}
                            onDoubleClick={(e) => e.stopPropagation()}
                          />}
                        </td>
                      </tr>
                    ))
                }
              </tbody>
            </table>
          )}

          {imagePath && viewMode === "filesystem" && entries.length === 0 && !error && (
            <div className="empty-dir">Empty folder</div>
          )}
        </div>
        </div>
      </div>

      {audioUrl && (
        <div className="audio-player">
          <span className="audio-player-label"><Icon name="music" /> {playingTrack !== null ? `Track ${String(playingTrack).padStart(2, "0")}` : "Track"}</span>
          {/* Transport is ours rather than the browser's: the native audio controls
              differ per platform and offer 15-second seek and playback-rate buttons,
              neither of which suits a disc of songs. The element itself stays, hidden,
              as the actual decoder. Keyed on the URL so switching tracks builds a
              fresh element: assigning a new src to a playing <audio> doesn't reliably
              reload in WebKit. */}
          <audio
            key={audioUrl}
            ref={(el) => { audioElRef.current = el; if (el) el.volume = audioVolume; }}
            className="audio-player-el" src={audioUrl} autoPlay
            onEnded={() => { setIsPlaying(false); playNextTrack(); }}
            onPlay={() => setIsPlaying(true)}
            onPause={() => setIsPlaying(false)}
            onTimeUpdate={(e) => setAudioPos(e.currentTarget.currentTime)}
            onLoadedMetadata={(e) => { setAudioDur(e.currentTarget.duration || 0); setAudioPos(0); }}
          />
          <span className="audio-player-transport">
            <button
              className="audio-player-btn" title="Previous track" onClick={skipBack}
            >⏮</button>
            <button
              className="audio-player-btn audio-player-btn--play"
              title={isPlaying ? "Pause" : "Play"} onClick={togglePlay}
            ><Icon name={isPlaying ? "pause" : "play"} /></button>
            <button
              className="audio-player-btn" title="Next track"
              disabled={!adjacentTrack(1)} onClick={() => stepTrack(1)}
            >⏭</button>
          </span>
          <input
            className="audio-player-seek" type="range" min={0} max={audioDur || 0} step={0.01}
            value={Math.min(audioPos, audioDur || 0)}
            onChange={(e) => {
              const t = Number(e.target.value);
              setAudioPos(t);
              if (audioElRef.current) audioElRef.current.currentTime = t;
            }}
          />
          <span className="audio-player-time">{fmtTime(audioPos)} / {fmtTime(audioDur)}</span>
          <span className="audio-player-volume" title={`Volume ${Math.round(audioVolume * 100)}%`}>
            <span className="audio-player-volume-icon"><Icon name={audioVolume === 0 ? "muted" : "volume"} /></span>
            <input
              type="range" min={0} max={1} step={0.01} value={audioVolume}
              onChange={(e) => setAudioVolume(Number(e.target.value))}
            />
          </span>
          <button
            className={`audio-player-toggle${autoAdvance ? " audio-player-toggle--on" : ""}`}
            title={autoAdvance ? "Continuous play is on — the next track follows automatically" : "Continuous play is off — playback stops at the end of this track"}
            onClick={() => setAutoAdvance((v) => !v)}
          ><Icon name="repeat" /></button>
          <button className="audio-player-close" title="Close player" onClick={closePlayer}>✕</button>
        </div>
      )}

      <div className="statusbar">
        <span className="statusbar-left">{statusText}</span>
        {/* Tauri's webview swallows target="_blank" anchors; route through the opener plugin. */}
        <a
          className={`statusbar-brand${supportSeen ? "" : " statusbar-brand--support"}`}
          href={SUPPORT_URL}
          title={supportSeen ? undefined : "Support development"}
          onClick={(e) => {
            e.preventDefault();
            openUrl(SUPPORT_URL);
            if (appVersion) localStorage.setItem(`supportSeen_${appVersion}`, "1");
            setSupportSeen(true);
          }}
        >
          {supportSeen ? "whatever industries" : <>whatever industrie<span className="statusbar-brand-dollar">$</span></>}
        </a>
        <span className="statusbar-right">
          <span className="statusbar-version" title="Release notes" onClick={() => openUrl("https://github.com/whatever-industries/disc-xplorer/releases")}>{appVersion ? `v${appVersion}` : ""}</span>
        </span>
      </div>
    </div>
  );
}

export default App;
