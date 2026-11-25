import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { tr } from "./i18n";

export interface AudioCdPreferences {
  lookupOnOpen: boolean;
  frontCover: boolean;
}

const PREFERENCES = "audioCdPreferences";
export function readAudioCdPreferences(): AudioCdPreferences {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFERENCES) ?? "{}");
    return { lookupOnOpen: saved?.lookupOnOpen === true, frontCover: saved?.frontCover === true };
  } catch {
    return { lookupOnOpen: false, frontCover: false };
  }
}

export function saveAudioCdPreferences(settings: AudioCdPreferences) {
  try { localStorage.setItem(PREFERENCES, JSON.stringify(settings)); } catch { /* Keep the in-memory setting. */ }
}

interface Names { title?: string; performer?: string }
interface CdText { disc: Names; tracks: Record<string, Names> }
interface PhysicalDisc { identity: string; tracks: { number: number; start: number; end: number }[] }
interface TrackRow { track_number: number; is_data: boolean }
interface Track { number: number; title: string; artist: string }
interface Cover { mime: string; data: string }
interface AlbumFields {
  album: string;
  artist: string;
  date: string;
  disc: number | null;
  disc_total: number | null;
  tracks: Track[];
}
interface Album extends AlbumFields {
  id: string;
  country: string;
  label: string;
  catalog: string;
}
interface ApprovedAlbum { fields: AlbumFields; cover: Cover | null }
interface Lookup { key: string; busy: boolean; releases: Album[]; error?: string }
interface VerificationReport {
  message: string;
  detail: string | null;
  coverage: string;
  text: string;
  matches: { id: number; shift: number; confidence: number; crc32: string }[];
}

let verificationSequence = 0;
type VerifyState = { key: string; id: string; result?: VerificationReport; error?: string };

