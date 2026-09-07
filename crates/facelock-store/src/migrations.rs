use std::path::Path;

use crate::error::{Result, StoreError};

/// Any failure while bringing the schema forward is a
/// [`StoreError::Migration`]: whatever the underlying SQLite code, the
/// database is on a schema this build cannot use, and no caller policy
/// distinguishes further.
fn migration_err(path: &Path, e: rusqlite::Error) -> StoreError {
    StoreError::Migration {
        path: path.to_path_buf(),
        detail: e.to_string(),
    }
}

/// Bring `conn`'s schema forward to the current version.
///
/// `path` is passed in rather than recovered from `conn.path()` because both
/// callers already hold it, and the recovered form has no answer for an
/// in-memory connection — it would have to invent one.
pub(crate) fn run_migrations(path: &Path, conn: &rusqlite::Connection) -> Result<()> {
    // V1: initial schema
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS face_models (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user TEXT NOT NULL,
            label TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            UNIQUE(user, label)
        );
        CREATE INDEX IF NOT EXISTS idx_face_models_user ON face_models(user);
    ",
    )
    .map_err(|e| migration_err(path, e))?;

    // Check current schema version
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .map_err(|e| migration_err(path, e))?;

    if version < 2 {
        // V2: face_embeddings allows multiple embeddings per model (no UNIQUE on model_id)
        // Drop old table if it had the UNIQUE constraint
        conn.execute_batch(
            "
            DROP TABLE IF EXISTS face_embeddings;
            CREATE TABLE face_embeddings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
                embedding BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_face_embeddings_model ON face_embeddings(model_id);
            INSERT OR REPLACE INTO schema_version (version) VALUES (2);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    if version < 3 {
        // V3: add sealed flag to face_embeddings for TPM integration
        conn.execute_batch(
            "
            ALTER TABLE face_embeddings ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0;
            INSERT OR REPLACE INTO schema_version (version) VALUES (3);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    if version < 4 {
        // V4: rate_limit table for persistent (SQLite-based) rate limiting.
        // Used by the oneshot auth path where in-memory rate limiting is not
        // possible (fresh process each invocation).
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS rate_limit (
                user TEXT NOT NULL,
                attempt_time INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rate_limit_user ON rate_limit(user);
            INSERT OR REPLACE INTO schema_version (version) VALUES (4);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    if version < 5 {
        // V5: track which embedder model generated each enrollment's embeddings.
        // Switching embedder models invalidates enrolled faces.
        conn.execute_batch(
            "
            ALTER TABLE face_models ADD COLUMN embedder_model TEXT NOT NULL DEFAULT '';
            INSERT OR REPLACE INTO schema_version (version) VALUES (5);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    if version < 6 {
        // V6: couple each template to the camera that enrolled it (Plan 02).
        // Nullable — legacy rows keep NULL and are governed by the
        // `bind_legacy_templates` policy so upgrades never lock anyone out.
        // Additive column, mirroring the V5 pattern exactly.
        conn.execute_batch(
            "
            ALTER TABLE face_models ADD COLUMN device_id TEXT;
            INSERT OR REPLACE INTO schema_version (version) VALUES (6);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    if version < 7 {
        // V7: record which key sealed each model (#354). Nullable — NULL means
        // a pre-V7 row or a plaintext (unencrypted) row, neither of which was
        // ever tagged with a key identity. Additive column, mirroring the V6
        // pattern exactly.
        conn.execute_batch(
            "
            ALTER TABLE face_models ADD COLUMN key_id TEXT;
            INSERT OR REPLACE INTO schema_version (version) VALUES (7);
        ",
        )
        .map_err(|e| migration_err(path, e))?;
    }

    Ok(())
}
