/** Settings → Backup: export the instance to an encrypted `.lyra` archive,
 * manage the archives kept on the server, and import (additive-merge) an
 * archive back. Export/import run as jobs; this card polls
 * `backupJobStatus` every 2s until the job settles. */

import { useEffect, useRef, useState } from 'react';
import { Archive, Download, Trash2, Upload } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { t, type SupportedLocale } from '@/i18n';
import { formatBytes } from '@/lib/attachments';
import {
  backupJobStatus,
  deleteArtifact,
  downloadArtifact,
  isExportReport,
  isImportReport,
  listArtifacts,
  startExport,
  uploadBackup,
  validatePassword,
  type BackupArtifact,
  type BackupJobStatus,
  type ImportMergeReport,
} from '@/lib/backup';
import { confirmAction } from '@/lib/confirm-action';

const inputClass =
  'h-8 w-full max-w-md rounded-lg border border-input bg-background px-2.5 text-[13px]';

const POLL_MS = 2000;

/** Consecutive status-read failures before a flow settles with an error. */
const MAX_POLL_FAILURES = 5;

const MERGE_SECTIONS = ['accounts', 'folders', 'messages', 'contacts', 'calendars'] as const;

type PollTimer = { current: ReturnType<typeof setInterval> | null };

export function BackupSettings({ locale }: { locale: SupportedLocale }) {
  const [artifacts, setArtifacts] = useState<BackupArtifact[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [downloadingId, setDownloadingId] = useState<string | null>(null);

  const [exportPw, setExportPw] = useState('');
  const [exportPw2, setExportPw2] = useState('');
  const [exportBusy, setExportBusy] = useState(false);
  const [exportPhase, setExportPhase] = useState<string | null>(null);
  const [exportError, setExportError] = useState<string | null>(null);
  const [exportDone, setExportDone] = useState(false);
  /** The artifact a finished export produced — drives the direct download. */
  const [exportArtifact, setExportArtifact] = useState<BackupArtifact | null>(null);

  const [importFile, setImportFile] = useState<File | null>(null);
  const [fileInputKey, setFileInputKey] = useState(0);
  const [importPw, setImportPw] = useState('');
  const [importBusy, setImportBusy] = useState(false);
  const [uploadPct, setUploadPct] = useState<number | null>(null);
  const [importPhase, setImportPhase] = useState<string | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const [importReport, setImportReport] = useState<ImportMergeReport | null>(null);

  // Export and import poll independently — a shared timer would let one
  // flow cancel the other's poll and strand its busy flag.
  const exportPollTimer: PollTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const importPollTimer: PollTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  useEffect(
    () => () => {
      if (exportPollTimer.current) clearInterval(exportPollTimer.current);
      if (importPollTimer.current) clearInterval(importPollTimer.current);
    },
    [],
  );

  const refreshArtifacts = async (): Promise<BackupArtifact[] | null> => {
    try {
      const items = await listArtifacts();
      setArtifacts(items);
      setListError(null);
      return items;
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
      return null;
    }
  };

  useEffect(() => {
    listArtifacts()
      .then((items) => {
        setArtifacts(items);
        setListError(null);
      })
      .catch((e: unknown) => setListError(e instanceof Error ? e.message : String(e)));
  }, []);

  const stopPoll = (timer: PollTimer) => {
    if (timer.current) clearInterval(timer.current);
    timer.current = null;
  };

  /** Poll until the job settles; after MAX_POLL_FAILURES consecutive read
   * failures the flow settles via `onPollFailed` instead of polling forever. */
  const pollJob = (
    timer: PollTimer,
    jobId: string,
    onPhase: (phase: string | null) => void,
    onSettled: (s: BackupJobStatus) => void,
    onPollFailed: () => void,
  ) => {
    stopPoll(timer);
    let failures = 0;
    timer.current = setInterval(() => {
      void backupJobStatus(jobId)
        .then((s) => {
          failures = 0;
          onPhase(s.progress?.phase ?? null);
          if (s.status === 'completed' || s.status === 'failed') {
            stopPoll(timer);
            onSettled(s);
          }
        })
        .catch(() => {
          failures += 1;
          if (failures >= MAX_POLL_FAILURES) {
            stopPoll(timer);
            onPollFailed();
          }
        });
    }, POLL_MS);
  };

  const phaseLabel = (phase: string | null): string => {
    if (!phase) return '';
    const key = `settings.backup.phase.${phase}`;
    const label = t(locale, key);
    return label === key ? phase : label;
  };

  const fmtDate = (iso: string) =>
    new Date(iso).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US');

  // ── Export ────────────────────────────────────────────────────────

  const exportMismatch = exportPw2.length > 0 && exportPw !== exportPw2;
  const exportTooShort = exportPw.length > 0 && validatePassword(exportPw) !== null;
  const exportDisabled =
    exportBusy ||
    !exportPw ||
    !exportPw2 ||
    exportPw !== exportPw2 ||
    validatePassword(exportPw) !== null;

  const handleExport = async () => {
    setExportBusy(true);
    setExportError(null);
    setExportDone(false);
    setExportArtifact(null);
    setExportPhase('queued');
    try {
      const { job_id } = await startExport(exportPw);
      pollJob(
        exportPollTimer,
        job_id,
        setExportPhase,
        (s) => {
          setExportBusy(false);
          setExportPhase(null);
          const report = s.report;
          if (s.status === 'completed' && report && isExportReport(report)) {
            setExportDone(true);
            setExportPw('');
            setExportPw2('');
            void refreshArtifacts().then((items) => {
              setExportArtifact(items?.find((a) => a.id === report.artifact_id) ?? null);
            });
          } else {
            setExportError(
              report && !report.ok ? report.error : t(locale, 'settings.backup.export.failed'),
            );
          }
        },
        () => {
          setExportBusy(false);
          setExportPhase(null);
          setExportError(t(locale, 'settings.backup.pollFailed'));
        },
      );
    } catch (e) {
      setExportBusy(false);
      setExportPhase(null);
      setExportError(e instanceof Error ? e.message : String(e));
    }
  };

  // ── Archives ───────────────────────────────────────────────────────

  const handleDownload = async (artifact: BackupArtifact) => {
    setDownloadingId(artifact.id);
    setListError(null);
    try {
      await downloadArtifact(artifact.id, artifact.filename);
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
    } finally {
      setDownloadingId(null);
    }
  };

  const handleDelete = async (artifact: BackupArtifact) => {
    const ok = await confirmAction({
      title: t(locale, 'settings.backup.archives.confirmDeleteTitle'),
      description: t(locale, 'settings.backup.archives.confirmDeleteDesc', {
        name: artifact.filename,
      }),
      confirmLabel: t(locale, 'common.delete'),
      cancelLabel: t(locale, 'common.cancel'),
      tone: 'destructive',
    });
    if (!ok) return;
    setDeletingId(artifact.id);
    setListError(null);
    try {
      await deleteArtifact(artifact.id);
      await refreshArtifacts();
      if (exportArtifact?.id === artifact.id) setExportArtifact(null);
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeletingId(null);
    }
  };

  // ── Import ─────────────────────────────────────────────────────────

  const importDisabled = importBusy || !importFile || !importPw;

  const handleImport = async () => {
    if (!importFile) return;
    setImportBusy(true);
    setImportError(null);
    setImportReport(null);
    setUploadPct(0);
    setImportPhase(null);
    try {
      const { jobId } = await uploadBackup(importFile, importPw, setUploadPct);
      setUploadPct(null);
      setImportPhase('decrypting');
      pollJob(
        importPollTimer,
        jobId,
        setImportPhase,
        (s) => {
          setImportBusy(false);
          setImportPhase(null);
          const report = s.report;
          if (s.status === 'completed' && report && isImportReport(report)) {
            setImportReport(report.report);
            setImportFile(null);
            setImportPw('');
            setFileInputKey((k) => k + 1);
          } else {
            setImportError(
              report && !report.ok ? report.error : t(locale, 'settings.backup.import.failed'),
            );
          }
        },
        () => {
          setImportBusy(false);
          setImportPhase(null);
          setImportError(t(locale, 'settings.backup.pollFailed'));
        },
      );
    } catch (e) {
      setImportBusy(false);
      setUploadPct(null);
      setImportError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="space-y-4">
      <section className="space-y-4 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex items-start gap-2.5">
          <Archive className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div>
            <h2 className="text-[13px] font-medium">{t(locale, 'settings.backup.export.title')}</h2>
            <p className="text-xs text-muted-foreground">
              {t(locale, 'settings.backup.export.hint')}
            </p>
          </div>
        </div>
        <label className="flex flex-col gap-1.5">
          <span className="text-[13px] font-medium">
            {t(locale, 'settings.backup.export.password')}
          </span>
          <Input
            className={inputClass}
            type="password"
            autoComplete="new-password"
            value={exportPw}
            disabled={exportBusy}
            onChange={(e) => setExportPw(e.target.value)}
          />
        </label>
        <label className="flex flex-col gap-1.5">
          <span className="text-[13px] font-medium">
            {t(locale, 'settings.backup.export.confirm')}
          </span>
          <Input
            className={inputClass}
            type="password"
            autoComplete="new-password"
            value={exportPw2}
            disabled={exportBusy}
            onChange={(e) => setExportPw2(e.target.value)}
          />
        </label>
        {exportTooShort ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.backup.export.tooShort')}
          </p>
        ) : null}
        {exportMismatch ? (
          <p className="text-xs text-destructive">{t(locale, 'settings.backup.export.mismatch')}</p>
        ) : null}
        <div className="flex items-center gap-3">
          <Button
            variant="outline"
            size="sm"
            disabled={exportDisabled}
            onClick={() => void handleExport()}
          >
            {exportBusy
              ? t(locale, 'settings.backup.export.creating')
              : t(locale, 'settings.backup.export.create')}
          </Button>
          {exportBusy && exportPhase ? (
            <span className="text-xs text-muted-foreground">{phaseLabel(exportPhase)}…</span>
          ) : null}
          {exportDone ? (
            <span className="text-xs text-ok">{t(locale, 'settings.backup.export.success')}</span>
          ) : null}
          {exportDone && exportArtifact ? (
            <Button
              variant="outline"
              size="sm"
              disabled={downloadingId === exportArtifact.id}
              onClick={() => void handleDownload(exportArtifact)}
            >
              {t(locale, 'settings.backup.archives.download')}
            </Button>
          ) : null}
          {exportError ? <span className="text-xs text-destructive">{exportError}</span> : null}
        </div>
      </section>

      <section className="space-y-3 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex items-start gap-2.5">
          <Download className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div>
            <h2 className="text-[13px] font-medium">
              {t(locale, 'settings.backup.archives.title')}
            </h2>
            <p className="text-xs text-muted-foreground">
              {t(locale, 'settings.backup.archives.hint')}
            </p>
          </div>
        </div>
        {artifacts === null && !listError ? (
          <p className="text-xs text-muted-foreground">{t(locale, 'common.loading')}</p>
        ) : null}
        {artifacts !== null && artifacts.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            {t(locale, 'settings.backup.archives.empty')}
          </p>
        ) : null}
        {artifacts?.map((artifact) => (
          <div
            key={artifact.id}
            className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3"
          >
            <div className="min-w-0">
              <div className="truncate text-[13px] font-medium">{artifact.filename}</div>
              <div className="text-xs text-muted-foreground">
                {formatBytes(artifact.size_bytes)} · {fmtDate(artifact.created_at)}
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={downloadingId === artifact.id || deletingId === artifact.id}
                onClick={() => void handleDownload(artifact)}
              >
                {t(locale, 'settings.backup.archives.download')}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={deletingId === artifact.id}
                onClick={() => void handleDelete(artifact)}
                aria-label={t(locale, 'settings.backup.archives.delete')}
              >
                <Trash2 className="size-4 text-destructive" aria-hidden />
              </Button>
            </div>
          </div>
        ))}
        {listError ? <p className="text-xs text-destructive">{listError}</p> : null}
      </section>

      <section className="space-y-4 rounded-[10px] border border-border bg-card px-5 py-4">
        <div className="flex items-start gap-2.5">
          <Upload className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          <div>
            <h2 className="text-[13px] font-medium">{t(locale, 'settings.backup.import.title')}</h2>
            <p className="text-xs text-muted-foreground">
              {t(locale, 'settings.backup.import.hint')}
            </p>
          </div>
        </div>
        <label className="flex flex-col gap-1.5">
          <span className="text-[13px] font-medium">
            {t(locale, 'settings.backup.import.chooseFile')}
          </span>
          <Input
            key={fileInputKey}
            className={inputClass}
            type="file"
            accept=".lyra"
            disabled={importBusy}
            onChange={(e) => setImportFile(e.target.files?.[0] ?? null)}
          />
        </label>
        <label className="flex flex-col gap-1.5">
          <span className="text-[13px] font-medium">
            {t(locale, 'settings.backup.import.password')}
          </span>
          <Input
            className={inputClass}
            type="password"
            autoComplete="off"
            value={importPw}
            disabled={importBusy}
            onChange={(e) => setImportPw(e.target.value)}
          />
        </label>
        <div className="flex items-center gap-3">
          <Button
            variant="outline"
            size="sm"
            disabled={importDisabled}
            onClick={() => void handleImport()}
          >
            {importBusy
              ? t(locale, 'settings.backup.import.importing')
              : t(locale, 'settings.backup.import.start')}
          </Button>
          {uploadPct !== null ? (
            <span className="text-xs text-muted-foreground">
              {t(locale, 'settings.backup.import.uploading', { pct: uploadPct })}
            </span>
          ) : null}
          {importBusy && importPhase ? (
            <span className="text-xs text-muted-foreground">{phaseLabel(importPhase)}…</span>
          ) : null}
          {importError ? <span className="text-xs text-destructive">{importError}</span> : null}
        </div>
        {importReport ? (
          <div className="space-y-1 border-t border-border pt-3">
            <div className="text-[13px] font-medium text-ok">
              {t(locale, 'settings.backup.import.success')}
            </div>
            <div className="text-xs text-muted-foreground">
              {t(locale, 'settings.backup.sections.settings')}:{' '}
              {importReport.settings
                ? t(locale, 'settings.backup.import.settingsImported')
                : t(locale, 'settings.backup.import.settingsSkipped')}
            </div>
            {MERGE_SECTIONS.map((section) => (
              <div key={section} className="text-xs text-muted-foreground">
                {t(locale, `settings.backup.sections.${section}`)}:{' '}
                {t(locale, 'settings.backup.import.counts', {
                  inserted: importReport[section].inserted,
                  skipped: importReport[section].skipped,
                  repaired: importReport[section].repaired,
                  failed: importReport[section].failed,
                })}
              </div>
            ))}
            {importReport.errors.length > 0 ? (
              <div className="text-xs text-destructive">
                <div>{t(locale, 'settings.backup.import.errors')}</div>
                <ul className="list-disc pl-4">
                  {importReport.errors.map((err, i) => (
                    <li key={i}>{err}</li>
                  ))}
                </ul>
              </div>
            ) : null}
          </div>
        ) : null}
      </section>
    </div>
  );
}
