import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { downloadDir } from "@tauri-apps/api/path";
import { dumpActive, dumpName, dumpOutput, elapsedDump, nameError } from "./dump";
import type { DumpController, DumpDrive, DumpJob } from "./dump";
import "./DumpView.css";
import type { DumpOptions } from "./DumpSettings";

interface Props {
  visible: boolean;
  actionsTarget: HTMLDivElement | null;
  ejectIcon: ReactNode;
  refreshIcon: ReactNode;
  folderIcon: ReactNode;
  controller: DumpController;
  preferredDrive: DumpDrive | null;
  onDriveChange: (drive: DumpDrive | null) => void;
  options: DumpOptions;
  source: string;
  externalPath: string;
  beforeStart: () => void;
  browseImage: (path: string) => Promise<void>;
  browseBusy: boolean;
}

export function DumpView({ visible, actionsTarget, ejectIcon, refreshIcon, folderIcon, controller: dump, preferredDrive, onDriveChange, options, source, externalPath, beforeStart, browseImage, browseBusy }: Props) {
  const [drives, setDrives] = useState<DumpDrive[]>([]);
  const [selected, setSelected] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [driveError, setDriveError] = useState<string | null>(null);
  const [output, setOutput] = useState(() => localStorage.getItem("dumpOutputFolder") || "");
  const [speed, setSpeed] = useState(() => localStorage.getItem("dumpSpeed") || "");
  const [name, setName] = useState(() => dumpName(null));
  const [setup, setSetup] = useState(true);
  const [now, setNow] = useState(Date.now);
  const [actionBusy, setActionBusy] = useState(false);
  const [recovery, setRecovery] = useState<string | null>(null);
  const [checkingRecovery, setCheckingRecovery] = useState(false);
  const [recoveryRevision, setRecoveryRevision] = useState(0);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
  const [commandDraft, setCommandDraft] = useState<{ key: string; value: string } | null>(null);
  const [commandPreview, setCommandPreview] = useState<{ key: string; draft: string | null; command: string; generated: string; error: string | null } | null>(null);
  const refreshId = useRef(0);
  const logRef = useRef<HTMLPreElement>(null);
  const followLog = useRef(true);
  const { job } = dump;
  const showJob = !!job && (!setup || dumpActive(job));
  const displayedJob = showJob ? job : null;
  const drivePath = displayedJob?.drive || selected;
  const drive = drives.find(d => d.raw_device_path === drivePath);
  const invalidName = nameError(name);
  const locked = showJob || dump.running || actionBusy;
  const destination = displayedJob?.output_path || (output ? dumpOutput(output, name) : "Choose an output folder");
  const canRecover = !showJob && recovery === destination;

  const commandKey = JSON.stringify({ request: { drive: drivePath, drive_name: drive?.name || displayedJob?.drive_name || "",
    output_parent: output, name: displayedJob?.name ?? name, speed: speed ? Number(speed) : null,
    source, external_path: externalPath || null, options }, refine: !!displayedJob?.can_refine || canRecover, jobId: displayedJob?.id ?? null });
  const manualCommand = options.advanced_command && commandDraft?.key === commandKey ? commandDraft.value : null;
  const commandEditable = !dump.running && (!displayedJob || displayedJob.can_refine);
  const commandPending = options.advanced_command && commandEditable
    && (commandPreview?.key !== commandKey || commandPreview.draft !== manualCommand);
  const commandError = options.advanced_command && !commandPending && commandEditable ? commandPreview?.error : null;
  const commandText = !commandEditable ? displayedJob?.command || commandPreview?.command || ""
    : manualCommand ?? (commandPreview?.key === commandKey ? commandPreview.command : "");
  const cannotStart = !drive || !!invalidName || !output || !dump.ready || dump.running
    || commandPending || !!commandError || refreshing || checkingRecovery || browseBusy || actionBusy || (source === "external" && !externalPath);

  useEffect(() => {
    if (!options.advanced_command || !visible || !commandEditable) return;
    let disposed = false;
    const timer = window.setTimeout(() => {
      const args = JSON.parse(commandKey);
      args.request.manual_command = manualCommand;
      void invoke<{ command: string; generated: string }>("preview_redumper_command", args)
        .then(result => { if (!disposed) setCommandPreview({ key: commandKey, draft: manualCommand, ...result, error: null }); })
        .catch(e => { if (!disposed) setCommandPreview(previous => ({ key: commandKey, draft: manualCommand,
          command: previous?.key === commandKey ? previous.command : "", generated: previous?.key === commandKey ? previous.generated : "", error: String(e) })); });
    }, 150);
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [commandKey, manualCommand, options.advanced_command, visible, commandEditable]);

  useEffect(() => {
    setRecovery(null);
    setRecoveryError(null);
    setCheckingRecovery(false);
    if (!visible || showJob || dump.running || invalidName || !output) return;
    let disposed = false;
    setCheckingRecovery(true);
    const timer = window.setTimeout(() => {
      void invoke<boolean>("inspect_redumper_dump", { outputParent: output, name })
        .then(found => { if (!disposed) setRecovery(found ? dumpOutput(output, name) : null); })
        .catch(e => { if (!disposed) setRecoveryError(`Could not check existing dump: ${e}`); })
        .finally(() => { if (!disposed) setCheckingRecovery(false); });
    }, 200);
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [visible, showJob, dump.running, invalidName, output, name, recoveryRevision]);

  useEffect(() => { onDriveChange(drive ?? null); }, [drive, onDriveChange]);

  useEffect(() => {
    let disposed = false;
    if (!output) void downloadDir().then(path => { if (!disposed) setOutput(current => current || path); });
    return () => { disposed = true; };
  }, []);
  useEffect(() => { if (output) localStorage.setItem("dumpOutputFolder", output); }, [output]);
  useEffect(() => { localStorage.setItem("dumpSpeed", speed); }, [speed]);
  useEffect(() => { if (job) setSetup(false); }, [job?.id]);
  useEffect(() => {
    if (!dump.running) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [dump.running]);
  useEffect(() => {
    if (followLog.current && visible && logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [job?.revision, visible]);

  function resetMissingDump(snapshot: DumpJob | null) {
    if (snapshot && !dumpActive(snapshot)
      && (snapshot.output_exists === false || (!snapshot.image_path && !snapshot.can_refine))) {
      setSetup(true);
    }
  }

  async function refresh(prefer = selected, resetName = false) {
    const id = ++refreshId.current;
    setRefreshing(true);
    setDriveError(null);
    try {
      const [inventory, snapshot] = await Promise.allSettled([invoke<DumpDrive[]>("list_optical_drives"), dump.refresh()]);
      if (id !== refreshId.current) return;
      setRecoveryRevision(value => value + 1);
      if (snapshot.status === "fulfilled") resetMissingDump(snapshot.value);
      if (inventory.status === "rejected") throw inventory.reason;
      if (snapshot.status === "rejected") setDriveError(`Could not refresh dump status: ${snapshot.reason}`);
      const result = inventory.value;
      setDrives(result);
      const next = result.find(d => d.raw_device_path === prefer) || result.find(d => d.has_disc) || result[0];
      setSelected(next?.raw_device_path || "");
      if (resetName || next?.raw_device_path !== selected || next?.volume_name !== drive?.volume_name) {
        setName(dumpName(next?.volume_name || null));
      }
    } catch (e) { if (id === refreshId.current) setDriveError(String(e)); }
    finally { if (id === refreshId.current) setRefreshing(false); }
  }

  useEffect(() => {
    if (visible && !dump.running) void refresh(preferredDrive?.raw_device_path || selected);
    // Only entering this mode or choosing Browse's drive initiates a scan.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, preferredDrive]);
  useEffect(() => () => { refreshId.current++; }, []);

  async function action(run: () => Promise<unknown>) {
    setActionBusy(true);
    dump.setError(null);
    try { await run(); } catch (e) { dump.setError(String(e)); }
    finally { setActionBusy(false); }
  }

  async function chooseOutput() {
    const path = await open({ directory: true, title: "Choose parent folder for dump", defaultPath: output || undefined });
    if (typeof path === "string") { setOutput(path); dump.setError(null); }
  }

  async function refineDump() {
    if (!displayedJob?.can_refine || dump.running || browseBusy || commandPending || commandError) return;
    const approved = await confirm(
      `Refining will reread the original disc and update existing files in:\n${displayedJob.output_path}\n\nKeep the same disc in ${displayedJob.drive_name}. Existing image, state, log, and derived files may be modified or replaced. Continue?`,
      { title: "Refine existing dump?", kind: "warning", okLabel: "Refine Dump", cancelLabel: "Cancel" },
    );
    if (approved) {
      followLog.current = true;
      await dump.refine(displayedJob.id, true, beforeStart, manualCommand);
    }
  }

  async function refineExistingDump() {
    if (!canRecover || cannotStart || !drive) return;
    const approved = await confirm(
      `Refine the existing dump in:\n${destination}\n\nKeep the original disc in ${drive.name}. Current dumping settings will be used. Existing image, state, log, and derived files may be updated or replaced. Continue?`,
      { title: "Refine existing dump?", kind: "warning", okLabel: "Refine Dump", cancelLabel: "Cancel" },
    );
    if (!approved) return;
    followLog.current = true;
    await dump.refineExisting({ drive: drive.raw_device_path, drive_name: drive.name, output_parent: output,
      name, speed: speed ? Number(speed) : null, source, external_path: externalPath || null, options, manual_command: manualCommand }, true, beforeStart);
    setRecoveryRevision(value => value + 1);
  }

  function nextDisc() {
    setSetup(true);
    dump.setError(null);
    void refresh(job?.drive || selected, true);
  }

  const stateLabel = job?.status === "completed" ? "Dump complete"
    : job?.status === "warning" ? "Review this dump"
    : job?.status === "cancelled" ? "Dump stopped"
    : job?.status === "failed" ? "Dump failed"
    : job?.status === "stopping" ? "Stopping dump…" : job?.stage || "Starting…";
  const statusError = dump.error || driveError || recoveryError || commandError || (!showJob && (invalidName
    || (source === "external" && !externalPath ? "Choose redumper in Settings." : null)));
  const progressLabel = displayedJob?.stage.replace(/\bDUMP$/, "Dumping in Progress") || "Starting";
  const progressText = `${progressLabel}…${displayedJob?.progress.percentage == null ? "" : ` [${Math.floor(displayedJob.progress.percentage)}%]`}`;
  const statusText = statusError ? String(statusError).replace(/^Error:\s*/, "")
    : dump.starting ? "Starting dump…"
    : displayedJob?.status === "stopping" ? "Stopping dump… Partial files will be kept."
    : displayedJob?.status === "running" ? progressText
    : displayedJob ? displayedJob.message || stateLabel
    : browseBusy ? "Waiting for the current disc operation…"
    : refreshing ? "Scanning drives…"
    : !drive ? "Insert a disc or connect a drive, then refresh."
    : !drive.has_disc ? "No disc detected. Insert and refresh, or try dumping."
    : !output ? "Choose an output folder."
    : checkingRecovery ? "Checking output folder…"
    : commandPending ? "Preparing command…"
    : canRecover ? "Existing dump found. Refine to continue."
    : "Ready to dump";
  const statusWarning = !!statusError || displayedJob?.status === "warning" || displayedJob?.status === "failed";

  return (
    <main className="dump-workspace" hidden={!visible} aria-label="Disc dumping">
      <form id="dump-setup" className="dump-setup" onSubmit={e => {
        e.preventDefault();
        if (cannotStart || showJob || canRecover || !drive) return;
        followLog.current = true;
        void dump.start({ drive: drive.raw_device_path, drive_name: drive.name, output_parent: output,
          name, speed: speed ? Number(speed) : null, source, external_path: externalPath || null, options, manual_command: manualCommand }, beforeStart);
      }}>
        <div className="dump-form-row">
          <label htmlFor="dump-drive">Drive</label>
          <div className="dump-drive-row">
            <select id="dump-drive" className="settings-input" value={drivePath} disabled={locked || refreshing}
              onChange={e => {
                const next = drives.find(d => d.raw_device_path === e.target.value);
                setSelected(e.target.value); setName(dumpName(next?.volume_name || null)); dump.setError(null);
              }}>
              {!drivePath && <option value="">{refreshing ? "Scanning drives…" : "No optical drives available"}</option>}
              {displayedJob && !drive && <option value={displayedJob.drive}>{displayedJob.drive_name} · {displayedJob.drive}</option>}
              {drives.map(d => <option key={d.raw_device_path} value={d.raw_device_path}>{d.name} · {d.raw_device_path}</option>)}
            </select>
            <button type="button" className="dump-secondary dump-icon-button dump-refresh" title={refreshing ? "Scanning drives…" : "Refresh"}
              aria-label="Refresh" aria-busy={refreshing} onClick={() => void refresh()} disabled={refreshing || dump.running || actionBusy}>
              {refreshIcon}
            </button>
            <button type="button" className="dump-secondary dump-icon-button dump-eject" title="Eject Disc" aria-label="Eject Disc" disabled={!drivePath || dump.running || actionBusy}
              onClick={() => void action(async () => { await invoke("eject_disc", { path: drivePath }); await refresh(); })}>{ejectIcon}</button>
          </div>
        </div>
        <div className="dump-form-row">
          <label htmlFor="dump-name">Image name</label>
          <div className="dump-name-row">
            <input id="dump-name" className="settings-input" value={displayedJob?.name ?? name} onChange={e => { setName(e.target.value); dump.setError(null); }}
              disabled={locked} aria-invalid={!showJob && !!invalidName} aria-describedby={!showJob && invalidName ? "dump-status-text" : undefined} />
            <label htmlFor="dump-speed">Read speed</label>
            <select id="dump-speed" className="settings-input" value={speed} onChange={e => setSpeed(e.target.value)} disabled={locked}
              title="The drive may choose a supported speed">
              <option value="">Auto</option>
              {[1, 2, 4, 6, 8, 12, 16, 24, 32, 48].map(n => <option key={n} value={n}>{n}×</option>)}
            </select>
          </div>
        </div>
        <div className="dump-form-row">
          <span id="dump-folder-label">{showJob ? "Output folder" : "Save in"}</span>
          <div className="dump-folder-row">
            <div className="dump-folder-path settings-input" aria-labelledby="dump-folder-label" title={showJob ? destination : `New folder: ${destination}`}>
              {destination}
            </div>
            <button type="button" className="dump-secondary dump-icon-button" title="Choose output folder" aria-label="Choose output folder" onClick={() => void action(chooseOutput)} disabled={locked}>{folderIcon}</button>
          </div>
        </div>
      </form>

      {visible && actionsTarget && createPortal(<div className="dump-actions">
        {dump.running ? <button key="stop" type="button" className="btn-open" onClick={() => void dump.stop()} disabled={dump.starting || job?.status === "stopping"}>
          {dump.starting ? "Starting…" : "Stop Dump"}
        </button> : displayedJob ? <>
          {displayedJob.can_refine && <button type="button" className="btn-open" onClick={() => void action(refineDump)}
            disabled={actionBusy || browseBusy || !dump.ready || commandPending || !!commandError}>Refine Dump</button>}
          {displayedJob.image_path && (displayedJob.status === "completed" || displayedJob.status === "warning") &&
            <button type="button" className="btn-open" onClick={() => void action(() => browseImage(displayedJob.image_path!))} disabled={actionBusy}>Browse Dump</button>}
          <button type="button" className="btn-open" onClick={nextDisc} disabled={actionBusy}>Dump Next Disc</button>
        </> : canRecover ? <button key="recover" type="button" className="btn-open" disabled={cannotStart}
          onClick={() => void action(refineExistingDump)}>Refine Dump</button>
          : <button key="start" className="btn-open" type="submit" form="dump-setup" disabled={cannotStart}>Dump Disc</button>}
      </div>, actionsTarget)}

      <div className={`dump-monitor${options.advanced_command ? " dump-command-monitor" : ""}`}>
        {options.advanced_command ? <section className="dump-command-editor" aria-label="Manual CLI command">
          <textarea id="dump-command" className="settings-input" aria-label="Redumper command" rows={3} spellCheck={false} autoCapitalize="off" autoCorrect="off"
            value={commandText} readOnly={!commandEditable || actionBusy} aria-invalid={!!commandError} aria-describedby="dump-status-text"
            placeholder="Preparing redumper command…" title="Drive and output follow the fields above. Changing those fields regenerates the command."
            onChange={e => { setCommandDraft({ key: commandKey, value: e.target.value }); dump.setError(null); }} />
        </section> : <section className="dump-progress-card" aria-label="Dump progress">
          <div className="dump-metrics">
            <div><span>Elapsed</span><strong>{displayedJob ? elapsedDump(displayedJob, now) : "—"}</strong></div>
            <div className="dump-sector-metric"><span>Sectors · current pass</span><strong>{displayedJob?.progress.current == null ? "—" :
              `${displayedJob.progress.current.toLocaleString()} / ${displayedJob.progress.total?.toLocaleString() ?? "—"}`}</strong></div>
            {([ ["SCSI errors", "scsi", "Read errors reported by redumper; counted in samples for CDs and sectors for DVD/Blu-ray."],
                ["EDC errors", "edc", "Data integrity check failures reported by redumper."],
                ["C2 errors", "c2", "CD samples flagged with C2 errors by the drive; this is not a count of bad sectors."],
                ["Q errors", "q", "Sectors with Q subchannel errors reported by redumper."] ] as const).map(([label, key, hint]) =>
              <div key={key} title={hint}><span>{label}</span><strong>{displayedJob?.progress[key]?.toLocaleString() ?? "—"}</strong></div>)}
          </div>
        </section>}
      </div>

      <section className="dump-log-panel" aria-labelledby="dump-log-heading">
        <div className="dump-log-heading">
          <h2 id="dump-log-heading">Redumper Status</h2>
          <span id="dump-status-text" className={`dump-header-status${statusWarning ? " dump-header-error" : dump.running ? " dump-header-active" : displayedJob?.status === "completed" ? " dump-header-success" : ""}`}
            title={statusText} role={statusError ? "alert" : "status"}>{statusText}</span>
        </div>
        <pre id="dump-log-output" ref={logRef} tabIndex={0} onScroll={e => {
          const el = e.currentTarget; followLog.current = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
        }}>{displayedJob?.logs.join("\n") || (dump.running ? "Waiting for redumper…" : "Output will appear here when dumping starts.")}</pre>
      </section>
    </main>
  );
}
