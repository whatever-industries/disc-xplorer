import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { tr, localizeMessage } from "./i18n";

interface SubchannelRecord {
  q: (number | null)[];
  channels: number[][] | null;
  adr: number | null;
  control: number | null;
  track: string | null;
  index: string | null;
  relative_msf: string | null;
  absolute_msf: string | null;
  recorded_crc: number | null;
  calculated_crc: number | null;
  crc_valid: boolean | null;
  timing_matches: boolean | null;
}
interface SubchannelSource {
  id: string;
  name: string;
  format: string;
  records: number;
  issue: string | null;
  record: SubchannelRecord | null;
  previous_sector: number | null;
  next_sector: number | null;
  previous_lba: number | null;
  next_lba: number | null;
}
interface SubchannelReport { disc_lba: number | null; record_only: boolean; sources: SubchannelSource[]; }

const hex = (value: number | null, digits = 2) => value === null ? "—" : value.toString(16).padStart(digits, "0").toUpperCase();

export function SubchannelView({ imagePath, lba, onGo }: { imagePath: string; lba: number; onGo: (lba: number) => void }) {
  const [loaded, setLoaded] = useState<{ imagePath: string; lba: number; recordLba: number | null; report: SubchannelReport | null; error: string | null } | null>(null);
  const [selected, setSelected] = useState("");
  const [inspection, setInspection] = useState<{ imagePath: string; lba: number; recordLba: number } | null>(null);
  const recordLba = inspection?.imagePath === imagePath && inspection.lba === lba ? inspection.recordLba : null;
  useEffect(() => { setInspection(null); }, [imagePath, lba]);
  useEffect(() => {
    let cancelled = false;
    setLoaded(null);
    invoke<SubchannelReport>("read_subchannels", { imagePath, lba, recordLba }).then(value => {
      if (!cancelled) setLoaded({ imagePath, lba, recordLba, report: value, error: null });
    }).catch(e => { if (!cancelled) setLoaded({ imagePath, lba, recordLba, report: null, error: String(e) }); });
    return () => { cancelled = true; };
  }, [imagePath, lba, recordLba]);
  // Never paint a previous image/sector's Q bytes while the next read starts.
  const current = loaded?.imagePath === imagePath && loaded.lba === lba && loaded.recordLba === recordLba ? loaded : null;
  const report = current?.report;
  const error = current?.error;
  const source = report?.sources.find(s => s.id === selected) ?? report?.sources[0];
  const record = source?.record;
  function jump(target: number | null, sector: number | null) {
    if (target === null) return;
    if (sector !== null) { setInspection(null); onGo(sector); }
    else setInspection({ imagePath, lba, recordLba: target });
  }
  return <div className="sv-subchannels" aria-live="polite">
    {error ? <p className="sv-subchannel-warning">{localizeMessage(error)}</p>
      : !report ? <p>{tr("Loading…")}</p>
      : !source ? <p>{tr("No matching .sub, .lsd, or .sbi sidecar found.")}</p>
      : <>
        <div className="sv-subchannel-source">
          <select aria-label={tr("Subchannel source")} value={source.id} title={source.id} onChange={e => { setSelected(e.target.value); setInspection(null); }}>
            {report.sources.map(s => <option key={s.id} value={s.id}>{report.sources.filter(other => other.name === s.name).length > 1 ? s.id : s.name}</option>)}
          </select>
          <span>{source.format}</span>
        </div>
        <div className="sv-subchannel-source">
          <span>{tr("Records")}: {source.records.toLocaleString()}</span>
          {report.disc_lba !== null && <span>{tr("Disc LBA")}: {report.disc_lba.toLocaleString()}</span>}
          <button className="sv-btn" disabled={source.previous_lba === null} onClick={() => jump(source.previous_lba, source.previous_sector)}>{tr("◀ Record")}</button>
          <button className="sv-btn" disabled={source.next_lba === null} onClick={() => jump(source.next_lba, source.next_sector)}>{tr("Record ▶")}</button>
          {recordLba !== null && <button className="sv-btn" onClick={() => setInspection(null)}>{tr("Current sector")}</button>}
        </div>
        {report.record_only && <p className="sv-subchannel-note">{tr("Sidecar record only; no corresponding image sector.")}</p>}
        {source.issue && <p className="sv-subchannel-warning">{localizeMessage(source.issue)}</p>}
        {!record ? <p>{tr("No record stored for this sector.")}</p> : <>
          <dl className="sv-subchannel-fields">
            <dt>{tr("Q bytes")}</dt><dd className="sv-subchannel-bytes">{record.q.map(b => hex(b)).join(" ")}</dd>
            {record.adr !== null && <><dt>ADR / Control</dt><dd>{hex(record.adr)} / {hex(record.control)}</dd></>}
            {record.control !== null && <><dt>{tr("Flags")}</dt><dd>{[
              tr(record.control & 4 ? "Data" : "Audio"),
              record.control & 2 ? tr("Copy permitted") : null,
              !(record.control & 4) && record.control & 1 ? tr("Pre-emphasis") : null,
              !(record.control & 4) && record.control & 8 ? tr("4-channel audio") : null,
            ].filter(Boolean).join(" · ")}</dd></>}
            {record.track !== null && <><dt>{tr("Track")} / {tr("Index")}</dt><dd>{record.track} / {record.index}</dd></>}
            {record.relative_msf !== null && <><dt>{tr("Relative MSF")}</dt><dd>{record.relative_msf}</dd></>}
            {record.absolute_msf !== null && <><dt>{tr("Recorded MSF")}</dt><dd>{record.absolute_msf}</dd></>}
            <dt>{tr("Recorded CRC")}</dt><dd>{record.recorded_crc === null ? tr("Not recorded") : hex(record.recorded_crc, 4)}</dd>
            <dt>{tr("Calculated CRC")}</dt><dd>{record.calculated_crc === null ? tr("Not available") : hex(record.calculated_crc, 4)}</dd>
            {record.crc_valid !== null && <><dt>{tr("Q checksum")}</dt><dd className={record.crc_valid ? "sv-subchannel-match" : "sv-subchannel-warning"}>{tr(record.crc_valid ? "Matches" : "Mismatch")}</dd></>}
            {record.timing_matches !== null && <><dt>{tr("Timing")}</dt><dd className={record.timing_matches ? "" : "sv-subchannel-warning"}>{tr(record.timing_matches ? "Matches disc LBA" : "Differs from disc LBA")}</dd></>}
          </dl>
          {record.channels && <div className="sv-subchannel-channels">{record.channels.map((bytes, i) => <div key={i}>
            <strong>{"PQRSTUVW"[i]}</strong><code>{bytes.map(b => hex(b)).join(" ")}</code>
          </div>)}</div>}
        </>}
        <p className="sv-subchannel-note">{tr("Missing bytes are shown as —. SBI does not store CRC bytes. Checksum or timing differences can be intentional; they do not prove disc damage.")}</p>
      </>}
  </div>;
}
