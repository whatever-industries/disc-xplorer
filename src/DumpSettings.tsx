export interface DumpOptions {
  advanced_command: boolean;
  any_drive: boolean;
  complete_with_errors: boolean;
  double_dump: boolean;
  correct_offset_shift: boolean;
  retries: number;
  log_archive: "7z" | "zip" | "off";
  auto_eject: boolean;
  skeleton: boolean;
  rings: boolean;
  verbose: boolean;
  refine_subchannel: boolean;
  refine_sector_mode: boolean;
  dvd_raw: boolean;
  bd_raw: boolean;
}

export const DEFAULT_DUMP_OPTIONS: DumpOptions = {
  advanced_command: false,
  any_drive: true,
  complete_with_errors: true,
  double_dump: false,
  correct_offset_shift: true,
  retries: 0,
  log_archive: "7z",
  auto_eject: false,
  skeleton: true,
  rings: false,
  verbose: false,
  refine_subchannel: false,
  refine_sector_mode: false,
  dvd_raw: false,
  bd_raw: false,
};

export function readDumpOptions(): DumpOptions {
  let saved: Partial<DumpOptions> = {};
  try { saved = JSON.parse(localStorage.getItem("dumpOptions") || "{}") || {}; } catch { /* Use defaults for corrupt preferences. */ }
  const result = { ...DEFAULT_DUMP_OPTIONS };
  for (const key of Object.keys(result) as (keyof DumpOptions)[]) {
    if (key === "log_archive") {
      if (saved.log_archive === "7z" || saved.log_archive === "zip" || saved.log_archive === "off") result.log_archive = saved.log_archive;
    } else if (key === "retries") {
      if (typeof saved.retries === "number" && Number.isInteger(saved.retries) && saved.retries >= 0 && saved.retries <= 10000) result.retries = saved.retries;
    } else if (typeof saved[key] === "boolean") result[key] = saved[key];
  }
  return result;
}

type ToggleKey = Exclude<keyof DumpOptions, "retries" | "log_archive">;
const ADVANCED_GROUPS: { title: string; options: [ToggleKey, string, string][] }[] = [
  { title: "Interface", options: [
    ["advanced_command", "Manual CLI command mode", "Replace dump metrics with an editable redumper command. Changes to the fields above regenerate the command."],
  ] },
  { title: "CD reading", options: [
    ["refine_subchannel", "Refine subchannel", "Retry sectors with subchannel errors (--refine-subchannel)."],
    ["refine_sector_mode", "Refine sector modes", "Retry sectors with inconsistent modes (--refine-sector-mode)."],
    ["rings", "Detect disc rings", "Use filesystem information to detect unreadable rings (--rings)."],
  ] },
  { title: "DVD / Blu-ray", options: [
    ["dvd_raw", "Read raw DVD sectors", "Requires compatible OmniDrive firmware (--dvd-raw)."],
    ["bd_raw", "Read raw Blu-ray sectors", "Requires compatible OmniDrive firmware (--bd-raw)."],
  ] },
  { title: "Output", options: [
    ["skeleton", "Generate skeleton", "Create an additional image with zeroed content for analysis (--skeleton)."],
    ["verbose", "Verbose logging", "Include more detail in redumper's log (--verbose)."],
  ] },
];

export function DumpSettings({ value, onChange, disabled, advanced = false }: {
  value: DumpOptions; onChange: (value: DumpOptions) => void; disabled: boolean; advanced?: boolean;
}) {
  const toggle = (key: ToggleKey, label: string, hint: string) => <div className="dump-setting-row" key={key} title={hint}>
    <label htmlFor={`dump-option-${key}`}>{label}</label>
    <input id={`dump-option-${key}`} type="checkbox" checked={value[key]} onChange={e => onChange({ ...value, [key]: e.target.checked })} />
  </div>;
  return <fieldset className="dump-settings dump-preferences" disabled={disabled}>
    <legend>{advanced ? "Advanced dumping" : "Dumping preferences"}</legend>
    {advanced ? ADVANCED_GROUPS.map(group => <section className="dump-setting-section" key={group.title} aria-label={group.title}>
      <h2>{group.title}</h2>
      {group.options.map(([key, label, hint]) => toggle(key, label, hint))}
    </section>) : <>
      <div className="dump-setting-row" title="Approved mode checks redumper’s recommended model list. Firmware requirements still apply.">
        <label htmlFor="dump-drive-mode">Drive compatibility</label>
        <select id="dump-drive-mode" className="settings-input" value={String(value.any_drive)} onChange={e => onChange({ ...value, any_drive: e.target.value === "true" })}>
          <option value="true">Works with Any Drive Model</option>
          <option value="false">Redump Approved Drives Only</option>
        </select>
      </div>
      <div className="dump-setting-row" title="Always Complete allows output despite read errors; the result still reports warnings. Recovery files are kept in either mode.">
        <label htmlFor="dump-error-mode">Read errors</label>
        <select id="dump-error-mode" className="settings-input" value={String(value.complete_with_errors)} onChange={e => onChange({ ...value, complete_with_errors: e.target.value === "true" })}>
          <option value="true">Always Complete Dump</option>
          <option value="false">Do not Complete Dump with Errors</option>
        </select>
      </div>
      {toggle("correct_offset_shift", "Correct CD write offsets", "Correct disc write-offset shifts (--correct-offset-shift). Enabled by default.")}
      <div className="dump-setting-row" title="Retries for sectors with SCSI or C2 errors. 0 uses redumper’s default of no extra retries.">
        <label htmlFor="dump-retries">Sector retries</label>
        <input id="dump-retries" className="settings-input dump-number" type="number" min={0} max={10000} step={1} value={value.retries}
          onChange={e => { const retries = Number(e.target.value); if (Number.isInteger(retries) && retries >= 0 && retries <= 10000) onChange({ ...value, retries }); }} />
      </div>
      {toggle("double_dump", "Double dump DVD / Blu-ray", "Read twice and compare SHA-256 hashes. Keeps both copies; requires twice the space. Does not apply to CDs or Refine Dump.")}
      {toggle("auto_eject", "Eject after successful dump", "Eject only after the whole job succeeds, including both dumps and hash comparison when enabled.")}
      <div className="dump-setting-row" title="Archive logs and auxiliary files after dumping. Waits for clean refinement if unresolved track errors remain. Uses installed 7z with ZIP fallback. For good dumps, archived originals are removed after verification.">
        <label htmlFor="dump-log-archive">Compress logs</label>
        <select id="dump-log-archive" className="settings-input" value={value.log_archive} onChange={e => onChange({ ...value, log_archive: e.target.value as DumpOptions["log_archive"] })}>
          <option value="7z">7z (ZIP fallback)</option>
          <option value="zip">ZIP</option>
          <option value="off">Off</option>
        </select>
      </div>
    </>}
    {disabled && <p>Stop the current dump before changing settings.</p>}
  </fieldset>;
}
