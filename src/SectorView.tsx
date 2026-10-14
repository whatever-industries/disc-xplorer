import { tr, localizeMessage } from "./i18n";
import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { sectorDropTarget, sectorImageExtensions } from "./sectorDrop";
import { open, save } from "@tauri-apps/plugin-dialog";

interface SectorData {
  bytes: number[];
  sector_size: number;
  user_data_offset: number;
  total_sectors: number;
  lba: number;
}

// Per-sector layout derived from the CD sync/header bytes.
interface Layout {
  hasCd: boolean;
  mode: number;
  form: number; // 0 = n/a or unknown, 1 = Form 1, 2 = Form 2 (Mode 2 only)
  syncEnd: number;
  headerEnd: number;
  subhdrEnd: number;
  dataStart: number;
  dataEnd: number;
  eccStart: number;
}

// Returns the ISOBuster-style mode label with user byte count, e.g. "Mode 2 / Form 1 (2048)".
function modeLabel(layout: Layout): string {
  const userBytes = layout.dataEnd - layout.dataStart;
  if (!layout.hasCd) return `${tr("Audio")} (${userBytes})`;
  if (layout.mode === 1) return `Mode 1 (${userBytes})`;
  if (layout.mode === 2) {
    if (layout.form === 1) return `Mode 2 / Form 1 (${userBytes})`;
    if (layout.form === 2) return `Mode 2 / Form 2 (${userBytes})`;
    return `Mode 2 (${userBytes})`;
  }
  return `Mode ${layout.mode} (${userBytes})`;
}

const CD_SYNC = [0x00,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0xFF,0x00];

function getLayout(bytes: number[], sectorSize: number): Layout {
  const noLayout: Layout = {
    hasCd: false, mode: 0, form: 0,
    syncEnd: 0, headerEnd: 0, subhdrEnd: 0,
    dataStart: 0, dataEnd: sectorSize, eccStart: sectorSize,
  };
  if (sectorSize !== 2352 || bytes.length < 16) return noLayout;
  if (!CD_SYNC.every((v, i) => bytes[i] === v)) return noLayout;

  const mode = bytes[15];
  if (mode === 1) {
    // Mode 1: sync(12) + header(4) + data(2048) + EDC(4) + zero(8) + ECC(276) = 2352
    return { hasCd: true, mode, form: 0, syncEnd: 12, headerEnd: 16, subhdrEnd: 16, dataStart: 16, dataEnd: 2064, eccStart: 2064 };
  }
  if (mode === 2) {
    // Mode 2: sync(12) + header(4) + sub-header(8) + data + ECC/EDC
    // Form 2 flag: sub-header byte 2 (absolute byte 18), bit 5.
    const isForm2 = bytes.length >= 19 && (bytes[18] & 0x20) !== 0;
    const form = isForm2 ? 2 : 1;
    // Form 1: data(2048) + EDC(4) + ECC(276) ending at 2352; Form 2: data(2324) + EDC/spare(4) at 2348
    const dataEnd = isForm2 ? 2348 : 2072;
    return { hasCd: true, mode, form, syncEnd: 12, headerEnd: 16, subhdrEnd: 24, dataStart: 24, dataEnd, eccStart: dataEnd };
  }
  // Unknown mode but has CD sync
  return { hasCd: true, mode, form: 0, syncEnd: 12, headerEnd: 16, subhdrEnd: 16, dataStart: 16, dataEnd: 2352, eccStart: 2352 };
}

function bcd(b: number): number { return (b >> 4) * 10 + (b & 0x0F); }

function parseDisc(bytes: number[], sectorSize: number) {
  if (sectorSize !== 2352 || bytes.length < 16) return null;
  if (!CD_SYNC.every((v, i) => bytes[i] === v)) return null;
  const m = bcd(bytes[12]), s = bcd(bytes[13]), f = bcd(bytes[14]);
  const mode = bytes[15];
  const discLba = (m * 60 + s) * 75 + f - 150;
  const msf = `${String(m).padStart(2,'0')}:${String(s).padStart(2,'0')}:${String(f).padStart(2,'0')}`;
  return { mode, msf, discLba };
}

function byteClass(idx: number, layout: Layout): string {
  if (!layout.hasCd) return '';
  if (idx < layout.syncEnd) return 'hb-sync';
  if (idx < layout.headerEnd) return 'hb-hdr';
  if (idx < layout.subhdrEnd) return 'hb-sub';
  if (idx >= layout.dataStart && idx < layout.dataEnd) return '';
  return 'hb-ecc';
}

