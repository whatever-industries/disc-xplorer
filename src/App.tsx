import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";

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

interface ConvJob {
  kind: "ps3" | "wiiu" | "wux";
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

function formatDuration(sectors: number): string {
  if (sectors === 0) return "—";
  const totalSeconds = Math.floor(sectors / 75);
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  const f = sectors % 75;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(f).padStart(2, "0")}`;
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

function TreeItem({
  node, imagePath, selectedPath, onSelect, onToggle, depth,
}: {
  node: TreeNode; imagePath: string; selectedPath: string;
  onSelect: (path: string) => void; onToggle: (path: string) => void; depth: number;
}) {
  const isAudio = node.nodeType === "audio_track";
  const isSession = node.nodeType === "session";
  const isDataTrack = node.nodeType === "data_track";
  const isFilesystem = node.nodeType === "filesystem";

  const icon = isSession || isDataTrack ? "📀"
    : isAudio ? "🎵"
    : isFilesystem ? "🗂️"
    : node.nodeType === "dir" ? "📁"
    : "💿";

  const alwaysExpanded = isSession;
  const noArrow = isAudio || isFilesystem || alwaysExpanded;

  function handleClick() {
    onSelect(node.path);
    if (!isAudio && !isFilesystem && !isSession) onToggle(node.path);
  }

  return (
    <div>
      <div
        className={[
          "tree-item",
          node.path === selectedPath ? "tree-item--selected" : "",
          isAudio ? "tree-item--audio" : "",
          isSession ? "tree-item--session" : "",
          isFilesystem ? "tree-item--filesystem" : "",
        ].filter(Boolean).join(" ")}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={handleClick}
      >
        <span className="tree-arrow">
          {noArrow ? " " : (node.children === null ? " " : node.expanded ? "▾" : "▶")}
        </span>
        <span className="tree-icon">{icon}</span>
        <span className="tree-label">{node.name}</span>
      </div>
      {(alwaysExpanded || node.expanded) && node.children && (
        <div>
          {node.children.map((child) => (
            <TreeItem key={child.path} node={child} imagePath={imagePath}
              selectedPath={selectedPath} onSelect={onSelect} onToggle={onToggle}
              depth={depth + 1} />
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
  const [sidebarPath, setSidebarPath] = useState<string>("");
  // Contiguous LBA ranges (inclusive) of unreadable/missing sectors, for flagging
  // files located in damaged areas (e.g. partial dumps). Fetched async per image.
  const [damagedRanges, setDamagedRanges] = useState<[number, number][]>([]);
  const [damagedTotal, setDamagedTotal] = useState<number>(0);
  const [showDamagedReport, setShowDamagedReport] = useState(false);
  const [damagedFiles, setDamagedFiles] = useState<{ path: string; size: number; lba: number }[] | null>(null);
  // In-app audio playback: the WAV blob URL + which track it belongs to.
  const [audioUrl, setAudioUrl] = useState<string | null>(null);
  const [playingTrack, setPlayingTrack] = useState<number | null>(null);
  const [audioLoading, setAudioLoading] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warn, setWarn] = useState<string | null>(null);
  const [statusText, setStatusText] = useState("No disc loaded");
  const [mountedDevice, setMountedDevice] = useState<string | null>(null);
  const [physicalDiscActive, setPhysicalDiscActive] = useState(false);
  const [drives, setDrives] = useState<DriveInfo[]>([]);
  const [showDriveMenu, setShowDriveMenu] = useState(false);
  const [showDumpDriveMenu, setShowDumpDriveMenu] = useState(false);
  const [loadingDrives, setLoadingDrives] = useState(false);
  const [colWidths, setColWidths] = useState<ColWidths>({
    name: 280, lba: 80, size: 110, modified: 160, save: 56,
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
  const [convJobs, setConvJobs] = useState<ConvJob[]>([]);
  const [convRunning, setConvRunning] = useState(false);
  const convCancelledRef = useRef(false);
  const [convCancelling, setConvCancelling] = useState(false);
  const [showExtractModal, setShowExtractModal] = useState(false);
  const [extractRunning, setExtractRunning] = useState(false);
  const [extractCancelling, setExtractCancelling] = useState(false);
  const [extractDone, setExtractDone] = useState(false);
  const [extractCancellable, setExtractCancellable] = useState(false);
  const [showSectorView, setShowSectorView] = useState(false);
  const [sectorViewPopout, setSectorViewPopout] = useState<boolean>(
    () => localStorage.getItem("sectorViewPopout") === "true"
  );
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
  const driveMenuRef = useRef<HTMLDivElement>(null);
  const dumpDriveMenuRef = useRef<HTMLDivElement>(null);
  const settingsRef = useRef<HTMLDivElement>(null);
  const settingsGearRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    invoke<string>("get_platform").then(setPlatform);
  }, []);

  useEffect(() => {
    localStorage.setItem("sectorViewPopout", String(sectorViewPopout));
  }, [sectorViewPopout]);

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

  // A file is "damaged" if its sector extent overlaps any missing-sector range.
  function isDamaged(entry: DiscEntry): boolean {
    if (entry.is_dir || damagedRanges.length === 0 || entry.lba <= 0) return false;
    const sectors = Math.max(1, Math.ceil(entry.size_bytes / 2048));
    const start = entry.lba;
    const end = entry.lba + sectors - 1;
    // Ranges are sorted ascending; linear scan is fine for typical counts.
    for (const [s, e] of damagedRanges) {
      if (s > end) break;
      if (e >= start) return true;
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
        if (!onPath) return { name: nm, path: nodePath, nodeType: "dir", children: null, expanded: false };
        if (depth + 1 === segs.length) {
          // This node is the current folder: show its subdirs (collapsed).
          const kids = currentSubdirs.filter((e) => e.is_dir)
            .map((e): TreeNode => ({ name: e.name, path: `${nodePath}/${e.name}`, nodeType: "dir", children: null, expanded: false }));
          return { name: nm, path: nodePath, nodeType: "dir", children: kids.length ? kids : null, expanded: kids.length > 0 };
        }
        // On-path ancestor: recurse.
        const children = await buildLevel(nodePath, depth + 1, await listSubdirNames(nodePath));
        return { name: nm, path: nodePath, nodeType: "dir", children, expanded: true };
      }));

    const rootNames = segs.length === 0
      ? currentSubdirs.filter((e) => e.is_dir).map((e) => e.name)
      : await listSubdirNames("/");
    const topChildren = await buildLevel("/", 0, rootNames);
    if (navIdRef.current !== myId) return;

    setSidebarPath(segs.length === 0 ? fsPath : `/${segs.join("/")}`);
    if (!fsPath) return;
    setTree((prev) => {
      let found = false;
      const swap = (nodes: TreeNode[]): TreeNode[] => nodes.map((n) => {
        if (n.nodeType === "filesystem") {
          if (n.path === fsPath) { found = true; return { ...n, expanded: true, children: topChildren }; }
          return { ...n, expanded: false };
        }
        if (n.children) return { ...n, children: swap(n.children) };
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
  // Currently handles PS3 ISOs (.iso + .dkey/.key); other key-based formats
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
      if (!keyPath) { jobs.push({ ...base, status: "error", error: "No matching .key/.dkey found" }); continue; }
      const encrypt = !info.encrypted;
      jobs.push({ ...base, keyPath, encrypt, outPath: convOutPath(img, outDir, encrypt) });
    }
    return jobs;
  }

  async function runConversionJobs(jobs: ConvJob[]) {
    if (jobs.length === 0) return;
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
    for (let i = 0; i < jobs.length; i++) {
      if (jobs[i].status === "error") continue; // pre-flagged (unsupported / no key)
      if (convCancelledRef.current) {
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "error", error: "Cancelled" } : j)));
        continue;
      }
      // Prompt before clobbering an existing file; skip just this job if declined.
      if (await invoke<boolean>("path_exists", { path: jobs[i].outPath })) {
        const name = jobs[i].outPath.split(/[/\\]/).pop() ?? jobs[i].outPath;
        const overwrite = await confirm(`"${name}" already exists. Overwrite it?`, {
          title: "File already exists",
          kind: "warning",
        });
        if (!overwrite) {
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
        } else if (jobs[i].kind === "wux") {
          await invoke("wiiu_compress_wux", {
            inPath: jobs[i].inPath,
            outPath: jobs[i].outPath,
            job: i,
            verify: jobs[i].verify ?? false,
          });
        }
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "done", done: j.total || 1, total: j.total || 1 } : j)));
      } catch (e) {
        const msg = String(e).includes("__cancelled__") ? "Cancelled" : String(e);
        setConvJobs((prev) => prev.map((j, idx) => (idx === i ? { ...j, status: "error", error: msg } : j)));
      }
    }
    setConvRunning(false);
    unlistenPs3();
    unlistenWiiu();
  }

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
  async function runExtraction(
    command: "save_file" | "save_directory",
    args: Record<string, unknown>,
    cancellable: boolean, // folder saves can be cancelled between files; single files can't
  ) {
    setExtractDone(false);
    setExtractCancelling(false);
    setExtractCancellable(cancellable);
    setShowExtractModal(true);
    setExtractRunning(true);
    try {
      await invoke(command, args);
      setExtractDone(true); // flash "Finished"
      window.setTimeout(() => setShowExtractModal(false), 900);
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("__cancelled__")) setError(msg);
      setShowExtractModal(false);
    } finally {
      setExtractRunning(false);
    }
  }

  async function cancelExtraction() {
    setExtractCancelling(true);
    try { await invoke("extract_cancel"); } catch { /* nothing running */ }
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

      // Show audio tracks on initial open; user navigates to data tracks via sidebar.
      const audio = buildAudioEntries(tracks);
      const audioCount = audio.filter((e) => !e.is_data).length;
      if (audio.length > 0) {
        navIdRef.current++;
        setAudioEntries(audio);
        setEntries([]);
        setViewMode("audio");
        setStatusText(`${audioCount} audio track${audioCount !== 1 ? "s" : ""}${audio.length > audioCount ? `, ${audio.length - audioCount} data track` : ""}`);
      } else {
        // Data-only disc: load the filesystem immediately.
        await loadDirectory(path, "/");
      }
    } else {
      setCueTracks([]);
      setSidebarPath("/");

      const filesystems = await invoke<string[]>("get_disc_filesystems", { imagePath: path }).catch(() => ["ISO 9660"]);
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
      const firstFs = filesystems[0] ?? "ISO 9660";
      const firstFsPath = `__fs_${firstFs.toLowerCase().replace(/ /g, "_")}`;
      setSidebarPath(firstFsPath);
      // loadDirectory's syncSidebarTree expands the first filesystem node with
      // its folder tree.
      await loadDirectory(path, "/", firstFs, firstFs);
    }
  }

  async function openImage() {
    const selected = await open({
      filters: [{ name: "Disc Images", extensions: ["iso", "img", "bin", "fatx", "chd", "cue", "mds", "mdx", "nrg", "ccd", "cdi", "gdi", "toc", "b5t", "b6t", "bwt", "c2d", "pdi", "gi", "daa", "cso", "ciso", "ecm", "wbfs", "wux", "wud", "scram", "sdram", "sbram", "aif", "cif", "uif", "skeleton", "zst"] }],
    });
    if (!selected) return;
    await openImageAtPath(selected as string);
  }

  async function handleDrop(dropped: string[]) {
    const isos = dropped.filter((p) => p.toLowerCase().endsWith(".iso"));
    const keys = dropped.filter((p) => /\.(key|dkey)$/i.test(p));
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

    const supported = ["iso", "img", "chd", "cue", "mds", "mdx", "nrg", "ccd", "cdi", "gdi", "toc", "b5t", "b6t", "bwt", "c2d", "pdi", "gi", "daa", "cso", "ciso", "ecm", "wbfs", "wux", "wud", "scram", "sdram", "sbram", "aif", "cif", "uif", "skeleton", "skeleton.zst", "iso.zst", "img.zst"];
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

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "drop") {
        setIsDragOver(false);
        handleDrop(event.payload.paths);
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
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

  async function dumpContents() {
    if (!imagePath) return;
    const destPath = await open({ directory: true, title: "Choose destination for disc contents" });
    if (!destPath) return;
    const volName = (tree[0]?.name ?? imageName).replace(/\.[^/.]+$/, "") || "disc";
    await runExtraction("save_directory", {
      imagePath,
      dirPath: "/",
      destPath: `${destPath}/${volName}`,
      filesystem: activeFilesystem || null,
      appleDouble: forkModeRef.current === "appledouble",
    }, true);
  }


  async function handleTreeToggle(nodePath: string) {
    if (!imagePath) return;

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

    if (nodePath.startsWith("__fs_") && cueTracks.length === 0) {
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
        if (node.path !== nodePath) {
          return { ...node, children: node.children ? await expandNode(node.children) : null };
        }
        if (node.expanded) return { ...node, expanded: false };
        let children = node.children;
        if (children === null) {
          const result = await invoke<DiscEntry[]>("list_disc_contents", { imagePath, dirPath: nodePath, showResourceForks: forkModeRef.current === "list" });
          children = result
            .filter((e) => e.is_dir)
            .map((e): TreeNode => ({
              name: e.name,
              path: nodePath === "/" ? `/${e.name}` : `${nodePath}/${e.name}`,
              nodeType: "dir",
              children: null,
              expanded: false,
            }));
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

  function handleTreeSelect(path: string) {
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

    if (path.startsWith("__audio_")) {
      setSidebarPath(path);
      const trackNum = parseInt(path.replace("__audio_", ""), 10);
      const track = cueTracks.find((t) => t.number === trackNum && !t.is_data);
      if (track) {
        navIdRef.current++;
        const audio = buildAudioEntries([track]);
        setAudioEntries(audio);
        setEntries([]);
        setViewMode("audio");
        setCurrentPath(path);
        setStatusText(`Track ${String(track.number).padStart(2, "0")} — ${track.mode}`);
      }
      return;
    }

    if (path.startsWith("__track_")) {
      setSidebarPath(path);
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
      loadDirectory(imagePath, path);
    }
  }

  async function saveEntry(entry: DiscEntry) {
    if (!imagePath) return;
    const entryPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;

    if (entry.is_dir) {
      const base = defaultDownloadPath || await open({ directory: true, title: `Choose destination for "${entry.name}"` }) as string | null;
      if (!base) return;
      await runExtraction("save_directory", { imagePath, dirPath: entryPath, destPath: `${base}/${entry.name}`, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble" }, true);
    } else {
      const destPath = defaultDownloadPath
        ? `${defaultDownloadPath}/${entry.name}`
        : await save({ defaultPath: entry.name });
      if (!destPath) return;
      await runExtraction("save_file", { imagePath, filePath: entryPath, destPath, filesystem: activeFilesystem || null, appleDouble: forkModeRef.current === "appledouble" }, false);
    }
  }

  async function saveAudioTrack(entry: AudioEntry) {
    if (!imagePath) return;
    const ext = audioFormat;
    const destPath = defaultDownloadPath
      ? `${defaultDownloadPath}/${entry.name}.${ext}`
      : await save({
          defaultPath: `${entry.name}.${ext}`,
          filters: [{ name: ext === "flac" ? "FLAC Audio" : ext === "mp3" ? "MP3 Audio" : "WAV Audio", extensions: [ext] }],
        });
    if (!destPath) return;
    try {
      await invoke("save_audio_track", {
        cuePath: imagePath,
        trackNumber: entry.track_number,
        destPath,
        format: ext,
      });
    } catch (e) { setError(String(e)); }
  }

  // Decode an audio track to WAV and load it into the player bar (autoplays).
  async function playTrack(entry: AudioEntry) {
    if (!imagePath || entry.is_data) return;
    setAudioLoading(entry.track_number);
    try {
      const buf = await invoke<ArrayBuffer>("audio_track_wav", { cuePath: imagePath, trackNumber: entry.track_number });
      const url = URL.createObjectURL(new Blob([buf], { type: "audio/wav" }));
      setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return url; });
      setPlayingTrack(entry.track_number);
    } catch (e) {
      setError(String(e));
    } finally {
      setAudioLoading(null);
    }
  }

  function closePlayer() {
    setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return null; });
    setPlayingTrack(null);
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
    { key: "lba", label: "LBA" },
    { key: "size", label: "Size" },
    { key: "modified", label: "Modified" },
    { key: "save", label: "Save" },
  ];

  const showAudioSave = audioEntries.some(e => !e.is_data);

  const audioCols: { key: keyof ColWidths; label: string }[] = [
    { key: "name", label: "Track" },
    { key: "lba", label: "Start Sector" },
    { key: "size", label: "Duration" },
    { key: "modified", label: "Format" },
    ...(showAudioSave ? [{ key: "save" as keyof ColWidths, label: "Save" }] : []),
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
        standalone
      />
    );
  }

  return (
    <div className="app">
      {isDragOver && (
        <div className="drag-overlay">
          <div className="drag-overlay-inner">
            <div className="drag-overlay-icon">💿</div>
            <p>Drop disc image to open</p>
          </div>
        </div>
      )}
      <div className="toolbar">
        <div className="toolbar-left" />
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

          {imagePath && viewMode === "filesystem" && (
            <>
              <button className="btn-dump" onClick={dumpContents} title="Extract all disc contents to a folder">
                Extract All Contents
              </button>
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
                    : "Place a .key or .dkey file with the same name beside this ISO to enable"}
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
            <button className="btn-icon" onClick={navigateUp} disabled={currentPath === "/"} title="Up">↑</button>
          )}
          {imagePath && damagedRanges.length > 0 && (
            <button className="btn-icon btn-icon--warn" onClick={buildDamagedReport} title="Damaged-sector report — files in unreadable areas">✕</button>
          )}
          {imagePath && viewMode === "filesystem" && currentPath === "/" && (
            <button className="btn-icon" onClick={exportFileList} title="Export file list (CSV / JSON / TXT / DFXML)">≡</button>
          )}
          {sourceImagePath && (
            <button
              className="btn-icon"
              onClick={() => {
                if (sectorViewPopout) {
                  invoke("open_sector_view_window", { imagePath: sourceImagePath, lba: 0, compareImagePath: null }).catch(() => {});
                } else {
                  setShowSectorView(true);
                }
              }}
              title={sectorViewPopout ? "Sector View (opens in separate window)" : "Sector View"}
            >🔍</button>
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
              <span className="settings-label">Sector View window default</span>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="sectorViewMode"
                    checked={!sectorViewPopout}
                    onChange={() => setSectorViewPopout(false)}
                  />
                  Integrated
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="sectorViewMode"
                    checked={sectorViewPopout}
                    onChange={() => setSectorViewPopout(true)}
                  />
                  Pop-out
                </label>
              </div>
            </div>
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
          </div>
          <div className="settings-col">
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

      {showConvModal && (
        <div className="modal-overlay" onClick={() => { if (!convRunning) setShowConvModal(false); }}>
          <div className="modal conv-modal" onClick={e => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">Image Conversion</span>
              {!convRunning && <button className="modal-close" onClick={() => setShowConvModal(false)}>✕</button>}
            </div>
            <div className="modal-body">
              {convJobs.map((j, i) => {
                const pct = j.total > 0 ? Math.floor((j.done / j.total) * 100) : 0;
                const label = j.status === "error" ? "Failed"
                  : j.status === "done" ? "Done"
                  : j.status === "running" ? `${pct}%` : "Queued";
                return (
                  <div key={i} style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 12 }}>
                    <div style={{ display: "flex", justifyContent: "space-between", gap: 12, fontSize: 13 }}>
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {j.kind === "ps3" ? (j.encrypt ? "Encrypt" : "Decrypt")
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

      {showSectorView && sourceImagePath && (
        <SectorView imagePath={sourceImagePath} onClose={() => setShowSectorView(false)} />
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
        </div>
      )}

      <div className="main">
        {imagePath && (
          <div className="sidebar">
            {tree.map((node) => (
              <TreeItem key={node.path} node={node} imagePath={imagePath}
                selectedPath={sidebarPath} onSelect={handleTreeSelect}
                onToggle={handleTreeToggle} depth={0} />
            ))}
          </div>
        )}

        <div className="content">
          {warn && <div className="warn">{warn}</div>}
          {error && <div className="error">{error}</div>}

          {!imagePath && viewMode !== "empty-drive" && (
            <div className="empty-state">
              <img src={appIcon} className="empty-icon" style={{ width: 240, height: 240, opacity: 0.85, marginBottom: 24, borderRadius: 40, userSelect: "none", pointerEvents: "none", WebkitUserSelect: "none" }} />
            </div>
          )}

          {viewMode === "empty-drive" && emptyDriveName && (
            <div className="empty-state">
              <div className="empty-icon">📀</div>
              <p>Optical disc drive is empty</p>
              <span className="empty-drive-name">{emptyDriveName}</span>
            </div>
          )}

          {(viewMode === "filesystem" ? entries.length > 0 : audioEntries.length > 0) && (
            <table className="file-table" style={{ tableLayout: "fixed" }}>
              <colgroup>
                {cols.map((c) => <col key={c.key} style={{ width: colWidths[c.key] }} />)}
              </colgroup>
              <thead>
                <tr>
                  {cols.map((c) => (
                    <th key={c.key} className={`col-${c.key}`}>
                      <span className="th-label">{c.label}</span>
                      <div className="resize-handle" onMouseDown={(e) => onResizeStart(c.key, e)} />
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {viewMode === "audio"
                  ? audioEntries.map((entry) => (
                      <tr
                        key={entry.track_number}
                        className={entry.is_data ? "row-data" : "row-audio"}
                        onDoubleClick={() => entry.is_data && imagePath && loadDirectory(imagePath, "/")}
                      >
                        <td className="col-name">
                          {entry.is_data ? (
                            <span className="entry-icon">💿</span>
                          ) : (
                            <button
                              className={`btn-play${playingTrack === entry.track_number ? " btn-play--active" : ""}`}
                              title={audioLoading === entry.track_number ? "Loading…" : "Play"}
                              onClick={() => playTrack(entry)}
                              disabled={audioLoading !== null}
                            >{audioLoading === entry.track_number ? "…" : "▶"}</button>
                          )}
                          {entry.name}
                        </td>
                        <td className="col-lba">{entry.start_lba.toLocaleString()}</td>
                        <td className="col-size">{entry.is_data ? formatSize(entry.size_bytes) : formatDuration(entry.num_sectors)}</td>
                        <td className="col-modified">{entry.format}</td>
                        {showAudioSave && (
                          <td className="col-save">
                            {!entry.is_data && <button className="btn-save" title="Save as WAV" onClick={() => saveAudioTrack(entry)}>⬇</button>}
                          </td>
                        )}
                      </tr>
                    ))
                  : entries.map((entry) => (
                      <tr
                        key={`${entry.lba}-${entry.name}`}
                        className={entry.is_dir ? "row-dir" : "row-file"}
                        onDoubleClick={() => {
                          if (!entry.is_dir || !imagePath) return;
                          const newPath = currentPath === "/" ? `/${entry.name}` : `${currentPath}/${entry.name}`;
                          loadDirectory(imagePath, newPath);
                        }}
                      >
                        <td className="col-name">
                          <span className="entry-icon">{entry.is_dir ? "📁" : "📄"}</span>
                          {entry.name}
                          {isDamaged(entry) && (
                            <span className="entry-damaged" title="Located in unreadable/missing sectors — may be incomplete or corrupt when extracted">✕</span>
                          )}
                        </td>
                        <td className="col-lba">{entry.is_dir && entry.lba === 0 ? "—" : entry.lba}</td>
                        <td className="col-size">{entry.is_dir ? "—" : (entry.size_bytes > 0 ? entry.size_bytes.toLocaleString() : "—")}</td>
                        <td className="col-modified">{entry.modified}</td>
                        <td className="col-save">
                          <button className="btn-save" title={entry.is_dir ? "Save folder" : "Save file"} onClick={() => saveEntry(entry)}>⬇</button>
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

      {audioUrl && (
        <div className="audio-player">
          <span className="audio-player-label">🎵 {audioEntries.find((e) => e.track_number === playingTrack)?.name ?? "Track"}</span>
          <audio className="audio-player-el" src={audioUrl} controls autoPlay onEnded={() => { /* keep loaded */ }} />
          <button className="audio-player-close" title="Close player" onClick={closePlayer}>✕</button>
        </div>
      )}

      <div className="statusbar">
        <span className="statusbar-left">{statusText}</span>
        <a className="statusbar-brand" href="https://whatever-industries.blogspot.com/" target="_blank" rel="noreferrer">whatever industries</a>
        <span className="statusbar-right">
          <span className="statusbar-version">v1.0.0</span>
        </span>
      </div>
    </div>
  );
}

export default App;
