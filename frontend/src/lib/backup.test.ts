import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/lib/api-client')>();
  return { ...actual, api: vi.fn(), apiBlob: vi.fn() };
});

import { ApiError, api, apiBlob } from '@/lib/api-client';
import {
  MIN_PASSWORD,
  backupJobStatus,
  deleteArtifact,
  downloadArtifact,
  isExportReport,
  isImportReport,
  listArtifacts,
  startExport,
  uploadBackup,
  validatePassword,
  type BackupReport,
} from '@/lib/backup';

const mockedApi = vi.mocked(api);
const mockedApiBlob = vi.mocked(apiBlob);

const MIB = 1024 * 1024;

beforeEach(() => {
  mockedApi.mockReset();
  mockedApiBlob.mockReset();
});

describe('validatePassword', () => {
  it('rejects empty and short passwords, accepts MIN_PASSWORD and up', () => {
    expect(validatePassword('')).toBe('tooShort');
    expect(validatePassword('a'.repeat(MIN_PASSWORD - 1))).toBe('tooShort');
    expect(validatePassword('a'.repeat(MIN_PASSWORD))).toBeNull();
    expect(validatePassword('a'.repeat(MIN_PASSWORD + 5))).toBeNull();
  });
});

describe('startExport / job status / artifacts', () => {
  it('posts the password and returns the queued job', async () => {
    mockedApi.mockResolvedValue({ job_id: 'j-1', artifact_id: 'a-1' } as never);
    const res = await startExport('correct horse');
    expect(mockedApi).toHaveBeenCalledWith(
      '/backup/export',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ password: 'correct horse' }),
      }),
    );
    expect(res).toEqual({ job_id: 'j-1', artifact_id: 'a-1' });
  });

  it('reads job status by id', async () => {
    mockedApi.mockResolvedValue({ status: 'running', progress: { phase: 'mail' } } as never);
    const res = await backupJobStatus('j-1');
    expect(mockedApi).toHaveBeenCalledWith('/backup/jobs/j-1');
    expect(res.status).toBe('running');
  });

  it('unwraps the artifacts list', async () => {
    mockedApi.mockResolvedValue({
      artifacts: [{ id: 'a-1', filename: 'lyra-backup-x.lyra', size_bytes: 12, created_at: 't' }],
    } as never);
    const items = await listArtifacts();
    expect(mockedApi).toHaveBeenCalledWith('/backup/artifacts');
    expect(items).toHaveLength(1);
    expect(items[0].filename).toBe('lyra-backup-x.lyra');
  });

  it('DELETEs an artifact', async () => {
    mockedApi.mockResolvedValue(undefined as never);
    await deleteArtifact('a-1');
    expect(mockedApi).toHaveBeenCalledWith('/backup/artifacts/a-1', { method: 'DELETE' });
  });
});

describe('downloadArtifact', () => {
  it('fetches the blob and clicks an object-URL anchor', async () => {
    mockedApiBlob.mockResolvedValue(new Blob(['bytes']) as never);
    const clicks: string[] = [];
    const origCreate = document.createElement.bind(document);
    const createSpy = vi.spyOn(document, 'createElement').mockImplementation(((
      tag: string,
      ...rest: unknown[]
    ) => {
      const el = origCreate(tag, ...(rest as []));
      if (tag === 'a') {
        el.click = () => clicks.push((el as HTMLAnchorElement).download);
      }
      return el;
    }) as typeof document.createElement);
    const urlSpy = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:mock');
    const revokeSpy = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});

    await downloadArtifact('a-1', 'lyra-backup-x.lyra');

    expect(mockedApiBlob).toHaveBeenCalledWith('/backup/artifacts/a-1/download');
    expect(clicks).toEqual(['lyra-backup-x.lyra']);
    expect(revokeSpy).toHaveBeenCalledWith('blob:mock');
    createSpy.mockRestore();
    urlSpy.mockRestore();
    revokeSpy.mockRestore();
  });
});

describe('uploadBackup', () => {
  it('slices a 20 MiB file into 3 chunks (8+8+4) in order, then finishes', async () => {
    const file = new File([new Uint8Array(20 * MIB)], 'backup.lyra');
    mockedApi.mockImplementation(async (path: string) => {
      if (path === '/backup/import/uploads') {
        return { upload_id: 'u-1', chunk_size: 8 * MIB } as never;
      }
      if (path.endsWith('/finish')) return { job_id: 'j-9' } as never;
      return undefined as never;
    });
    const progress: number[] = [];

    const res = await uploadBackup(file, 'pw-123456', (pct) => progress.push(pct));

    expect(res).toEqual({ jobId: 'j-9' });
    const puts = mockedApi.mock.calls.filter(([p]) => String(p).includes('/chunks/'));
    expect(puts.map(([p]) => p)).toEqual([
      '/backup/import/uploads/u-1/chunks/0',
      '/backup/import/uploads/u-1/chunks/1',
      '/backup/import/uploads/u-1/chunks/2',
    ]);
    const sizes = puts.map(([, init]) => (init?.body as Blob | undefined)?.size);
    expect(sizes).toEqual([8 * MIB, 8 * MIB, 4 * MIB]);
    for (const [, init] of puts) {
      expect(init?.method).toBe('PUT');
      expect(new Headers(init?.headers).get('Content-Type')).toBe('application/octet-stream');
    }
    expect(progress).toEqual([33, 67, 100]);
    expect(mockedApi).toHaveBeenLastCalledWith('/backup/import/uploads/u-1/finish', {
      method: 'POST',
      body: JSON.stringify({ password: 'pw-123456', total_chunks: 3 }),
    });
  });

  it('surfaces a finish 400 with the backend code intact', async () => {
    const file = new File([new Uint8Array(8 * MIB)], 'backup.lyra');
    mockedApi.mockImplementation(async (path: string) => {
      if (path === '/backup/import/uploads') {
        return { upload_id: 'u-1', chunk_size: 8 * MIB } as never;
      }
      if (path.endsWith('/finish')) {
        throw new ApiError(
          400,
          'http',
          'upload incomplete; missing chunks: [0]',
          'upload_incomplete',
        );
      }
      return undefined as never;
    });

    const err = await uploadBackup(file, 'pw-123456').catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).serverCode).toBe('upload_incomplete');
  });
});

describe('report type guards', () => {
  it('distinguishes export, import and failure reports', () => {
    const exportReport: BackupReport = {
      ok: true,
      artifact_id: 'a-1',
      sections: { settings: true, accounts: 1, messages: 2, contacts: 0, calendars: 0, blobs: 2 },
      warnings: [],
    };
    const importReport: BackupReport = {
      ok: true,
      report: {
        settings: true,
        accounts: { inserted: 1, skipped: 0, repaired: 0, failed: 0 },
        folders: { inserted: 2, skipped: 0, repaired: 0, failed: 0 },
        messages: { inserted: 3, skipped: 1, repaired: 0, failed: 0 },
        contacts: { inserted: 0, skipped: 0, repaired: 0, failed: 0 },
        calendars: { inserted: 0, skipped: 0, repaired: 0, failed: 0 },
        errors: [],
      },
    };
    const failure: BackupReport = { ok: false, error: 'invalid password' };

    expect(isExportReport(exportReport)).toBe(true);
    expect(isExportReport(importReport)).toBe(false);
    expect(isExportReport(failure)).toBe(false);
    expect(isImportReport(importReport)).toBe(true);
    expect(isImportReport(exportReport)).toBe(false);
    expect(isImportReport(failure)).toBe(false);
  });
});
