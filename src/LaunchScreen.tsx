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
  donationUrl: string;
  donationEligible: boolean;
  donationActivated: boolean;
  onVerifyDonation: (email: string) => Promise<{ activated: boolean; message: string }>;
  availableUpdateVersion: string | null;
  installingUpdate: boolean;
  updatePhase?: "downloading" | "installing";
  onInstallUpdate: () => Promise<void>;
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

function UpdateIcon() {
  return <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 3v10" />
    <path d="m7.8 9.2 4.2 4.2 4.2-4.2" />
    <path d="M5 15.5v3A1.5 1.5 0 0 0 6.5 20h11a1.5 1.5 0 0 0 1.5-1.5v-3" />
  </svg>;
}

export function LaunchScreen({ onBrowseImage, onBrowseDrive, onDump, driveBusy, settings, tools, logo, donationUrl, donationEligible, donationActivated, onVerifyDonation, availableUpdateVersion, installingUpdate, updatePhase = "downloading", onInstallUpdate }: Props) {
  const [scanning, setScanning] = useState(false);
  const [drives, setDrives] = useState<DumpDrive[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [verifyOpen, setVerifyOpen] = useState(false);
  const [donationEmail, setDonationEmail] = useState("");
  const [verifyMessage, setVerifyMessage] = useState<string | null>(null);
  const [verifying, setVerifying] = useState(false);
  const verifyGeneration = useRef(0);
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

  async function verifyDonation() {
    setVerifyMessage(null);
    setVerifying(true);
    const generation = ++verifyGeneration.current;
    try {
      const result = await onVerifyDonation(donationEmail);
      if (!active.current || generation !== verifyGeneration.current) return;
      setVerifyMessage(result.message);
      if (result.activated) setVerifyOpen(false);
    } catch (e) {
      if (active.current && generation === verifyGeneration.current) setVerifyMessage(String(e));
    } finally {
      if (active.current && generation === verifyGeneration.current) setVerifying(false);
    }
  }

  function closeVerify() {
    verifyGeneration.current++;
    void invoke("cancel_donation_activation");
    setVerifying(false);
    setVerifyMessage(null);
    setVerifyOpen(false);
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
      {availableUpdateVersion ? <button className="launch-update" type="button" onClick={() => void onInstallUpdate()} disabled={installingUpdate}
        title={tr("Update available")} aria-label={tr("Update available")}>
        <UpdateIcon /><span>{installingUpdate ? tr(updatePhase === "downloading" ? "Downloading…" : "Installing…") : tr("Update")}</span><small>v{availableUpdateVersion}</small>
      </button> : donationActivated ? <div className="launch-icon launch-support launch-icon--static" aria-label="Disc Xplorer">
        <img src={logo} alt="Disc Xplorer" draggable={false} />
      </div> : donationEligible ? <div className="launch-donation" role="group" aria-label={tr("Support development")}>
        <button className="launch-donation-main" title={tr("Unlock Auto-Updates")} aria-label={tr("Unlock Auto-Updates")}
          onClick={() => void openUrl(donationUrl)}><span className="launch-donation-lead">{tr("Donate to")}</span><span>{tr("Unlock Auto-Updates")}</span></button>
        <button className="launch-donation-verify" type="button" onClick={() => { setVerifyMessage(null); setVerifyOpen(true); }}>{tr("Verify Donation")}</button>
      </div> : <a className="launch-icon launch-support" href={donationUrl} title={tr("Support development")} aria-label={tr("Support development")}
        onClick={e => { e.preventDefault(); void openUrl(donationUrl); }}>
        <img src={logo} alt="Disc Xplorer" draggable={false} />
      </a>}
      {error && <div className="launch-error" role="alert">{localizeMessage(error)} <button onClick={() => void browseDrive()} disabled={scanning}>{tr("Refresh")}</button></div>}
    </div>
    {verifyOpen && <div className="donation-dialog-backdrop" role="presentation" onMouseDown={event => { if (event.target === event.currentTarget && !verifying) closeVerify(); }}>
      <form className="donation-dialog" aria-modal="true" aria-labelledby="donation-dialog-title" onSubmit={event => { event.preventDefault(); void verifyDonation(); }}>
        <h2 id="donation-dialog-title">{tr("Verify Donation")}</h2>
        <p>{tr("Enter the email used for the donation.")}</p>
        <label>
          <span>{tr("Email")}</span>
          <input type="email" autoComplete="email" autoFocus value={donationEmail} disabled={verifying}
            onChange={event => setDonationEmail(event.currentTarget.value)} required maxLength={254} />
        </label>
        {verifyMessage && <p className="donation-dialog-message" role="status">{localizeMessage(verifyMessage)}</p>}
        <div className="donation-dialog-actions">
          <button type="button" onClick={closeVerify} disabled={verifying}>{tr("Cancel")}</button>
          <button className="primary" type="submit" disabled={verifying}>{verifying ? tr("Verifying…") : tr("Verify Donation")}</button>
        </div>
      </form>
    </div>}
  </section>;
}
