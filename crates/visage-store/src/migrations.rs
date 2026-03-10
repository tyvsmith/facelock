use visage_core::error::{VisageError, Result};

pub fn run_migrations(conn: &rusqlite::Connection) -> Result<()> {
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
    .map_err(|e| VisageError::Storage(e.to_string()))?;

    // Check current schema version
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .map_err(|e| VisageError::Storage(e.to_string()))?;

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
        .map_err(|e| VisageError::Storage(e.to_string()))?;
    }

    Ok(())
}