export function useAudioCdTools(
  imagePath: string | null, fromDrive: boolean, physical: PhysicalDisc | null,
  tracks: TrackRow[], cdText: CdText | null, preferences: AudioCdPreferences, enabled: boolean,
) {
  const audioCue = !fromDrive && Boolean(imagePath && /\.cue$/i.test(imagePath)) &&
    tracks.some(track => !track.is_data) && tracks.every(track => !track.is_data);
  const physicalSource = fromDrive ? physical : null;
  const key = enabled && (physicalSource ? `drive:${imagePath}:${physicalSource.identity}` : audioCue ? imagePath : null);
  const sourceRef = useRef(key);
  sourceRef.current = key;
  const request = useRef(0);
  const lastLookup = useRef<string | null>(null);
  const verification = useRef<VerifyState | null>(null);
  const [lookup, setLookup] = useState<Lookup | null>(null);
  const [albumOpen, setAlbumOpen] = useState<string | null>(null);
  const [approved, setApproved] = useState<{ key: string; album: ApprovedAlbum } | null>(null);
  const [report, setReport] = useState<VerifyState | null>(null);

  function lookupAlbum() {
    if (!key) return;
    lastLookup.current = key;
    const id = ++request.current;
    setLookup({ key, busy: true, releases: [] });
    const args = physicalSource
      ? { imagePath: null, starts: physicalSource.tracks.map(track => track.start), leadOut: physicalSource.tracks[physicalSource.tracks.length - 1]?.end }
      : { imagePath, starts: null, leadOut: null };
    void invoke<Album[]>("lookup_audio_releases", args).then(releases => {
      if (sourceRef.current === key && request.current === id) setLookup({ key, busy: false, releases });
    }).catch(error => {
      if (sourceRef.current === key && request.current === id) setLookup({ key, busy: false, releases: [], error: String(error) });
    });
  }

  useEffect(() => {
    if (key && preferences.lookupOnOpen && lastLookup.current !== key) lookupAlbum();
  }, [key, preferences.lookupOnOpen]);

  function cancelVerification() {
    const active = verification.current;
    verification.current = null;
    setReport(null);
    if (active) void invoke("cancel_audio_verification", { requestId: active.id }).catch(() => {});
  }

  useEffect(() => {
    if (report && report.key !== key) cancelVerification();
    if (albumOpen && albumOpen !== key) setAlbumOpen(null);
    if (approved && approved.key !== key) setApproved(null);
    if (!key) {
      lastLookup.current = null;
      setLookup(null);
    }
  }, [key, albumOpen, approved, report]);

  useEffect(() => () => {
    const active = verification.current;
    if (active) void invoke("cancel_audio_verification", { requestId: active.id }).catch(() => {});
  }, []);

  function startVerification() {
    if (!key || !audioCue || !imagePath) return;
    cancelVerification();
    const current = { key, id: `${Date.now()}-${++verificationSequence}` };
    verification.current = current;
    setReport(current);
    void invoke<VerificationReport>("verify_audio_rip", { imagePath, requestId: current.id }).then(result => {
      if (verification.current?.id === current.id && sourceRef.current === key) {
        verification.current = null;
        setReport({ ...current, result });
      }
    }).catch(error => {
      if (verification.current?.id === current.id && sourceRef.current === key) {
        verification.current = null;
        setReport({ ...current, error: String(error) });
      }
    });
  }

  function showAlbum() {
    if (!key) return;
    setAlbumOpen(key);
    if (lastLookup.current !== key) lookupAlbum();
  }

  const album = approved && approved.key === key ? approved.album : null;
  function tagsForTrack(number: number) {
    const matched = album?.fields.tracks.find(track => track.number === number);
    if (!album || !matched) return null;
    return {
      album: album.fields.album, album_artist: album.fields.artist, title: matched.title,
      artist: matched.artist || album.fields.artist, date: album.fields.date,
      disc: album.fields.disc, disc_total: album.fields.disc_total, track: number,
      track_total: album.fields.tracks.length, cover: preferences.frontCover ? album.cover : null,
    };
  }

  const activeLookup = lookup && lookup.key === key ? lookup : null;
  return {
    canLookUp: Boolean(key),
    canVerify: Boolean(key && audioCue),
    releases: activeLookup?.releases.length ?? 0,
    albumTags: album ? { ...album.fields, cover: preferences.frontCover ? album.cover : null } : null,
    showAlbum, startVerification, tagsForTrack,
    dialogs: <>
      {key && albumOpen === key && <AlbumDialog key={key} approved={album} lookup={activeLookup}
        cdText={cdText} tracks={tracks} withCover={preferences.frontCover} onLookup={lookupAlbum}
        onClose={() => setAlbumOpen(null)} onApply={value => {
          setApproved(value ? { key, album: value } : null);
          setAlbumOpen(null);
        }} />}
      {report && report.key === key && <VerificationDialog state={report} source={imagePath ?? ""}
        onClose={cancelVerification} />}
    </>,
  };
}

function Dialog({ title, children, footer, onClose }: {
  title: string; children: ReactNode; footer: ReactNode; onClose: () => void;
}) {
  return <div className="modal-overlay" onClick={onClose}>
    <div className="modal audio-cd-modal" role="dialog" aria-modal="true" aria-label={title}
      onClick={event => event.stopPropagation()} onKeyDown={event => {
        if (event.key === "Escape") { event.stopPropagation(); onClose(); }
      }}>
      <div className="modal-header"><span className="modal-title">{title}</span></div>
      <div className="modal-body audio-cd-body">{children}</div>
      <div className="modal-footer">{footer}</div>
    </div>
  </div>;
}

