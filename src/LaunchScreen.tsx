import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { DumpDrive } from "./dump";
import { localizeMessage, tr } from "./i18n";
import "./LaunchScreen.css";

interface Props {
  onBrowseImage: () => void;
  onBrowseDrive: (drive: DumpDrive) => Promise<void>;
  onDump: () => void;
  driveBusy: boolean;
  settings: ReactNode;
  tools: ReactNode;
  logo: string;
  supportUrl: string;
}

function ActionIcon({ kind }: { kind: "image" | "drive" | "dump" }) {
  return <svg className={`launch-action-icon${kind === "image" ? " launch-action-icon-image" : ""}`} viewBox={kind === "image" ? "0 0 24 28" : "0 0 24 24"} aria-hidden="true" focusable="false"
    fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
    {kind === "image" ? <>
      <path d="M14 3H5a1 1 0 0 0-1 1v20a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9l-6-6Z" />
      <path d="M14 3v6h6" />
      <circle className="launch-action-icon-detail" cx="12" cy="17" r="5.15" fill="currentColor" fillOpacity=".12" />
      <circle className="launch-action-icon-detail" cx="12" cy="17" r="1.4" strokeWidth="1.2" />
    </> : <>
      {kind === "drive" ? <>
        <circle cx="12" cy="8" r="6" fill="currentColor" fillOpacity=".12" />
        <circle className="launch-action-icon-detail" cx="12" cy="8" r="1.4" />
      </> : <path className="launch-action-icon-detail" strokeWidth="2" d="M12 2v12m-5-5 5 5 5-5" />}
      <path d="M5 13 3 17v3a1 1 0 0 0 1 1h16a1 1 0 0 0 1-1v-3l-2-4M3 17h18" />
      <path className="launch-action-icon-detail" d="M16 19h2" />
    </>}
  </svg>;
}

export function LaunchScreen({ onBrowseImage, onBrowseDrive, onDump, driveBusy, settings, tools, logo, supportUrl }: Props) {
  const [scanning, setScanning] = useState(false);
  const [drives, setDrives] = useState<DumpDrive[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const active = useRef(true);
  const picker = useRef<HTMLDivElement>(null);
  const browseButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; };
  }, []);
  useEffect(() => {
    if (!drives) return;
    picker.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const close = (event: MouseEvent) => {
      if (!picker.current?.contains(event.target as Node) && !browseButton.current?.contains(event.target as Node)) setDrives(null);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [drives]);

  async function browseDrive() {
    setError(null);
    setDrives(null);
    setScanning(true);
    try {
      const inventory = await invoke<DumpDrive[]>("list_optical_drives");
      if (!active.current) return;
      if (inventory.length === 1) await onBrowseDrive(inventory[0]);
      else if (inventory.length) setDrives(inventory);
      else setError("Insert a disc or connect a drive, then refresh.");
    } catch (e) {
      if (active.current) setError(String(e));
    } finally {
      if (active.current) setScanning(false);
    }
  }

  return <section className="launch-screen">
    <div className="launch-grid">
      <button className="launch-action" onClick={onBrowseImage}><ActionIcon kind="image" /><span>{tr("Browse Disc Image")}</span></button>
      {settings}
      <div className="launch-drive">
        <button ref={browseButton} className="launch-action" onClick={() => void browseDrive()} disabled={scanning || driveBusy}
          aria-expanded={!!drives} aria-controls={drives ? "launch-drives" : undefined}>
          <ActionIcon kind="drive" /><span>{scanning ? tr("Scanning drives…") : tr("Browse Disc in Drive")}</span>
        </button>
        {drives && <div id="launch-drives" className="launch-drives" ref={picker} role="group" aria-label={tr("Select a drive.")}
          onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); setDrives(null); browseButton.current?.focus(); } }}>
          {drives.map(drive => <button key={drive.raw_device_path} onClick={() => {
            setDrives(null);
            void onBrowseDrive(drive).catch(e => { if (active.current) setError(String(e)); });
          }}>
            <span>{drive.name}</span>
            <small>{drive.volume_name || (drive.has_disc ? drive.device_path : tr("Optical disc drive is empty"))}</small>
          </button>)}
        </div>}
      </div>
      {tools}
      <button className="launch-action" onClick={onDump}><ActionIcon kind="dump" /><span>{tr("Dump Disc in Drive")}</span></button>
      <a className="launch-icon launch-support" href={supportUrl} title={tr("Support development")} aria-label={tr("Support development")}
        onClick={e => { e.preventDefault(); void openUrl(supportUrl); }}>
        <img src={logo} alt="Disc Xplorer" draggable={false} />
      </a>
      {error && <div className="launch-error" role="alert">{localizeMessage(error)} <button onClick={() => void browseDrive()} disabled={scanning}>{tr("Refresh")}</button></div>}
    </div>
  </section>;
}
