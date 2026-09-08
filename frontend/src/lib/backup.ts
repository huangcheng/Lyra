/**
 * Backup export/import client: `/api/v1/backup` wrappers, the chunked
 * `.lyra` upload flow, and the browser download helper. Settings → Backup
 * renders on top of this module; job polling (2s interval) is the caller's
 * job, driven off {@link backupJobStatus}.
 */

import { api, apiBlob } from '@/lib/api-client';

export interface BackupArtifact {
  id: string;
  filename: string;
  size_bytes: number;
  created_at: string;
}

/** kv `backup:progress:<job>` payload; `section` only during import merges. */
export interface BackupJobProgress {
  phase?: string;
  section?: string;
}

export interface ExportSectionCounts {
  settings: boolean;
  accounts: number;
  messages: number;
  contacts: number;
  calendars: number;
  blobs: number;
}

/** `{"ok":true,…}` report written by a finished export job. */
export interface ExportReport {
  ok: true;
  artifact_id: string;
  sections: ExportSectionCounts;
  warnings: string[];
}

export interface ImportSectionCounts {
  inserted: number;
  skipped: number;
  repaired: number;
  failed: number;
}

/** Per-section merge tallies inside a finished import job's report. */
export interface ImportMergeReport {
  settings: boolean;
  accounts: ImportSectionCounts;
  folders: ImportSectionCounts;
  messages: ImportSectionCounts;
  contacts: ImportSectionCounts;
  calendars: ImportSectionCounts;
  errors: string[];
}

/** `{"ok":true,…}` report written by a finished import job. */
export interface ImportReport {
  ok: true;
  report: ImportMergeReport;
}

/** `{"ok":false,"error":…}` written before a failed job propagates. */
export interface BackupFailureReport {
  ok: false;
  error: string;
}

export type BackupReport = ExportReport | ImportReport | BackupFailureReport;

export interface BackupJobStatus {
  status: 'pending' | 'running' | 'completed' | 'failed';
  progress?: BackupJobProgress | null;
  report?: BackupReport | null;
}

export function isExportReport(report: BackupReport): report is ExportReport {
  return report.ok && 'artifact_id' in report;
}

export function isImportReport(report: BackupReport): report is ImportReport {
  return report.ok && 'report' in report;
}

export const MIN_PASSWORD = 8;

/** Shared by the export/import forms; returns an i18n-ready reason or null. */
export function validatePassword(pw: string): 'tooShort' | null {
  return pw.length < MIN_PASSWORD ? 'tooShort' : null;
}

/** Queue an export job; the artifact materializes when the job completes. */
export function startExport(password: string): Promise<{ job_id: string; artifact_id: string }> {
  return api('/backup/export', { method: 'POST', body: JSON.stringify({ password }) });
}

export function backupJobStatus(jobId: string): Promise<BackupJobStatus> {
  return api(`/backup/jobs/${jobId}`);
}

export async function listArtifacts(): Promise<BackupArtifact[]> {
  const res = await api<{ artifacts: BackupArtifact[] }>('/backup/artifacts');
  return res.artifacts;
}

export function deleteArtifact(id: string): Promise<void> {
  return api(`/backup/artifacts/${id}`, { method: 'DELETE' });
}

/** Fetch artifact bytes and trigger a browser download. */
export async function downloadArtifact(id: string, filename: string): Promise<void> {
  const blob = await apiBlob(`/backup/artifacts/${id}/download`);
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/** Server-side staging chunk size (8 MiB); also the fallback below. */
export const UPLOAD_CHUNK_SIZE = 8 * 1024 * 1024;

/**
 * Stage a `.lyra` archive: POST an upload, PUT each chunk sequentially
 * (raw bytes — an explicit Content-Type keeps `api()` from forcing JSON),
 * then finish to queue the import job. `onProgress` fires after each chunk
 * with 0–100. A 400 from finish surfaces with `serverCode` intact (e.g.
 * `upload_incomplete`).
 */
export async function uploadBackup(
  file: File,
  password: string,
  onProgress?: (pct: number) => void,
): Promise<{ jobId: string }> {
  const { upload_id, chunk_size } = await api<{ upload_id: string; chunk_size: number }>(
    '/backup/import/uploads',
    { method: 'POST' },
  );
  const size = chunk_size > 0 ? chunk_size : UPLOAD_CHUNK_SIZE;
  const total = Math.max(1, Math.ceil(file.size / size));
  for (let n = 0; n < total; n += 1) {
    const chunk = file.slice(n * size, Math.min(file.size, (n + 1) * size));
    await api(`/backup/import/uploads/${upload_id}/chunks/${n}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: chunk,
    });
    onProgress?.(Math.round(((n + 1) / total) * 100));
  }
  const { job_id } = await api<{ job_id: string }>(`/backup/import/uploads/${upload_id}/finish`, {
    method: 'POST',
    body: JSON.stringify({ password, total_chunks: total }),
  });
  return { jobId: job_id };
}
