use howdy_core::error::{HowdyError, Result};

pub fn run_migrations(conn: &rusqlite::Connection) -> Result<()> {
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
        CREATE TABLE IF NOT EXISTS face_embeddings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
            embedding BLOB NOT NULL,
            UNIQUE(model_id)
        );
        CREATE INDEX IF NOT EXISTS idx_face_models_user ON face_models(user);
        CREATE INDEX IF NOT EXISTS idx_face_embeddings_model ON face_embeddings(model_id);
    ",
    )
    .map_err(|e| HowdyError::Storage(e.to_string()))
}
