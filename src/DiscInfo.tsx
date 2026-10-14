import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { tr, localizeMessage } from "./i18n";

interface DiscInfoReport {
  filesystems: string[];
  tracks: { number: number; mode: string; start_lba: number; num_sectors: number }[];
  sections: { title: string; fields: { label: string; value: string }[] }[];
}

export function DiscInfo({ imagePath, onClose }: { imagePath: string; onClose: () => void }) {
  const [report, setReport] = useState<DiscInfoReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setReport(null);
    setError(null);
    invoke<DiscInfoReport>("disc_info", { imagePath })
      .then(value => { if (!cancelled) setReport(value); })
      .catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [imagePath]);

  return <div className="modal-overlay" onClick={onClose}>
    <div className="modal conv-modal disc-info-modal" role="dialog" aria-modal="true" aria-labelledby="disc-info-title"
      onClick={e => e.stopPropagation()} onKeyDown={e => {
        if (e.key === "Escape") { e.stopPropagation(); onClose(); }
        // Close is the only focusable control in this read-only dialog.
        if (e.key === "Tab") e.preventDefault();
      }}>
      <div className="modal-header"><span className="modal-title" id="disc-info-title">{tr("Disc Info")}</span></div>
      <div className="modal-body" aria-live="polite">
        <div className="disc-info-name" title={imagePath}>{imagePath.split(/[/\\]/).pop()}</div>
        {error ? <p className="disc-info-error">{localizeMessage(error)}</p> : !report ? <p>{tr("Loading…")}</p> : <>
          <dl className="disc-info-fields">
            <dt>{tr("Format")}</dt><dd>{imagePath.split('.').pop()?.toUpperCase()}</dd>
            {report.filesystems.length > 0 && <><dt>{tr("Filesystems")}</dt><dd>{report.filesystems.join(", ")}</dd></>}
          </dl>
          {report.sections.map((section, index) => <section key={index}>
            <h3>{section.title}</h3>
            <dl className="disc-info-fields">{section.fields.map(field => <div key={field.label}>
              <dt>{tr(field.label)}</dt><dd>{field.value}</dd>
            </div>)}</dl>
          </section>)}
          {report.tracks.length > 0 && <section>
            <h3>{tr("Tracks")}</h3>
            <table className="disc-info-tracks"><thead><tr>
              <th>{tr("Track")}</th><th>{tr("Mode")}</th><th>{tr("Start LBA")}</th><th>{tr("Sectors")}</th>
            </tr></thead><tbody>{report.tracks.map(track => <tr key={track.number}>
              <td>{track.number}</td><td>{track.mode}</td><td>{track.start_lba.toLocaleString()}</td><td>{track.num_sectors.toLocaleString()}</td>
            </tr>)}</tbody></table>
          </section>}
          {report.sections.length === 0 && <p>{tr("Header details are not available for this image.")}</p>}
        </>}
      </div>
      <div className="modal-footer"><button autoFocus className="btn-open" onClick={onClose}>{tr("Close")}</button></div>
    </div>
  </div>;
}
