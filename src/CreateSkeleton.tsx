import { useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { tr, localizeMessage } from "./i18n";

export function CreateSkeleton({ imagePath, source, externalPath, onClose }: {
  imagePath: string; source: string; externalPath: string; onClose: () => void;
}) {
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [progress, setProgress] = useState("");
  const [error, setError] = useState("");
  const [output, setOutput] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  async function start() {
    const parent = await open({ directory: true, title: tr("Choose destination folder") });
    if (typeof parent !== "string") return;
    const name = imagePath.split(/[/\\]/).pop()!.replace(/\.[^.]+$/, "");
    const destination = `${parent.replace(/[/\\]$/, "")}/${name}_skeleton`;
    setOutput(destination); setError(""); setFiles([]); setRunning(true); setStopping(false);
    setProgress(tr("Generating Skeleton..."));
    const channel = new Channel<string>();
    channel.onmessage = setProgress;
    try {
      setFiles(await invoke<string[]>("create_skeleton", {
        imagePath, outputPath: destination, source, externalPath: externalPath || null, progress: channel,
      }));
    } catch (e) { setError(localizeMessage(String(e))); }
    finally { setRunning(false); }
  }
  async function cancel() {
    setStopping(true);
    try { await invoke("cancel_skeleton"); }
    catch (e) { setError(localizeMessage(String(e))); setStopping(false); }
  }
  const close = () => { if (!running) onClose(); };
  return <div className="modal-overlay" onClick={close}>
    <div className="modal conv-modal" role="dialog" aria-modal="true" aria-labelledby="skeleton-title"
      onClick={e => e.stopPropagation()} onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); close(); } }}>
      <div className="modal-header"><span className="modal-title" id="skeleton-title">{tr("Create Skeleton")}</span></div>
      <div className="modal-body" aria-live="polite">
        <p>{tr("Creates .skeleton and .hash files in a new folder using redumper.")}</p>
        {output && <p style={{ overflowWrap: "anywhere" }}>{output}</p>}
        {running && <p style={{ overflowWrap: "anywhere" }}>{progress}</p>}
        {error && <p className="disc-info-error">{error}</p>}
        {files.length > 0 && <><p>{tr("Skeleton created.")}</p><ul>{files.map(file => <li key={file}>{file.split(/[/\\]/).pop()}</li>)}</ul></>}
      </div>
      <div className="modal-footer">
        {running ? <button className="btn-open" onClick={() => void cancel()} disabled={stopping}>{tr("Cancel")}</button>
          : <>{files.length === 0 && <button className="btn-open" onClick={() => void start()}>{tr("Create Skeleton")}</button>}
            <button className="btn-open" onClick={close}>{tr("Close")}</button></>}
      </div>
    </div>
  </div>;
}
