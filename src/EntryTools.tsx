import { useEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { tr, localizeMessage } from "./i18n";

export interface EntryToolRequest {
  imagePath: string;
  filePath: string;
  filesystem: string | null;
  kind: "location" | "hashes";
  xaMode?: number;
  isXa?: boolean;
}
interface SectorRange {
  start_lba: number; end_lba: number; sector_count: number;
  start_byte_offset: number; end_byte_offset: number;
}
interface Location {
  is_directory: boolean; logical_block_size: number; declared_bytes: number;
  ranges: SectorRange[]; sector_count: number; viewer_lba: number | null; lba_basis?: string; directory_storage?: string;
}
interface Hashes { bytes: number; crc32: string; md5: string; sha1: string; sha256: string }

export function EntryTools({ request, onClose }: { request: EntryToolRequest; onClose: () => void }) {
  const [location, setLocation] = useState<Location | null>(null);
  const [hashes, setHashes] = useState<Hashes | null>(null);
  const [error, setError] = useState("");
  const [bytes, setBytes] = useState(0);
  const [running, setRunning] = useState(true);
  const [stopping, setStopping] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const job = useRef("");
  const hashesRequested = request.kind === "hashes";
  const title = hashesRequested ? tr("Calculate Hashes") : tr("Sector Location");
  const mode = ["File content", "Subheader", "Raw"][request.xaMode ?? 0];
  useEffect(() => {
    let active = true;
    const jobId = crypto.randomUUID();
    job.current = jobId;
    const progress = new Channel<number>();
    progress.onmessage = value => { if (active) setBytes(value); };
    const args = { imagePath: request.imagePath, filePath: request.filePath, filesystem: request.filesystem };
    let started = false;
    queueMicrotask(() => {
      if (!active) return;
      started = true;
      const task = hashesRequested
        ? invoke<Hashes>("calculate_file_hashes", { ...args, jobId, xaMode: request.xaMode ?? 0, progress })
          .then(value => { if (active) setHashes(value); })
        : invoke<Location>("sector_location", args).then(value => { if (active) setLocation(value); });
      task.catch(e => { if (active) setError(String(e).includes("Hash calculation cancelled.") ? tr("Hash calculation cancelled.") : localizeMessage(String(e))); })
        .finally(() => { if (active) setRunning(false); });
    });
    return () => {
      active = false;
      if (started && hashesRequested) void invoke("cancel_file_hashes", { jobId }).catch(() => {});
    };
  }, [request, hashesRequested]);
  const close = () => { if (!running || !hashesRequested) onClose(); };
  async function cancel() {
    setStopping(true);
    try { await invoke("cancel_file_hashes", { jobId: job.current }); }
    catch (e) { setError(localizeMessage(String(e))); setStopping(false); }
  }
  async function copy() {
    if (!hashes) return;
    try {
      await navigator.clipboard.writeText(`${request.filePath}\n${request.isXa ? `${tr("Format")}: ${tr(mode)}\n` : ""}CRC32: ${hashes.crc32}\nMD5: ${hashes.md5}\nSHA-1: ${hashes.sha1}\nSHA-256: ${hashes.sha256}`);
      setCopied("all");
    } catch (e) { setError(localizeMessage(String(e))); }
  }
  async function copyValue(key: "crc32" | "md5" | "sha1" | "sha256") {
    if (!hashes) return;
    try { await navigator.clipboard.writeText(hashes[key]); setCopied(key); }
    catch (e) { setError(localizeMessage(String(e))); }
  }
  return <div className="modal-overlay" onClick={close}>
    <div className="modal conv-modal disc-info-modal entry-tools-modal" role="dialog" aria-modal="true" aria-labelledby="entry-tool-title"
      onClick={e => e.stopPropagation()} onKeyDown={e => {
        if (e.key === "Escape") { e.stopPropagation(); close(); }
        if (e.key === "Tab") {
          const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
          const next = e.shiftKey ? buttons[buttons.length - 1] : buttons[0];
          const edge = e.shiftKey ? buttons[0] : buttons[buttons.length - 1];
          if (next && (document.activeElement === edge || !buttons.includes(document.activeElement as HTMLButtonElement))) {
            e.preventDefault(); next.focus();
          }
        }
      }}>
      <div className="modal-header"><span className="modal-title" id="entry-tool-title">{title}</span></div>
      <div className="modal-body" aria-live="polite">
        <div className="disc-info-name">{request.filePath}</div>
        {hashesRequested && request.isXa && <p>{tr("Format")}: {tr(mode)}</p>}
        {running && <p><span className="extract-spinner" /> {hashesRequested ? tr("Calculating hashes… {0} bytes read", bytes.toLocaleString()) : tr("Loading…")}</p>}
        {error && <p className="disc-info-error">{error}</p>}
        {hashes && <dl className="disc-info-fields entry-hash-values">
          <dt>{tr("Bytes hashed")}</dt><dd>{hashes.bytes.toLocaleString()}{hashes.bytes === 0 && <> · {tr("Empty file")}</>}</dd>
          {(["crc32", "md5", "sha1", "sha256"] as const).map((key, i) => <div key={key}>
            <dt>{["CRC32", "MD5", "SHA-1", "SHA-256"][i]}</dt><dd>
              <button type="button" className={`entry-hash-copy${copied === key ? " is-copied" : ""}`}
                title={tr(copied === key ? "Copied" : "Click to copy")}
                aria-label={`${["CRC32", "MD5", "SHA-1", "SHA-256"][i]} · ${tr(copied === key ? "Copied" : "Click to copy")}`}
                onClick={() => void copyValue(key)}><code>{hashes[key]}</code></button>
            </dd>
          </div>)}
        </dl>}
        {location && <>
          <p>{tr("Offsets are zero-based within each {0}-byte logical block.", location.logical_block_size)}{location.lba_basis === "track" && <> {tr("LBAs are relative to the data track.")}</>}</p>
          {location.is_directory && <p>{tr(location.directory_storage === "catalog" ? "These bytes hold the folder’s HFS catalog record." : "These sectors hold directory records; child files and folders are stored separately.")}</p>}
          {location.ranges.length === 0 ? <p>{tr("This entry occupies no data sectors.")}</p> : <>
            <dl className="disc-info-fields">
              <dt>{tr("Sector Count")}</dt><dd>{location.sector_count.toLocaleString()}</dd>
              <dt>{tr("Declared size (bytes)")}</dt><dd>{location.declared_bytes.toLocaleString()}</dd>
            </dl>
            <table className="disc-info-tracks entry-sector-ranges"><thead><tr>
              <th>{tr("Start LBA")}</th><th>{tr("Start byte offset")}</th><th>{tr("End LBA (inclusive)")}</th><th>{tr("End byte offset")}</th>
            </tr></thead><tbody>{location.ranges.map((range, i) => <tr key={i}>
              <td>{range.start_lba.toLocaleString()}</td><td>{range.start_byte_offset}</td><td>{range.end_lba.toLocaleString()}</td><td>{range.end_byte_offset}</td>
            </tr>)}</tbody></table>
          </>}
        </>}
      </div>
      <div className="modal-footer">
        {running && hashesRequested ? <button autoFocus className="btn-open" disabled={stopping} onClick={() => void cancel()}>{tr("Cancel")}</button> : (!running || !hashesRequested) && <>
          {hashes && <button className="btn-open" onClick={() => void copy()}>{tr(copied === "all" ? "Copied" : "Copy hashes")}</button>}
          {location?.viewer_lba != null && <button className="btn-open" onClick={() => {
            invoke("open_sector_view_window", { imagePath: request.imagePath, lba: location.viewer_lba, compareImagePath: null })
              .catch(e => setError(localizeMessage(String(e))));
          }}>{tr("View in Sector View")}</button>}
          <button autoFocus className="btn-open" onClick={close}>{tr("Close")}</button>
        </>}
      </div>
    </div>
  </div>;
}