function AlbumDialog({ approved, lookup, cdText, tracks, withCover, onLookup, onApply, onClose }: {
  approved: ApprovedAlbum | null; lookup: Lookup | null; cdText: CdText | null;
  tracks: TrackRow[]; withCover: boolean; onLookup: () => void;
  onApply: (value: ApprovedAlbum | null) => void; onClose: () => void;
}) {
  const [fields, setFields] = useState<AlbumFields>(() => approved?.fields ?? {
    album: cdText?.disc.title ?? "", artist: cdText?.disc.performer ?? "",
    date: "", disc: null, disc_total: null,
    tracks: tracks.filter(track => !track.is_data).map(({ track_number: number }) => ({
      number, title: cdText?.tracks[number]?.title ?? "", artist: cdText?.tracks[number]?.performer ?? "",
    })),
  });
  const prefill = useRef(!approved && !cdText);
  useEffect(() => {
    if (!prefill.current || !cdText) return;
    prefill.current = false;
    setFields(current => ({
      ...current,
      album: current.album || cdText.disc.title || "",
      artist: current.artist || cdText.disc.performer || "",
      tracks: current.tracks.map(track => ({
        ...track, title: track.title || cdText.tracks[track.number]?.title || "",
        artist: track.artist || cdText.tracks[track.number]?.performer || "",
      })),
    }));
  }, [cdText]);
  const [selection, setSelection] = useState("");
  const [cover, setCover] = useState<Cover | null>(approved?.cover ?? null);
  const [coverStatus, setCoverStatus] = useState("");
  const [coverBusy, setCoverBusy] = useState(false);
  const coverRequest = useRef(0);
  useEffect(() => () => { coverRequest.current++; }, []);

  function chooseRelease(value: string) {
    setSelection(value);
    const id = ++coverRequest.current;
    setCover(null);
    setCoverStatus("");
    setCoverBusy(false);
    const source = value && lookup?.releases[Number(value)];
    if (!source) return;
    prefill.current = false;
    setFields(source);
    if (!withCover) return;
    setCoverBusy(true);
    void invoke<Cover | null>("fetch_album_cover", { id: source.id }).then(result => {
      if (coverRequest.current !== id) return;
      setCover(result);
      if (!result) setCoverStatus(tr("No front cover found"));
    }).catch(() => {
      if (coverRequest.current === id) setCoverStatus(tr("Front cover unavailable"));
    }).finally(() => {
      if (coverRequest.current === id) setCoverBusy(false);
    });
  }

  function updateTrack(index: number, change: Partial<Track>) {
    setFields(current => ({ ...current, tracks: current.tracks.map((track, i) =>
      i === index ? { ...track, ...change } : track) }));
  }

  return <Dialog title={tr("Album Tags")} onClose={onClose} footer={<>
    {approved && <button className="btn-open btn-open-secondary" onClick={() => onApply(null)}>{tr("Clear Tags")}</button>}
    <button autoFocus className="btn-open btn-open-secondary" onClick={onClose}>{tr("Cancel")}</button>
    <button className="btn-open" onClick={() => onApply({ fields, cover: withCover ? cover : null })} disabled={coverBusy}>
      {tr("Use Album Tags")}
    </button>
  </>}>
    <div className="audio-cd-lookup">
      <label>{tr("MusicBrainz release")}
        <select className="settings-input" value={selection}
          onChange={event => chooseRelease(event.target.value)} disabled={!lookup?.releases.length}>
          <option value="">{tr("Choose a release")}</option>
          {lookup?.releases.map((release, index) => <option key={`${release.id}:${release.disc}`} value={index}>
            {[release.album, release.artist, release.country, release.date, release.label, release.catalog,
              release.disc_total != null && release.disc_total > 1 ? tr("Disc {0}", release.disc) : ""].filter(Boolean).join(" · ")}
          </option>)}
        </select>
      </label>
      <button className="btn-open btn-open-secondary" disabled={lookup?.busy} onClick={() => {
        setSelection("");
        onLookup();
      }}>{tr("Look Up Album")}</button>
    </div>
    {lookup?.busy && <p role="status">{tr("Looking up album…")}</p>}
    {lookup?.error && <p role="status">{tr("MusicBrainz lookup unavailable")} · {tr(lookup.error)}</p>}
    {lookup && !lookup.busy && !lookup.error && lookup.releases.length === 0 && <p role="status">{tr("No matching album found")}</p>}
    <div className="audio-cd-tag-fields">
      <label>{tr("Album")}<input className="settings-input" value={fields.album}
        onChange={event => setFields({ ...fields, album: event.target.value })} /></label>
      <label>{tr("Album artist")}<input className="settings-input" value={fields.artist}
        onChange={event => setFields({ ...fields, artist: event.target.value })} /></label>
      <label>{tr("Release date")}<input className="settings-input" value={fields.date}
        onChange={event => setFields({ ...fields, date: event.target.value })} /></label>
      <label>{tr("Disc number")}<input className="settings-input" type="number" min={1} max={99} value={fields.disc ?? ""}
        onChange={event => {
          const disc = event.target.valueAsNumber;
          setFields({ ...fields, disc: Number.isInteger(disc) && disc >= 1 && disc <= 99 ? disc : null });
        }} /></label>
    </div>
    {cover && withCover && <img className="audio-cd-cover" src={`data:${cover.mime};base64,${cover.data}`} alt={tr("Front cover")} />}
    {coverBusy && <p role="status">{tr("Loading front cover…")}</p>}
    {coverStatus && <p role="status">{coverStatus}</p>}
    <table className="audio-cd-track-tags">
      <thead><tr><th scope="col">{tr("Track")}</th><th scope="col">{tr("Title")}</th><th scope="col">{tr("Artist")}</th></tr></thead>
      <tbody>{fields.tracks.map((track, index) => <tr key={track.number}>
        <th scope="row">{track.number}</th>
        <td><input className="settings-input" aria-label={tr("Track {0} title", track.number)} value={track.title}
          onChange={event => updateTrack(index, { title: event.target.value })} /></td>
        <td><input className="settings-input" aria-label={tr("Track {0} artist", track.number)} value={track.artist}
          onChange={event => updateTrack(index, { artist: event.target.value })} /></td>
      </tr>)}</tbody>
    </table>
  </Dialog>;
}

