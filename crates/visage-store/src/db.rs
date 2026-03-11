use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use visage_core::error::{VisageError, Result};
use visage_core::types::{FaceEmbedding, FaceModelInfo};

use crate::migrations::run_migrations;

pub struct FaceStore {
    conn: rusqlite::Connection,
}

fn map_err(e: rusqlite::Error) -> VisageError {
    VisageError::Storage(e.to_string())
}

impl FaceStore {
    /// Open database at the given path, enable WAL mode and foreign keys, run migrations.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(db_path).map_err(map_err)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(map_err)?;
        run_migrations(&conn)?;
        Ok(Self { conn })
    }

    /// Open database in read-only mode for authentication queries.
    /// Does not enable WAL or run migrations (avoids needing write access).
    pub fn open_readonly(db_path: &Path) -> Result<Self> {
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(map_err)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;").map_err(map_err)?;
        Ok(Self { conn })
    }

    /// Open an in-memory database for testing.
    pub fn open_memory() -> Result<Self> {
        let conn = rusqlite::Connection::open_in_memory().map_err(map_err)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(map_err)?;
        run_migrations(&conn)?;
        Ok(Self { conn })
    }

    /// Add a face model with its embedding. Returns the new model ID.
    pub fn add_model(&self, user: &str, label: &str, embedding: &FaceEmbedding) -> Result<u32> {
        let tx = self.conn.unchecked_transaction().map_err(map_err)?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        tx.execute(
            "INSERT INTO face_models (user, label, created_at) VALUES (?1, ?2, ?3)",
            params![user, label, created_at],
        )
        .map_err(map_err)?;

        let model_id = tx.last_insert_rowid() as u32;

        let bytes: &[u8] = bytemuck::cast_slice(embedding.as_slice());
        tx.execute(
            "INSERT INTO face_embeddings (model_id, embedding) VALUES (?1, ?2)",
            params![model_id, bytes],
        )
        .map_err(map_err)?;

        tx.commit().map_err(map_err)?;
        Ok(model_id)
    }

    /// Add an embedding to an existing model. Used during enrollment to store
    /// multiple embeddings (from different angles) under a single model.
    pub fn add_embedding(&self, model_id: u32, embedding: &FaceEmbedding) -> Result<()> {
        let bytes: &[u8] = bytemuck::cast_slice(embedding.as_slice());
        self.conn
            .execute(
                "INSERT INTO face_embeddings (model_id, embedding) VALUES (?1, ?2)",
                params![model_id, bytes],
            )
            .map_err(map_err)?;
        Ok(())
    }

    /// Remove any existing model with the given user+label, if present.
    /// Returns true if a model was removed.
    pub fn remove_model_by_label(&self, user: &str, label: &str) -> Result<bool> {
        let affected = self
            .conn
            .execute(
                "DELETE FROM face_models WHERE user = ?1 AND label = ?2",
                params![user, label],
            )
            .map_err(map_err)?;
        Ok(affected > 0)
    }

    /// Get all embeddings for a user as (model_id, embedding) pairs.
    pub fn get_user_embeddings(&self, user: &str) -> Result<Vec<(u32, FaceEmbedding)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fm.id, fe.embedding
                 FROM face_models fm
                 JOIN face_embeddings fe ON fe.model_id = fm.id
                 WHERE fm.user = ?1",
            )
            .map_err(map_err)?;

        let rows = stmt
            .query_map(params![user], |row| {
                let id: u32 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .map_err(map_err)?;

        let mut results = Vec::new();
        for row in rows {
            let (id, blob) = row.map_err(map_err)?;
            let floats: &[f32] = bytemuck::cast_slice(&blob);
            let mut embedding = [0f32; 512];
            embedding.copy_from_slice(floats);
            results.push((id, embedding));
        }
        Ok(results)
    }

    /// List face models for a user (metadata only, no embeddings).
    pub fn list_models(&self, user: &str) -> Result<Vec<FaceModelInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, user, label, created_at FROM face_models WHERE user = ?1")
            .map_err(map_err)?;

        let rows = stmt
            .query_map(params![user], |row| {
                Ok(FaceModelInfo {
                    id: row.get(0)?,
                    user: row.get(1)?,
                    label: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(map_err)?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(map_err)?);
        }
        Ok(results)
    }

    /// Remove a specific model by ID (only if owned by the given user).
    /// Returns true if a row was deleted, false if not found.
    pub fn remove_model(&self, user: &str, model_id: u32) -> Result<bool> {
        let affected = self
            .conn
            .execute(
                "DELETE FROM face_models WHERE id = ?1 AND user = ?2",
                params![model_id, user],
            )
            .map_err(map_err)?;
        Ok(affected > 0)
    }

    /// Remove all models for a user. Returns the number of models removed.
    pub fn clear_user(&self, user: &str) -> Result<u32> {
        let affected = self
            .conn
            .execute("DELETE FROM face_models WHERE user = ?1", params![user])
            .map_err(map_err)?;
        Ok(affected as u32)
    }

    /// Check if a user has any stored models.
    pub fn has_models(&self, user: &str) -> Result<bool> {
        let count: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_models WHERE user = ?1",
                params![user],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_embedding() -> FaceEmbedding {
        let mut e = [0.0f32; 512];
        for (i, v) in e.iter_mut().enumerate() {
            *v = i as f32 / 512.0;
        }
        e
    }

    #[test]
    fn test_add_and_retrieve() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let id = store.add_model("alice", "front", &emb).unwrap();
        let results = store.get_user_embeddings("alice").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, id);
        for i in 0..512 {
            assert_eq!(results[0].1[i], emb[i], "mismatch at index {i}");
        }
    }

    #[test]
    fn test_duplicate_label() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb).unwrap();
        let result = store.add_model("alice", "front", &emb);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_models() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb).unwrap();
        store.add_model("alice", "side", &emb).unwrap();
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].user, "alice");
        assert_eq!(models[1].user, "alice");
        let labels: Vec<&str> = models.iter().map(|m| m.label.as_str()).collect();
        assert!(labels.contains(&"front"));
        assert!(labels.contains(&"side"));
    }

    #[test]
    fn test_remove_model() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let id = store.add_model("alice", "front", &emb).unwrap();
        assert!(store.remove_model("alice", id).unwrap());
        let models = store.list_models("alice").unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn test_clear_user() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "a", &emb).unwrap();
        store.add_model("alice", "b", &emb).unwrap();
        store.add_model("alice", "c", &emb).unwrap();
        let count = store.clear_user("alice").unwrap();
        assert_eq!(count, 3);
        assert!(!store.has_models("alice").unwrap());
    }

    #[test]
    fn test_multi_user() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb).unwrap();
        store.add_model("bob", "front", &emb).unwrap();

        let alice_models = store.list_models("alice").unwrap();
        let bob_models = store.list_models("bob").unwrap();
        assert_eq!(alice_models.len(), 1);
        assert_eq!(bob_models.len(), 1);

        store.clear_user("alice").unwrap();
        assert!(!store.has_models("alice").unwrap());
        assert!(store.has_models("bob").unwrap());
    }

    #[test]
    fn test_has_models() {
        let store = FaceStore::open_memory().unwrap();
        assert!(!store.has_models("alice").unwrap());
        let emb = test_embedding();
        store.add_model("alice", "front", &emb).unwrap();
        assert!(store.has_models("alice").unwrap());
        store.clear_user("alice").unwrap();
        assert!(!store.has_models("alice").unwrap());
    }

    #[test]
    fn test_empty_store() {
        let store = FaceStore::open_memory().unwrap();
        assert!(store.list_models("alice").unwrap().is_empty());
        assert!(!store.has_models("alice").unwrap());
        assert!(store.get_user_embeddings("alice").unwrap().is_empty());
    }

    #[test]
    fn test_embedding_round_trip() {
        let store = FaceStore::open_memory().unwrap();
        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[1] = -1.0;
        emb[2] = std::f32::consts::PI;
        emb[3] = f32::MIN_POSITIVE;
        emb[511] = 42.0;

        store.add_model("alice", "test", &emb).unwrap();
        let results = store.get_user_embeddings("alice").unwrap();
        assert_eq!(results.len(), 1);
        for i in 0..512 {
            assert_eq!(
                results[0].1[i].to_bits(),
                emb[i].to_bits(),
                "bit-exact mismatch at index {i}"
            );
        }
    }

    #[test]
    fn test_add_embedding_to_model() {
        let store = FaceStore::open_memory().unwrap();
        let emb1 = test_embedding();
        let mut emb2 = test_embedding();
        emb2[0] = 99.0;

        let id = store.add_model("alice", "front", &emb1).unwrap();
        store.add_embedding(id, &emb2).unwrap();

        let results = store.get_user_embeddings("alice").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].1[0], emb1[0]);
        assert_eq!(results[1].1[0], emb2[0]);
    }

    #[test]
    fn test_remove_model_by_label() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb).unwrap();
        assert!(store.remove_model_by_label("alice", "front").unwrap());
        assert!(!store.has_models("alice").unwrap());
        // Removing again returns false
        assert!(!store.remove_model_by_label("alice", "front").unwrap());
    }

    #[test]
    fn test_remove_nonexistent() {
        let store = FaceStore::open_memory().unwrap();
        assert!(!store.remove_model("alice", 9999).unwrap());
    }

    #[test]
    fn test_cascade_delete() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let id = store.add_model("alice", "front", &emb).unwrap();

        // Verify embedding exists
        let embs = store.get_user_embeddings("alice").unwrap();
        assert_eq!(embs.len(), 1);

        // Remove model — cascade should delete embedding
        store.remove_model("alice", id).unwrap();

        // Verify embedding is also gone
        let embs = store.get_user_embeddings("alice").unwrap();
        assert!(embs.is_empty());

        // Also verify directly in face_embeddings table
        let count: u32 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_embeddings WHERE model_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
