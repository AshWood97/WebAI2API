/**
 * Persistent media job/file registry.
 *
 * Media is intentionally kept outside SQLite. SQLite only stores metadata and
 * relative paths, so a large video can never inflate the database or API RAM.
 */

import crypto from 'crypto';
import fs from 'fs';
import path from 'path';
import Database from 'better-sqlite3';

const TERMINAL_STATUSES = new Set(['completed', 'failed', 'cancelled']);

function now() {
    return Math.floor(Date.now() / 1000);
}

function parseJson(value, fallback) {
    if (!value) return fallback;
    try {
        return JSON.parse(value);
    } catch {
        return fallback;
    }
}

function jobId(kind) {
    return `${kind}_${crypto.randomUUID().replace(/-/g, '')}`;
}

/**
 * SQLite-backed registry for async video/audio/image jobs and uploaded files.
 */
export class JobStore {
    constructor(dataDir) {
        this.dataDir = dataDir;
        this.mediaDir = path.join(dataDir, 'media');
        this.uploadDir = path.join(this.mediaDir, 'uploads');
        this.outputDir = path.join(this.mediaDir, 'outputs');
        fs.mkdirSync(this.uploadDir, { recursive: true, mode: 0o700 });
        fs.mkdirSync(this.outputDir, { recursive: true, mode: 0o700 });

        this.db = new Database(path.join(dataDir, 'media-jobs.sqlite3'));
        this.db.pragma('journal_mode = WAL');
        this.db.pragma('foreign_keys = ON');
        this._init();
    }

    _init() {
        this.db.exec(`
            CREATE TABLE IF NOT EXISTS media_schema (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS media_jobs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN ('video', 'audio', 'image')),
                model TEXT NOT NULL,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('queued', 'in_progress', 'completed', 'failed', 'cancelled')),
                progress INTEGER NOT NULL DEFAULT 0 CHECK(progress >= 0 AND progress <= 100),
                provider TEXT,
                worker_name TEXT,
                provider_task_id TEXT,
                conversation_id TEXT,
                options_json TEXT NOT NULL DEFAULT '{}',
                input_file_ids_json TEXT NOT NULL DEFAULT '[]',
                output_json TEXT NOT NULL DEFAULT '[]',
                error TEXT,
                message TEXT,
                idempotency_key TEXT,
                idempotency_scope TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                cancelled_at INTEGER
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_media_jobs_idempotency
                ON media_jobs(idempotency_scope, idempotency_key)
                WHERE idempotency_key IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_media_jobs_status ON media_jobs(status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_media_jobs_expires ON media_jobs(expires_at);

            CREATE TABLE IF NOT EXISTS media_files (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                purpose TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                relative_path TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_media_files_expires ON media_files(expires_at);
        `);
        this.db.prepare('INSERT OR IGNORE INTO media_schema(version, applied_at) VALUES(1, ?)').run(now());
    }

    _hydrateJob(row) {
        if (!row) return null;
        return {
            ...row,
            options: parseJson(row.options_json, {}),
            input_file_ids: parseJson(row.input_file_ids_json, []),
            outputs: parseJson(row.output_json, [])
        };
    }

    createJob({ kind, model, prompt, options = {}, inputFileIds = [], idempotencyKey, scope = 'default', retentionSeconds }) {
        if (idempotencyKey) {
            const existing = this.db.prepare(
                'SELECT * FROM media_jobs WHERE idempotency_scope = ? AND idempotency_key = ?'
            ).get(scope, idempotencyKey);
            if (existing) return { job: this._hydrateJob(existing), reused: true };
        }

        const createdAt = now();
        const id = jobId(kind);
        const expiresAt = createdAt + retentionSeconds;
        this.db.prepare(`
            INSERT INTO media_jobs (
                id, kind, model, prompt, status, options_json, input_file_ids_json,
                idempotency_key, idempotency_scope, created_at, updated_at, expires_at
            ) VALUES (?, ?, ?, ?, 'queued', ?, ?, ?, ?, ?, ?, ?)
        `).run(
            id,
            kind,
            model,
            prompt,
            JSON.stringify(options),
            JSON.stringify(inputFileIds),
            idempotencyKey || null,
            scope,
            createdAt,
            createdAt,
            expiresAt
        );
        return { job: this.getJob(id), reused: false };
    }