function HexRow({ offset, bytes, layout, diffMask }: {
  offset: number;
  bytes: number[];
  layout: Layout;
  diffMask?: boolean[];
}) {
  const content: (string | React.JSX.Element)[] = [];

  content.push(<span key="addr" className="hex-addr">{offset.toString(16).padStart(4, '0').toUpperCase()}</span>);
  content.push(' : ');

  for (let j = 0; j < 16; j++) {
    if (j === 8) content.push('  '); else if (j > 0) content.push(' ');
    const cls = byteClass(offset + j, layout);
    const alt = j % 2 === 1 ? 'hb-alt' : '';
    const diff = diffMask?.[j] ? ' hb-diff' : '';
    const hex = bytes[j].toString(16).padStart(2, '0').toUpperCase();
    content.push(<span key={`h${j}`} className={`hb ${alt} ${cls}${diff}`}>{hex}</span>);
  }

  content.push('   ');

  for (let j = 0; j < 16; j++) {
    const b = bytes[j];
    const cls = byteClass(offset + j, layout);
    const alt = j % 2 === 1 ? 'hb-alt' : '';
    const diff = diffMask?.[j] ? ' hb-diff' : '';
    const ch = b >= 0x20 && b < 0x7F ? String.fromCharCode(b) : '.';
    content.push(<span key={`a${j}`} className={`ha ${alt} ${cls}${diff}`}>{ch}</span>);
  }

  return <div className="hex-row">{content}</div>;
}

function HexDump({ data, rawMode, compareBytes }: {
  data: SectorData;
  rawMode: boolean;
  compareBytes?: number[];
}) {
  const layout = getLayout(data.bytes, data.sector_size);
  const slice = (!rawMode && layout.hasCd)
    ? data.bytes.slice(layout.dataStart, layout.dataEnd)
    : data.bytes;
  const compareSlice = compareBytes
    ? ((!rawMode && layout.hasCd) ? compareBytes.slice(layout.dataStart, layout.dataEnd) : compareBytes)
    : undefined;

  const rows: React.JSX.Element[] = [];
  for (let i = 0; i < slice.length; i += 16) {
    const rowBytes = slice.slice(i, i + 16);
    const rowCmp = compareSlice?.slice(i, i + 16);
    const diffMask = rowCmp ? rowBytes.map((b, j) => rowCmp[j] !== undefined && b !== rowCmp[j]) : undefined;
    rows.push(
      <HexRow
        key={i}
        offset={i}
        bytes={rowBytes}
        layout={rawMode ? layout : { ...layout, hasCd: false }}
        diffMask={diffMask}
      />
    );
  }
  return <div className="sv-hex-dump">{rows}</div>;
}

// Invisible alternatives reserve their natural translated widths. The visible
// label can change without moving adjacent controls or readouts.
function StableReadout({ children, reserve }: { children: React.ReactNode; reserve: React.ReactNode[] }) {
  return <span className="sv-stable-readout">
    {reserve.map((value, i) => <span key={i} aria-hidden="true" className="sv-width-reserve">{value}</span>)}
    <span>{children}</span>
  </span>;
}

