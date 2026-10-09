import { useCallback, useEffect, useRef, useState } from "react";
import type { DumpOptions } from "./DumpSettings";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface DumpDrive {
  name: string;
  device_path: string;
  raw_device_path: string;
  has_disc: boolean;
  volume_name: string | null;
  mount_point: string | null;
}

export interface DumpJob {
  id: number;
  revision: number;
  drive: string;
  drive_name: string;
  name: string;
  output_path: string;
  image_path: string | null;
  can_refine: boolean;
  output_exists: boolean;
  status: "running" | "stopping" | "completed" | "warning" | "failed" | "cancelled";
  stage: string;
  message: string;
  command?: string;
  started_at: number;
  finished_at: number | null;
  progress: {
    percentage: number | null;
    current: number | null;
    total: number | null;
    scsi: number | null;
    edc: number | null;
    c2: number | null;
    q: number | null;
  };
  logs: string[];
}

export interface DumpRequest {
  drive: string;
  drive_name: string;
  output_parent: string;
  name: string;
  speed: number | null;
  source: string;
  external_path: string | null;
  options: DumpOptions;
  manual_command?: string | null;
}

export function dumpActive(job: DumpJob | null): boolean {
  return job?.status === "running" || job?.status === "stopping";
}

// Start responses and initial snapshots may arrive after newer progress events.
export function newerDump(current: DumpJob | null, incoming: DumpJob | null): DumpJob | null {
  if (!incoming) return current;
  if (current && (current.id > incoming.id || (current.id === incoming.id && current.revision > incoming.revision))) return current;
  return incoming;
}

export function dumpName(label: string | null, now = new Date()): string {
  const clean = (label || "").replace(/[\x00-\x1f/\\:*?"<>|]/g, "_").replace(/^\.+|[. ]+$/g, "").trim().slice(0, 100);
  if (clean && !nameError(clean)) return clean;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `Disc_${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
}

export function nameError(name: string): string | null {
  if (!name.trim()) return "Enter a disc name.";
  if (name.length > 100 || /^[.]/.test(name) || /[. ]$/.test(name) || /[\x00-\x1f/\\:*?"<>|]/.test(name)
    || /^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(?:\.|$)/i.test(name)) {
    return "Use a name of up to 100 characters without reserved filename characters.";
  }
  return null;
}

export function dumpOutput(parent: string, name: string): string {
  const sep = parent.includes("\\") ? "\\" : "/";
  return `${parent.replace(/[/\\]+$/, "")}${sep}${name}`;
}

// Display only: retain canonical extended paths for filesystem operations and CLI arguments.
export function displayDumpPath(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) return "\\\\" + path.slice(8);
  if (/^\\\\\?\\[A-Za-z]:\\/.test(path)) return path.slice(4);
  return path;
}

export function elapsedDump(job: DumpJob, now: number): string {
  const seconds = Math.max(0, Math.floor(((job.finished_at ?? now) - job.started_at) / 1000));
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

export function useDiscDump(enabled: boolean) {
  const [job, setJob] = useState<DumpJob | null>(null);
  const [ready, setReady] = useState(false);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const startingRef = useRef(false);
  const startingDrive = useRef("");
  const jobRef = useRef<DumpJob | null>(null);
  const receive = useCallback((incoming: DumpJob | null) => {
    jobRef.current = newerDump(jobRef.current, incoming);
    setJob(jobRef.current);
  }, []);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let off: (() => void) | undefined;
    void (async () => {
      try {
        off = await listen<DumpJob>("dump-job", e => { if (!disposed) receive(e.payload); });
        if (disposed) { off(); return; }
        const initial = await invoke<DumpJob | null>("get_dump_job");
        if (!disposed) { receive(initial); setReady(true); }
      } catch (e) { if (!disposed) setError(`Could not connect to the dumping engine: ${e}`); }
    })();
    return () => { disposed = true; off?.(); };
  }, [enabled, receive]);

  async function launch(command: string, args: Record<string, unknown>, drive: string, beforeStart: () => void) {
    if (!ready || startingRef.current || dumpActive(jobRef.current)) return;
    startingRef.current = true;
    startingDrive.current = drive;
    setStarting(true);
    setError(null);
    try {
      beforeStart();
      receive(await invoke<DumpJob>(command, args));
    } catch (e) { setError(String(e)); }
    finally { startingRef.current = false; setStarting(false); }
  }

  async function start(request: DumpRequest, beforeStart: () => void) {
    await launch("start_redumper_dump", { request }, request.drive, beforeStart);
  }

  async function refine(id: number, overwriteConfirmed: boolean, beforeStart: () => void, manualCommand?: string | null) {
    const current = jobRef.current;
    if (!overwriteConfirmed || !current || current.id !== id || !current.can_refine) return;
    await launch("refine_redumper_dump", { id, overwriteConfirmed, ...(manualCommand != null ? { manualCommand } : {}) }, current.drive, beforeStart);
  }

  async function refineExisting(request: DumpRequest, overwriteConfirmed: boolean, beforeStart: () => void) {
    if (!overwriteConfirmed) return;
    await launch("refine_existing_redumper_dump", { request, overwriteConfirmed }, request.drive, beforeStart);
  }

  const refresh = useCallback(async () => {
    const snapshot = await invoke<DumpJob | null>("get_dump_job");
    receive(snapshot);
    return jobRef.current;
  }, [receive]);

  async function stop() {
    const current = jobRef.current;
    if (!current || !dumpActive(current)) return;
    setError(null);
    try { await invoke("cancel_redumper_dump", { id: current.id }); }
    catch (e) { setError(String(e)); }
  }

  function isDriveReserved(drive: string) {
    return (startingRef.current && startingDrive.current === drive)
      || (dumpActive(jobRef.current) && jobRef.current?.drive === drive);
  }

  return { job, ready, starting, running: starting || dumpActive(job), error, setError, start, refine, refineExisting, refresh, stop, isDriveReserved };
}

export type DumpController = ReturnType<typeof useDiscDump>;