    getJob(id) {
        return this._hydrateJob(this.db.prepare('SELECT * FROM media_jobs WHERE id = ?').get(id));
    }

    updateJob(id, changes = {}) {
        const fields = {
            status: changes.status,
            progress: changes.progress,
            provider: changes.provider,
            worker_name: changes.workerName,
            provider_task_id: changes.providerTaskId,
            conversation_id: changes.conversationId,
            options_json: changes.options === undefined ? undefined : JSON.stringify(changes.options),
            output_json: changes.outputs === undefined ? undefined : JSON.stringify(changes.outputs),
            error: changes.error,
            message: changes.message,
            cancelled_at: changes.cancelledAt
        };
        const assignments = [];
        const values = [];
        for (const [field, value] of Object.entries(fields)) {
            if (value === undefined) continue;
            assignments.push(`${field} = ?`);
            values.push(value);
        }
        if (assignments.length === 0) return this.getJob(id);
        assignments.push('updated_at = ?');
        values.push(now());
        values.push(id);
        this.db.prepare(`UPDATE media_jobs SET ${assignments.join(', ')} WHERE id = ?`).run(...values);
        return this.getJob(id);
    }

    markCancelled(id) {
        return this.updateJob(id, {
            status: 'cancelled',
            progress: 100,
            cancelledAt: now(),
            message: 'Cancelled by caller'
        });
    }

    listRecoverableJobs() {
        return this.db.prepare(
            "SELECT * FROM media_jobs WHERE status IN ('queued', 'in_progress') ORDER BY created_at ASC"
        ).all().map(row => this._hydrateJob(row));
    }

    createFile({ filename, purpose = 'user_data', mimeType, sizeBytes, relativePath, retentionSeconds }) {
        const createdAt = now();
        const id = `file_${crypto.randomUUID().replace(/-/g, '')}`;
        this.db.prepare(`
            INSERT INTO media_files (id, filename, purpose, mime_type, size_bytes, relative_path, created_at, expires_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        `).run(id, filename, purpose, mimeType, sizeBytes, relativePath, createdAt, createdAt + retentionSeconds);
        return this.getFile(id);
    }

    getFile(id) {
        const row = this.db.prepare('SELECT * FROM media_files WHERE id = ?').get(id);
        return row ? { ...row, absolute_path: path.join(this.mediaDir, row.relative_path) } : null;
    }

    listFiles(ids) {
        if (!Array.isArray(ids) || ids.length === 0) return [];
        const placeholders = ids.map(() => '?').join(', ');
        return this.db.prepare(`SELECT * FROM media_files WHERE id IN (${placeholders})`).all(...ids)
            .map(row => ({ ...row, absolute_path: path.join(this.mediaDir, row.relative_path) }));
    }

    /**
     * Delete expired terminal-job output and expired uploads. Returns paths that
     * the caller may unlink. In-progress jobs are never removed.
     */
    collectExpiredPaths() {
        const timestamp = now();
        const paths = [];
        const jobs = this.db.prepare(`
            SELECT id, output_json FROM media_jobs
            WHERE expires_at < ? AND status IN ('completed', 'failed', 'cancelled')
        `).all(timestamp);
        for (const job of jobs) {
            for (const output of parseJson(job.output_json, [])) {
                if (output.relative_path) paths.push(path.join(this.mediaDir, output.relative_path));
            }
        }
        this.db.prepare(`
            DELETE FROM media_jobs
            WHERE expires_at < ? AND status IN ('completed', 'failed', 'cancelled')
        `).run(timestamp);

        const files = this.db.prepare('SELECT relative_path FROM media_files WHERE expires_at < ?').all(timestamp);
        for (const file of files) paths.push(path.join(this.mediaDir, file.relative_path));
        this.db.prepare('DELETE FROM media_files WHERE expires_at < ?').run(timestamp);
        return paths;
    }

    getStoredBytes() {
        const walk = dir => {
            let total = 0;
            for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
                const entryPath = path.join(dir, entry.name);
                if (entry.isDirectory()) total += walk(entryPath);
                else if (entry.isFile()) total += fs.statSync(entryPath).size;
            }
            return total;
        };
        return walk(this.mediaDir);
    }

    isTerminal(job) {
        return TERMINAL_STATUSES.has(job?.status);
    }

    close() {
        this.db.close();
    }
}