export function SectorView({ imagePath: initialImagePath, onClose, initialLba, initialCompareImagePath }: {
  imagePath: string;
  onClose: () => void;
  initialLba?: number;
  initialCompareImagePath?: string | null;
}) {
  const [imagePath, setImagePath] = useState(initialImagePath);
  const [data, setData] = useState<SectorData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [inputVal, setInputVal] = useState("0");
  const [showExport, setShowExport] = useState(false);
  const [exportStart, setExportStart] = useState("0");
  const [exportEnd, setExportEnd] = useState("0");
  const [exporting, setExporting] = useState(false);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  // discOffset: constant difference between disc-absolute LBA (from sync header MSF)
  // and track-relative LBA. Null until we've seen a sector with a valid CD sync header.
  const [discOffset, setDiscOffset] = useState<number | null>(null);
  const [discMode, setDiscMode] = useState(false);
  const [rawMode, setRawMode] = useState(true);

  // Compare state

  const [compareImagePath, setCompareImagePath] = useState<string | null>(initialCompareImagePath ?? null);
  const [compareData, setCompareData] = useState<SectorData | null>(null);
  const [scanning, setScanning] = useState(false);
  const [noPrevDiff, setNoPrevDiff] = useState(false);
  const [noNextDiff, setNoNextDiff] = useState(false);
  const [allIdentical, setAllIdentical] = useState(false);

  const showCompare = !!compareImagePath;
  const [compareError, setCompareError] = useState<string | null>(null);
  const [dropSide, setDropSide] = useState<"a" | "b" | null>(null);
  const panelA = useRef<HTMLDivElement>(null);
  const panelB = useRef<HTMLDivElement>(null);
  const loadRequest = useRef(0);
  const pairRevision = useRef(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const toDisplay = (trackLba: number) =>
    discMode && discOffset !== null ? trackLba + discOffset : trackLba;

  const toTrack = (displayLba: number) =>
    discMode && discOffset !== null ? displayLba - discOffset : displayLba;

  const load = useCallback(async (trackLba: number) => {
    const request = ++loadRequest.current;
    setError(null);
    try {
      const result = await invoke<SectorData>("read_sector", { imagePath, lba: trackLba });
      if (request !== loadRequest.current) return;
      const disc = parseDisc(result.bytes, result.sector_size);
      if (disc !== null) setDiscOffset(disc.discLba - result.lba);
      setData(result);
    } catch (e) {
      if (request === loadRequest.current) setError(String(e));
    }
  }, [imagePath]);

  // Ignore late reads after navigation or either image has been replaced.
  useEffect(() => {
    let cancelled = false;
    setCompareData(null);
    setCompareError(null);
    if (!compareImagePath || data === null) return;
    invoke<SectorData>("read_sector", { imagePath: compareImagePath, lba: data.lba })
      .then(result => { if (!cancelled) setCompareData(result); })
      .catch(e => { if (!cancelled) setCompareError(String(e)); });
    return () => { cancelled = true; };
  }, [compareImagePath, data]);

  // A detached window opened on a single image is only wide enough for one hex
  // panel; turning compare on inside it needs room for the second.
  useEffect(() => {
    if (compareImagePath) void invoke("widen_sector_view_for_compare").catch(() => {});
  }, [compareImagePath]);

  // Sync input field whenever data or mode changes.
  useEffect(() => {
    if (data) setInputVal(String(toDisplay(data.lba)));
  }, [data, discMode, discOffset]);

  useEffect(() => {
    setData(null);
    setDiscOffset(null);
    void load(imagePath === initialImagePath ? initialLba ?? 0 : 0);
    return () => { ++loadRequest.current; };
  }, [imagePath]);

  const lba = data?.lba ?? 0;
  const total = data?.total_sectors ?? 0;

  function go(targetTrackLba: number) {
    if (!total) return;
    load(Math.max(0, Math.min(targetTrackLba, total - 1)));
  }

  function commit() {
    const n = parseInt(inputVal, 10);
    if (!isNaN(n)) go(toTrack(n));
  }

  function toggleMode() {
    if (discOffset === null) return;
    setDiscMode(m => !m);
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.target instanceof HTMLInputElement) return;
      if (e.key === 'ArrowLeft'  || e.key === 'ArrowUp')   { e.preventDefault(); go(lba - 1); }
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown')  { e.preventDefault(); go(lba + 1); }
      if (e.key === 'PageUp')   { e.preventDefault(); go(lba - 100); }
      if (e.key === 'PageDown') { e.preventDefault(); go(lba + 100); }
      if (e.key === 'Home')     { e.preventDefault(); go(0); }
      if (e.key === 'End')      { e.preventDefault(); go(total - 1); }
      if (e.key === 'Escape')   onClose();
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [lba, total]);

  function openExport() {
    setExportStart(String(toDisplay(lba)));
    setExportEnd(String(toDisplay(lba)));
    setExportStatus(null);
    setShowExport(true);
  }

  async function runExport() {
    const start = parseInt(exportStart, 10);
    const end = parseInt(exportEnd, 10);
    if (isNaN(start) || isNaN(end) || end < start) {
      setExportStatus("Invalid range");
      return;
    }
    const destPath = await save({ defaultPath: "sectors.bin" });
    if (!destPath) return;
    setExporting(true);
    setExportStatus(null);
    try {
      const written = await invoke<number>("export_sector_range", {
        imagePath,
        lbaStart: toTrack(start),
        lbaEnd: toTrack(end),
        destPath,
      });
      setExportStatus(`Exported ${written} sector${written === 1 ? "" : "s"}`);
    } catch (e) {
      setExportStatus(String(e));
    } finally {
      setExporting(false);
    }
  }

  async function jumpToDiff(forward: boolean, fromLba = lba, inclusive = false) {
    if (!compareImagePath || scanning) return;
    const revision = pairRevision.current;
    setScanning(true);
    try {
      const found = await invoke<number | null>("find_diff_sector", {
        imagePathA: imagePath,
        imagePathB: compareImagePath,
        fromLba,
        forward,
        inclusive,
      });
      if (revision !== pairRevision.current) return;
      if (found !== null) {
        setNoPrevDiff(false);
        setNoNextDiff(false);
        setAllIdentical(false);
        go(found);
      } else {
        if (forward) {
          setNoNextDiff(true);
          if (noPrevDiff || lba === 0) setAllIdentical(true);
        } else {
          setNoPrevDiff(true);
          if (noNextDiff || lba >= total - 1) setAllIdentical(true);
        }
      }
    } catch (e) {
      if (revision === pairRevision.current) setCompareError(String(e));
    } finally {
      if (revision === pairRevision.current) setScanning(false);
    }
  }

  function resetComparison() {
    ++pairRevision.current;
    setScanning(false);
    setCompareData(null);
    setCompareError(null);
    setNoPrevDiff(false);
    setNoNextDiff(false);
    setAllIdentical(false);
  }

  function replaceImage(side: "a" | "b", path: string) {
    resetComparison();
    if (side === "a") {
      ++loadRequest.current;
      setDiscOffset(null);
      setDiscMode(false);
      setData(null);
      setError(null);
      setImagePath(path);
      // Re-dropping the same image should refresh it as well.
      if (path === imagePath) void load(0);
    } else {
      setCompareImagePath(path);
      if (path === compareImagePath && data) {
        // Changing the dependency refreshes the second read without changing LBA.
        setData({ ...data });
      }
    }
  }

  async function pickCompareImage() {
    const path = await open({
      title: tr("Open disc image to compare"),
      filters: [{ name: tr("Disc Images"), extensions: [...sectorImageExtensions] }],
    });
    if (path && typeof path === "string") replaceImage("b", path);
  }

  function clearCompare() {
    resetComparison();
    setCompareImagePath(null);
  }

  // Native drops carry physical coordinates; panels use CSS pixels. Read the
  // current panel bounds for every event so scrolling and resizing stay correct.
  const dropState = useRef({ enabled: showCompare, replaceImage });
  dropState.current = { enabled: showCompare, replaceImage };
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (disposed) return;
      if (payload.type === "leave" || !dropState.current.enabled) {
        setDropSide(null);
        return;
      }
      const side = sectorDropTarget(payload.position, window.devicePixelRatio,
        panelA.current?.getBoundingClientRect(), panelB.current?.getBoundingClientRect());
      setDropSide(payload.type === "drop" ? null : side);
      if (payload.type === "drop" && side) {
        const path = payload.paths.find(path => sectorImageExtensions.some(ext => path.toLowerCase().endsWith(`.${ext}`)));
        if (path) dropState.current.replaceImage(side, path);
      }
    }).then(off => { if (disposed) off(); else unlisten = off; }).catch(e => {
      if (!disposed) setError(String(e));
    });
    return () => { disposed = true; unlisten?.(); ++pairRevision.current; };
  }, []);

  const disc = data ? parseDisc(data.bytes, data.sector_size) : null;
  const layout = data ? getLayout(data.bytes, data.sector_size) : null;
  const isLastSector = total > 0 && lba >= total - 1;

  const minInput = discMode && discOffset !== null ? discOffset : 0;
  const maxInput = discMode && discOffset !== null ? total - 1 + discOffset : total > 0 ? total - 1 : 0;

  const compareFileName = compareImagePath?.split(/[/\\]/).pop() ?? null;

  return (
    <div className="sv-standalone" onClick={e => e.stopPropagation()}>

        <div className="sv-nav">
          <button
            className={`sv-mode-toggle ${discMode ? 'sv-mode-toggle--active' : ''}`}
            onClick={toggleMode}
            disabled={discOffset === null}
            title={discOffset === null ? tr("No CD sync header — disc LBA unavailable") : discMode ? tr("Switch to track-relative LBA") : tr("Switch to disc-absolute LBA")}
          >
            <StableReadout reserve={[tr("Disc LBA"), tr("Track LBA")]}>{discMode ? tr("Disc LBA") : tr("Track LBA")}</StableReadout>
          </button>
          <input
            ref={inputRef}
            className="sv-input"
            type="number"
            min={minInput}
            max={maxInput}
            value={inputVal}
            onChange={e => setInputVal(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') commit(); }}
            onBlur={commit}
          />
          {total > 0 && (
            <span className="sv-total"><StableReadout reserve={[
              tr("of {0}", (total - 1).toLocaleString()),
              tr("of {0}", (total - 1 + (discOffset ?? 0)).toLocaleString()),
            ]}>{tr("of {0}", (discMode && discOffset !== null ? total - 1 + discOffset : total - 1).toLocaleString())}</StableReadout></span>
          )}
          <div className="sv-nav-btns">
            <button className="sv-btn" onClick={() => go(0)}           disabled={lba === 0}     title={tr("First (Home)")}>⏮</button>
            <button className="sv-btn" onClick={() => go(lba - 1)}     disabled={lba === 0}     title={tr("Previous (←)")}>◀</button>
            <button className="sv-btn" onClick={() => go(lba + 1)}     disabled={isLastSector}  title={tr("Next (→)")}>▶</button>
            <button className="sv-btn" onClick={() => go(total - 1)}   disabled={isLastSector}  title={tr("Last (End)")}>⏭</button>
          </div>
          {layout?.hasCd && (
            <button
              className={`sv-mode-toggle ${!rawMode ? 'sv-mode-toggle--active' : ''}`}
              onClick={() => setRawMode(m => !m)}
              title={rawMode
                ? tr("Show user data only ({0}B)", layout.dataEnd - layout.dataStart)
                : tr("Show full raw sector ({0}B)", data!.sector_size)}
            >
              {rawMode ? tr("{0}B raw", data!.sector_size) : tr("{0}B user", layout.dataEnd - layout.dataStart)}
            </button>
          )}
          <button
            className={`sv-mode-toggle ${showExport ? 'sv-mode-toggle--active' : ''}`}
            onClick={() => showExport ? setShowExport(false) : openExport()}
            disabled={!data}
            title={tr("Export sector range as raw binary")}
          >{tr("Export range")}</button>
          <button
            className={`sv-mode-toggle ${showCompare ? 'sv-mode-toggle--active' : ''}`}
            onClick={() => void pickCompareImage()}
            disabled={!data}
            title={tr("Compare with a second disc image")}
          >{tr("Compare")}</button>
        </div>

        {showExport && (
          <div className="sv-export-row">
            <span className="sv-export-label">LBA</span>
            <input
              className="sv-input"
              type="number"
              min={minInput}
              max={maxInput}
              value={exportStart}
              onChange={e => setExportStart(e.target.value)}
            />
            <span className="sv-export-label">{tr("to")}</span>
            <input
              className="sv-input"
              type="number"
              min={minInput}
              max={maxInput}
              value={exportEnd}
              onChange={e => setExportEnd(e.target.value)}
            />
            <button className="sv-btn sv-export-btn" onClick={runExport} disabled={exporting}>
              {exporting ? tr("Exporting…") : tr("Export .bin")}
            </button>
            {exportStatus && <span className="sv-export-status">{localizeMessage(exportStatus)}</span>}
          </div>
        )}

        {showCompare && (
          <div className="sv-compare-row">
            <button className="sv-btn sv-export-btn" onClick={pickCompareImage}>
              {compareImagePath ? tr("Change…") : tr("Open image…")}
            </button>
            {compareFileName
              ? <span className="sv-compare-path" title={compareImagePath ?? ""}>{compareFileName}</span>
              : <span className="sv-export-label">{tr("No image selected")}</span>
            }
            {compareImagePath && (
              <button className="sv-btn sv-export-btn" onClick={clearCompare}>{tr("Clear")}</button>
            )}
            {compareData && (
              <>
                <span className="sv-compare-diff-count">
                  {allIdentical
                    ? <span style={{ color: "#4ec94e" }}>{tr("Images identical")}</span>
                    : (() => {
                        const a = data!.bytes;
                        const b = compareData.bytes;
                        const len = Math.max(a.length, b.length);
                        let diff = 0;
                        for (let i = 0; i < len; i++) if (a[i] !== b[i]) diff++;
                        return diff === 0
                          ? <span style={{ color: "#4ec94e" }}>{tr("Sector identical")}</span>
                          : <span style={{ color: "#e5a550" }}>{tr("Different bytes: {0}", diff.toLocaleString())}</span>;
                      })()
                  }
                </span>
                <button className="sv-btn sv-export-btn" onClick={() => jumpToDiff(false)} disabled={scanning || lba === 0 || noPrevDiff} title={noPrevDiff ? tr("No previous differences found") : tr("Jump to previous differing sector")}>{tr("◀ Prev diff")}</button>
                <button className="sv-btn sv-export-btn" onClick={() => jumpToDiff(true)} disabled={scanning || lba >= total - 1 || noNextDiff} title={noNextDiff ? tr("No further differences found") : tr("Jump to next differing sector")}>
                  {scanning ? tr("Scanning…") : tr("Next diff ▶")}
                </button>
              </>
            )}
          </div>
        )}

        {data && (
          <div className="sv-info">
            <div className="sv-info-left">
              {disc ? (
                <>
                  <span className="sv-badge sv-badge-cd">CD</span>
                  <span>{layout ? modeLabel(layout) : tr("Mode {0}", disc.mode)}</span>
                  <span className="sv-sep">·</span>
                  <StableReadout reserve={[
                    <>{tr("Track LBA")} <strong>{Math.max(total - 1, 0).toLocaleString()}</strong></>,
                    <>{tr("Disc LBA")} <strong>{(Math.max(total - 1, 0) + (discOffset ?? 0)).toLocaleString()}</strong></>,
                  ]}>
                    <span title={discMode ? tr("Track-relative LBA {0}", data.lba) : tr("Disc-absolute LBA {0}", disc.discLba)}>
                      {discMode ? tr("Disc LBA") : tr("Track LBA")} <strong>{(discMode ? disc.discLba : data.lba).toLocaleString()}</strong>
                    </span>
                  </StableReadout>
                  <span className="sv-sep">·</span>
                  <span>MSF {disc.msf}</span>
                </>
              ) : (
                <>
                  <span className="sv-badge sv-badge-iso">ISO</span>
                  <span>{tr("{0}B / sector", data.sector_size)}</span>
                </>
              )}
            </div>
            <div className="sv-legend">
              {layout?.hasCd && <><span className="sv-leg"><span className="sv-swatch hb-sync"/>Sync</span><span className="sv-leg"><span className="sv-swatch hb-hdr"/>{tr("Hdr")}</span></>}
              {layout?.hasCd && layout.subhdrEnd > layout.headerEnd && <span className="sv-leg"><span className="sv-swatch hb-sub"/>{tr("Sub-Hdr")}</span>}
              {layout?.hasCd && <span className="sv-leg"><span className="sv-swatch hb-ecc"/>ECC</span>}
              {compareData && <span className="sv-leg"><span className="sv-swatch hb-diff"/>{tr("Diff")}</span>}
            </div>
          </div>
        )}

        {error && <div className="sv-error">{localizeMessage(error)}</div>}

        <div className={`sv-hex-area ${showCompare ? "sv-hex-area--compare" : ""}`}>
          <div ref={panelA} className={`${showCompare ? "sv-hex-panel" : ""}${dropSide === "a" ? " sv-drop-active" : ""}`}>
            {showCompare && <div className="sv-hex-panel-label" title={imagePath}>{tr("Image A — {0}", imagePath.split(/[/\\]/).pop())}</div>}
            {data && <HexDump data={data} rawMode={rawMode} compareBytes={compareData?.bytes} />}
          </div>
          {showCompare && (
            <div ref={panelB} className={`sv-hex-panel${dropSide === "b" ? " sv-drop-active" : ""}`}>
              <div className="sv-hex-panel-label" title={compareImagePath ?? ""}>{tr("Image B — {0}", compareFileName)}</div>
              {compareError && <div className="sv-error">{localizeMessage(compareError)}</div>}
              {compareData && <HexDump data={compareData} rawMode={rawMode} compareBytes={data?.bytes} />}
            </div>
          )}
        </div>

      </div>
  );
}