function VerificationDialog({ state, source, onClose }: { state: VerifyState; source: string; onClose: () => void }) {
  const [saveError, setSaveError] = useState("");
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  const result = state.result;
  const pending = !result && !state.error;
  async function saveReport() {
    if (!result || saving) return;
    setSaving(true);
    setSaveError("");
    try {
      const leaf = source.split(/[/\\]/).pop()?.replace(/\.cue$/i, "") ?? "Audio";
      const path = await save({ defaultPath: `${leaf}_ctdb.txt`, filters: [{ name: tr("Text"), extensions: ["txt"] }] });
      if (!path) return;
      await invoke("save_audio_verification_report", { path, text: result.text });
      setSaved(true);
    } catch (error) {
      setSaveError(String(error));
    } finally {
      setSaving(false);
    }
  }
  return <Dialog title={tr("Verify Audio Rip")} onClose={onClose} footer={<>
    {result && <button className="btn-open btn-open-secondary" disabled={saving} onClick={() => void saveReport()}>{tr("Save Report")}</button>}
    <button autoFocus className="btn-open" onClick={onClose}>{tr(pending ? "Cancel" : "Close")}</button>
  </>}>
    {pending && <p role="status"><span className="extract-spinner" /> {tr("Verifying audio with CTDB")}</p>}
    {state.error && <p role="alert">{tr(state.error)}</p>}
    {result && <>
      <h3>{tr(result.message)}</h3>
      {result.detail && <p>{tr(result.detail)}</p>}
      {result.matches.map(match => <p key={match.id}>
        {tr("Reference {0} · Alignment {1} stereo samples · Confidence {2} · CRC32 {3}",
          match.id, match.shift > 0 ? `+${match.shift}` : match.shift, match.confidence, match.crc32)}
      </p>)}
      <p>{tr(result.coverage)}</p>
    </>}
    {saveError && <p role="alert">{saveError}</p>}
    {saved && <p role="status">{tr("Report saved")}</p>}
  </Dialog>;
}
