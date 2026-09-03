use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;

use facelock_core::fs_security::ensure_mode;
use facelock_core::types::{FaceEmbedding, FaceModelInfo, Wiped};

use crate::error::{Result, StoreError};
use crate::migrations::run_migrations;

/// The fewest embeddings a stored model may hold.
///
/// Owned by the store because it is a row invariant, not a capture policy:
/// [`FaceStore::replace_model_with_embeddings`] refuses a shorter set, so a
/// model that exists always has enough embeddings to be compared against
/// from more than one angle (#308). The daemon's enrollment loop derives its
/// minimum-capture count from this constant, so the two cannot drift.
pub const MIN_EMBEDDINGS_PER_MODEL: usize = 3;

/// How long an operation waits for another connection's write lock before
/// giving up with [`StoreError::Busy`].
///
/// Facelock's writers genuinely exclude each other: an enrollment commits its
/// model in one transaction, and the encryption-key gate holds an exclusive
/// section across its row check and its key write
/// ([`FaceStore::with_exclusive`]). Both are short — one small query and a
/// 32-byte file — so the alternative to waiting is failing an enrollment
/// because a key check happened to be in flight. Five seconds is far longer
/// than either section and far shorter than any caller's own deadline.
///
/// Set explicitly even though rusqlite happens to apply the same value to
/// every connection it opens: that default is undocumented, and whether a
/// concurrent enrollment waits or fails is not a property to leave resting on
/// it.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct FaceStore {
    conn: rusqlite::Connection,
    /// Where this store lives, kept so post-open failures can name the
    /// database they failed on. `:memory:` for [`FaceStore::open_memory`].
    path: PathBuf,
}

impl FaceStore {
    /// Open an **existing** database read-write; never creates one.
    ///
    /// This is the constructor almost every caller wants. `SQLITE_OPEN_CREATE`
    /// is deliberately absent, because creating on open is not a harmless
    /// convenience here: a command that merely *reads* (list, remove, clear,
    /// status) pointed at a typo'd or wrong path would materialise an empty
    /// database there and then report "nothing enrolled" — a silent lie about
    /// a store of biometric templates. A missing database must be an error the
    /// caller sees.
    ///
    /// The error says why: [`StoreError::Absent`] for a missing file (a
    /// fresh install a caller may legitimately proceed on),
    /// [`StoreError::Denied`]/[`StoreError::Corrupt`]/[`StoreError::Busy`]
    /// when the file is there but cannot be used — cases that must never be
    /// read as "nothing enrolled".
    ///
    /// **Migrations do run.** Not creating and not migrating are separate
    /// concerns: a database that is *present* but on an older schema still has
    /// to be brought forward, or every query touching a newer column fails.
    /// The connection is read-write, so migrating is exactly as safe as it is
    /// under [`FaceStore::create`].
    pub fn open_existing(db_path: &Path) -> Result<Self> {
        // The flags below are what actually guarantee no file is created; this
        // stat exists to classify the common failures precisely. ENOENT is the
        // only evidence for `Absent`: a stat the filesystem *refuses* (e.g. an
        // unsearchable parent directory) is `Denied`, because a bare
        // `is_file()` there would report a possibly-present database as
        // absent — which a destructive guard would read as "nothing to
        // protect".
        Self::stat_existing(db_path)?;

        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            // No CREATE, and no URI handling either: a path is a path, never a
            // `file:` URI with query parameters.
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StoreError::classify(db_path, e))?;

        Self::init(conn, db_path)
    }

    /// Classify what is at `db_path` before a no-create open: absent, a
    /// non-file, or unstatable. `Ok(())` means a regular file is present.
    fn stat_existing(db_path: &Path) -> Result<()> {
        match std::fs::metadata(db_path) {
            Ok(m) if m.is_file() => Ok(()),
            Ok(_) => Err(StoreError::Corrupt {
                path: db_path.to_path_buf(),
                detail: "path exists but is not a regular file".into(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::Absent {
                path: db_path.to_path_buf(),
            }),
            Err(e) => Err(StoreError::Denied {
                path: db_path.to_path_buf(),
                detail: format!("cannot stat: {e}"),
            }),
        }
    }

    /// Create the database if absent, then open it read-write.
    ///
    /// Only enrollment and setup legitimately need this: they are the flows
    /// that are *supposed* to bring a database into existence. Everything else
    /// should use [`FaceStore::open_existing`] — see its docs for why creating
    /// as a side effect of reading strands a user's templates.
    pub fn create(db_path: &Path) -> Result<Self> {
        // `Connection::open` implies SQLITE_OPEN_CREATE, so `Absent` cannot
        // come back from this constructor — an uncreatable path (missing or
        // unwritable parent) surfaces as `Denied`.
        let conn =
            rusqlite::Connection::open(db_path).map_err(|e| StoreError::classify(db_path, e))?;
        Self::init(conn, db_path)
    }

    /// Create-or-open, as this constructor has always behaved.
    ///
    /// Kept working for callers that have not yet been split, but every use is
    /// a decision that was never made: pick [`FaceStore::open_existing`] if the
    /// database is expected to be there, [`FaceStore::create`] if this flow is
    /// the one that brings it into being.
    #[deprecated(
        note = "ambiguous: use FaceStore::open_existing to read an existing database, or FaceStore::create when this flow is meant to create one"
    )]
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::create(db_path)
    }

    /// Whether a database file is present at this path.
    ///
    /// A cheap `stat`, no connection opened and no schema inspected. Note it
    /// cannot distinguish "absent" from "unstatable": for any decision that
    /// treats those differently, call [`FaceStore::open_existing`] and match
    /// on [`StoreError::Absent`] vs [`StoreError::Denied`] instead.
    pub fn database_exists(db_path: &Path) -> bool {
        db_path.is_file()
    }

    /// Shared tail of [`FaceStore::open_existing`] and [`FaceStore::create`]:
    /// WAL, foreign keys, migrations, restrictive file modes. Only the flags
    /// used to obtain `conn` differ between them.
    fn init(conn: rusqlite::Connection, db_path: &Path) -> Result<Self> {
        // First, not rusqlite's default: migrations below can contend with a
        // concurrent writer, and should wait on the timeout this crate
        // chose rather than whatever rusqlite starts a connection with.
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| StoreError::classify(db_path, e))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| StoreError::classify(db_path, e))?;
        run_migrations(db_path, &conn)?;
        secure_database_files(db_path)?;
        Ok(Self {
            conn,
            path: db_path.to_path_buf(),
        })
    }

    /// Open database in read-only mode for authentication queries.
    /// Does not enable WAL or run migrations (avoids needing write access).
    ///
    /// Like [`FaceStore::open_existing`], this omits `SQLITE_OPEN_CREATE` and
    /// so never brings a database into existence; it differs in giving up write
    /// access entirely, which is why it also cannot migrate.
    pub fn open_readonly(db_path: &Path) -> Result<Self> {
        // Same stat classification as `open_existing`: a missing file is
        // `Absent`, not an undifferentiated open failure.
        Self::stat_existing(db_path)?;
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StoreError::classify(db_path, e))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| StoreError::classify(db_path, e))?;
        Ok(Self {
            conn,
            path: db_path.to_path_buf(),
        })
    }

    /// Open an in-memory database for testing.
    pub fn open_memory() -> Result<Self> {
        let path = Path::new(":memory:");
        let conn =
            rusqlite::Connection::open_in_memory().map_err(|e| StoreError::classify(path, e))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| StoreError::classify(path, e))?;
        run_migrations(path, &conn)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Classify a rusqlite failure from this store's connection.
    fn err(&self, e: rusqlite::Error) -> StoreError {
        StoreError::classify(&self.path, e)
    }

    /// Begin a write transaction that takes the database's write lock now,
    /// not at its first write statement.
    ///
    /// `BEGIN IMMEDIATE`, never `BEGIN DEFERRED`. A deferred transaction that
    /// reads before it writes has to *promote* a read transaction to a write
    /// one, and SQLite will not run the busy handler for that — the two
    /// waiters could deadlock — so it fails with `SQLITE_BUSY` at once
    /// however long [`BUSY_TIMEOUT`] is. Today's transactions all write with
    /// their first statement and so would wait anyway; taking the lock at
    /// `BEGIN` costs them nothing and keeps a later read-then-write edit from
    /// quietly turning a wait into a failed enrollment.
    fn write_tx(&self) -> Result<rusqlite::Transaction<'_>> {
        rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| self.err(e))
    }

    /// Run `f` with this connection holding the database's write lock, and
    /// release it only once `f` has returned.
    ///
    /// `BEGIN EXCLUSIVE`, so the lock is taken before `f` runs rather than at
    /// its first write: `f` may only read and still excludes every writer.
    /// That is what the encryption-key gate needs. It asks whether any stored
    /// blob is encrypted and then writes the key artifact, and those two steps
    /// have to be one indivisible act — under WAL a plain read sees the
    /// snapshot it opened on, so an enrollment committing a sealed row between
    /// the question and the answer produces a row the new key cannot read
    /// (#231). Enrollment's own write is one transaction
    /// ([`Self::replace_model_with_embeddings`]), so the two serialize.
    ///
    /// `f` is handed this store's own connection, so a query issued through
    /// `&self` from inside the closure runs inside the same transaction — but
    /// a method that opens a transaction of its own (every write on this type)
    /// cannot: SQLite has no nested transactions, and such a call fails rather
    /// than silently escaping the section.
    ///
    /// Returning `Err` — or unwinding — rolls the section back. The section
    /// holds no writes of its own, so rollback and commit differ only in name.
    pub fn with_exclusive<T, E>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StoreError>,
    {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Exclusive,
        )
        .map_err(|e| E::from(self.err(e)))?;
        let value = f(&tx)?;
        tx.commit().map_err(|e| E::from(self.err(e)))?;
        Ok(value)
    }

    /// Add a face model with its embedding. Returns the new model ID.
    /// Stores a NULL `device_id` (not coupled to any camera); use
    /// [`FaceStore::add_model_with_device`] to bind the enrolling camera.
    pub fn add_model(
        &self,
        user: &str,
        label: &str,
        embedding: &FaceEmbedding,
        embedder_model: &str,
    ) -> Result<u32> {
        self.add_model_with_device(user, label, embedding, embedder_model, None)
    }

    /// Insert the `face_models` row for a new model inside `tx` and return
    /// its ID. Every model-creating write goes through here, so the row shape
    /// and its timestamp are defined once.
    fn insert_model_row(
        &self,
        tx: &rusqlite::Transaction<'_>,
        user: &str,
        label: &str,
        embedder_model: &str,
        device_id: Option<&str>,
    ) -> Result<u32> {
        // Stored as INTEGER (i64) in SQLite. Cast keeps the code portable
        // across rusqlite versions (0.39+ no longer impls ToSql/FromSql for u64
        // because SQLite INTEGER is signed 64-bit).
        let created_at: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        tx.execute(
            "INSERT INTO face_models (user, label, created_at, embedder_model, device_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![user, label, created_at, embedder_model, device_id],
        )
        .map_err(|e| self.err(e))?;

        Ok(tx.last_insert_rowid() as u32)
    }

    /// Add a face model with its embedding and the enrolling camera's canonical
    /// device fingerprint (`Some("vid:pid:serial")`), or `None` when the camera
    /// exposes no readable USB identity. Returns the new model ID.
    pub fn add_model_with_device(
        &self,
        user: &str,
        label: &str,
        embedding: &FaceEmbedding,
        embedder_model: &str,
        device_id: Option<&str>,
    ) -> Result<u32> {
        let tx = self.write_tx()?;

        let model_id = self.insert_model_row(&tx, user, label, embedder_model, device_id)?;

        let bytes: &[u8] = bytemuck::cast_slice(embedding.as_slice());
        tx.execute(
            "INSERT INTO face_embeddings (model_id, embedding) VALUES (?1, ?2)",
            params![model_id, bytes],
        )
        .map_err(|e| self.err(e))?;

        tx.commit().map_err(|e| self.err(e))?;
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
            .map_err(|e| self.err(e))?;
        Ok(())
    }

    /// Add a raw embedding (possibly encrypted) to an existing model.
    /// Used during enrollment to store encrypted embeddings under a single model.
    pub fn add_embedding_raw(&self, model_id: u32, data: &[u8], sealed: bool) -> Result<()> {
        let sealed_int: i64 = if sealed { 1 } else { 0 };
        self.conn
            .execute(
                "INSERT INTO face_embeddings (model_id, embedding, sealed) VALUES (?1, ?2, ?3)",
                params![model_id, data, sealed_int],
            )
            .map_err(|e| self.err(e))?;
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
            .map_err(|e| self.err(e))?;
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
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map(params![user], |row| {
                let id: u32 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .map_err(|e| self.err(e))?;

        // Accumulated through [`Wiped`]'s borrowed form, the shape the
        // decrypt path uses (#293): a corrupt row mid-loop zeroizes the
        // embeddings already collected, and growth wipes the outgrown
        // allocation instead of letting `Vec`'s reallocation free the
        // plaintext live.
        let mut results = Vec::new();
        let mut guarded = Wiped::new(&mut results);
        for row in rows {
            let (id, blob) = row.map_err(|e| self.err(e))?;
            if blob.len() != 512 * 4 {
                // Wrong-size template data is a corrupt store, not a failed
                // query: the row exists but cannot mean what it must.
                return Err(StoreError::Corrupt {
                    path: self.path.clone(),
                    detail: format!(
                        "invalid embedding blob size: expected {} bytes, got {}",
                        512 * 4,
                        blob.len()
                    ),
                });
            }
            let floats: &[f32] = bytemuck::cast_slice(&blob);
            let mut embedding = [0f32; 512];
            embedding.copy_from_slice(floats);
            guarded.push((id, embedding));
        }
        // Every row collected: the caller takes the plaintext over.
        // Forgetting the guard skips its wipe without leaking — it owns only
        // the borrow.
        std::mem::forget(guarded);
        Ok(results)
    }

    /// List face models for a user (metadata only, no embeddings).
    pub fn list_models(&self, user: &str) -> Result<Vec<FaceModelInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, user, label, created_at, embedder_model, device_id FROM face_models WHERE user = ?1")
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map(params![user], |row| {
                Ok(FaceModelInfo {
                    id: row.get(0)?,
                    user: row.get(1)?,
                    label: row.get(2)?,
                    // Read as i64 and widen; rusqlite 0.39 dropped FromSql for u64.
                    created_at: row.get::<_, i64>(3)? as u64,
                    embedder_model: row.get(4)?,
                    // Nullable column (V6): NULL → None for legacy rows.
                    device_id: row.get::<_, Option<String>>(5)?,
                })
            })
            .map_err(|e| self.err(e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| self.err(e))?);
        }
        Ok(results)
    }

    /// List every distinct user that has at least one stored model.
    ///
    /// Used to rebuild per-user enrollment markers from the authoritative
    /// database (`facelock setup` reconcile).
    pub fn list_users(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT user FROM face_models ORDER BY user")
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| self.err(e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| self.err(e))?);
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
            .map_err(|e| self.err(e))?;
        Ok(affected > 0)
    }

    /// Remove all models for a user. Returns the number of models removed.
    pub fn clear_user(&self, user: &str) -> Result<u32> {
        let affected = self
            .conn
            .execute("DELETE FROM face_models WHERE user = ?1", params![user])
            .map_err(|e| self.err(e))?;
        Ok(affected as u32)
    }

    /// Get all embeddings for a user as raw bytes with sealed flag.
    /// Returns (model_id, raw_bytes, sealed) triples.
    /// Uses `fm.id` (face_models ID) — not `fe.id` — so the returned IDs
    /// are consistent with `get_user_embeddings` and can be looked up via
    /// `list_models`.
    pub fn get_user_embeddings_raw(&self, user: &str) -> Result<Vec<(u32, Vec<u8>, bool)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fm.id, fe.embedding, fe.sealed
                 FROM face_models fm
                 JOIN face_embeddings fe ON fe.model_id = fm.id
                 WHERE fm.user = ?1",
            )
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map(params![user], |row| {
                let id: u32 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                let sealed: bool = row.get::<_, i64>(2)? != 0;
                Ok((id, blob, sealed))
            })
            .map_err(|e| self.err(e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| self.err(e))?);
        }
        Ok(results)
    }

    /// Like [`FaceStore::get_user_embeddings_raw`], but also returns each row's
    /// enrolling-camera `device_id` (NULL for legacy/unidentified templates).
    ///
    /// Used by the decrypt path when opt-in hard device binding
    /// (`security.bind_device_aad`) is active: the AAD for each blob is derived
    /// from its own template's `device_id`.
    #[allow(clippy::type_complexity)]
    pub fn get_user_embeddings_raw_with_device(
        &self,
        user: &str,
    ) -> Result<Vec<(u32, Vec<u8>, bool, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fm.id, fe.embedding, fe.sealed, fm.device_id
                 FROM face_models fm
                 JOIN face_embeddings fe ON fe.model_id = fm.id
                 WHERE fm.user = ?1",
            )
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map(params![user], |row| {
                let id: u32 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                let sealed: bool = row.get::<_, i64>(2)? != 0;
                let device_id: Option<String> = row.get(3)?;
                Ok((id, blob, sealed, device_id))
            })
            .map_err(|e| self.err(e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| self.err(e))?);
        }
        Ok(results)
    }

    /// Add a face model with raw bytes and a sealed flag. Returns the new model ID.
    /// Stores a NULL `device_id`; use [`FaceStore::add_model_raw_with_device`] to
    /// bind the enrolling camera.
    pub fn add_model_raw(
        &self,
        user: &str,
        label: &str,
        data: &[u8],
        sealed: bool,
        embedder_model: &str,
    ) -> Result<u32> {
        self.add_model_raw_with_device(user, label, data, sealed, embedder_model, None)
    }

    /// Add a face model with raw (possibly encrypted) bytes, a sealed flag, and
    /// the enrolling camera's canonical device fingerprint. Returns the new
    /// model ID.
    pub fn add_model_raw_with_device(
        &self,
        user: &str,
        label: &str,
        data: &[u8],
        sealed: bool,
        embedder_model: &str,
        device_id: Option<&str>,
    ) -> Result<u32> {
        let tx = self.write_tx()?;

        let model_id = self.insert_model_row(&tx, user, label, embedder_model, device_id)?;
        let sealed_int: i64 = if sealed { 1 } else { 0 };

        tx.execute(
            "INSERT INTO face_embeddings (model_id, embedding, sealed) VALUES (?1, ?2, ?3)",
            params![model_id, data, sealed_int],
        )
        .map_err(|e| self.err(e))?;

        tx.commit().map_err(|e| self.err(e))?;
        Ok(model_id)
    }

    /// Replace whatever model `user` has under `label` with a new one holding
    /// every blob in `embeddings`, as one transaction. Returns the new model
    /// ID.
    ///
    /// This is enrollment's only write (#308): the template a later
    /// authentication can load either has all of its embeddings or does not
    /// exist. `UNIQUE(user, label)` is why the old model is deleted inside the
    /// same transaction rather than beforehand — a failure anywhere after the
    /// delete rolls the old model back into place, so a re-enrollment that
    /// fails keeps the template it was replacing.
    ///
    /// `sealed` applies to every blob: an enrollment is encrypted or plain,
    /// never mixed. Fewer than [`MIN_EMBEDDINGS_PER_MODEL`] blobs is refused
    /// with [`StoreError::Query`] before the transaction opens, whatever the
    /// caller's own gates said: a model that exists is a complete template.
    pub fn replace_model_with_embeddings(
        &self,
        user: &str,
        label: &str,
        embeddings: &[&[u8]],
        sealed: bool,
        embedder_model: &str,
        device_id: Option<&str>,
    ) -> Result<u32> {
        if embeddings.len() < MIN_EMBEDDINGS_PER_MODEL {
            return Err(StoreError::Query {
                path: self.path.clone(),
                detail: format!(
                    "refusing to store a model for {user}/{label} with {} embeddings (minimum {MIN_EMBEDDINGS_PER_MODEL})",
                    embeddings.len()
                ),
            });
        }

        let tx = self.write_tx()?;

        // Cascades to the old model's embeddings (`ON DELETE CASCADE`, with
        // foreign keys on for every connection this crate opens).
        tx.execute(
            "DELETE FROM face_models WHERE user = ?1 AND label = ?2",
            params![user, label],
        )
        .map_err(|e| self.err(e))?;

        let model_id = self.insert_model_row(&tx, user, label, embedder_model, device_id)?;
        let sealed_int: i64 = if sealed { 1 } else { 0 };

        for data in embeddings {
            tx.execute(
                "INSERT INTO face_embeddings (model_id, embedding, sealed) VALUES (?1, ?2, ?3)",
                params![model_id, data, sealed_int],
            )
            .map_err(|e| self.err(e))?;
        }

        tx.commit().map_err(|e| self.err(e))?;
        Ok(model_id)
    }

    /// Update an existing embedding's data and sealed flag in-place.
    pub fn update_embedding_sealed(
        &self,
        embedding_id: u32,
        data: &[u8],
        sealed: bool,
    ) -> Result<()> {
        let sealed_int: i64 = if sealed { 1 } else { 0 };
        let affected = self
            .conn
            .execute(
                "UPDATE face_embeddings SET embedding = ?1, sealed = ?2 WHERE id = ?3",
                params![data, sealed_int, embedding_id],
            )
            .map_err(|e| self.err(e))?;

        if affected == 0 {
            return Err(StoreError::Query {
                path: self.path.clone(),
                detail: format!("embedding ID {embedding_id} not found"),
            });
        }
        Ok(())
    }

    /// Count sealed vs unsealed embeddings across all users.
    /// Returns (sealed_count, unsealed_count).
    pub fn count_sealed(&self) -> Result<(u32, u32)> {
        let sealed: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_embeddings WHERE sealed != 0",
                [],
                |row| row.get(0),
            )
            .map_err(|e| self.err(e))?;
        let unsealed: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_embeddings WHERE sealed = 0",
                [],
                |row| row.get(0),
            )
            .map_err(|e| self.err(e))?;
        Ok((sealed, unsealed))
    }

    /// The version byte and byte length of every stored embedding blob.
    ///
    /// Returns one `(first_byte, len)` per row; `first_byte` is `None` for an
    /// empty blob.
    ///
    /// This exists so a caller can classify rows by *encryption shape* — how
    /// many templates are software-encrypted, how many TPM-sealed — without
    /// reading templates. [`Self::count_sealed`] cannot answer that question:
    /// the `sealed` column is one bit that every method sets, so a TPM system
    /// and a keyfile system look identical through it, and a row whose flag
    /// outlived its ciphertext looks encrypted when it is not. The blob's own
    /// version byte is the fact. Selecting whole rows to reach that byte would
    /// pull every plaintext biometric template into the caller's memory,
    /// outside the wiping discipline the read paths keep; SQLite does the
    /// projection instead.
    pub fn embedding_blob_shapes(&self) -> Result<Vec<(Option<u8>, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT substr(embedding, 1, 1), length(embedding) FROM face_embeddings")
            .map_err(|e| self.err(e))?;
        let rows = stmt
            .query_map([], |row| {
                // `substr` over a zero-length blob is NULL, not an empty
                // blob — the one row shape that would otherwise turn a
                // classification query into a type error.
                let head: Option<Vec<u8>> = row.get(0)?;
                let len: i64 = row.get(1)?;
                Ok((
                    head.as_deref().and_then(<[u8]>::first).copied(),
                    len.max(0) as usize,
                ))
            })
            .map_err(|e| self.err(e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| self.err(e))
    }

    /// Get all embeddings (all users) as raw bytes with sealed flag.
    /// Returns (embedding_id, model_user, raw_bytes, sealed) tuples.
    #[allow(clippy::type_complexity)]
    pub fn get_all_embeddings_raw(&self) -> Result<Vec<(u32, String, Vec<u8>, bool)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fe.id, fm.user, fe.embedding, fe.sealed
                 FROM face_embeddings fe
                 JOIN face_models fm ON fm.id = fe.model_id",
            )
            .map_err(|e| self.err(e))?;

        let rows = stmt
            .query_map([], |row| {
                let id: u32 = row.get(0)?;
                let user: String = row.get(1)?;
                let blob: Vec<u8> = row.get(2)?;
                let sealed: bool = row.get::<_, i64>(3)? != 0;
                Ok((id, user, blob, sealed))
            })
            .map_err(|e| self.err(e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| self.err(e))?);
        }
        Ok(results)
    }

    /// Record a failed authentication attempt for rate limiting.
    /// Inserts the current unix timestamp for the given user.
    pub fn record_auth_attempt(&self, user: &str) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO rate_limit (user, attempt_time) VALUES (?1, ?2)",
                params![user, now],
            )
            .map_err(|e| self.err(e))?;
        Ok(())
    }

    /// Check whether the user is within the rate limit.
    /// Returns `true` if the user has fewer than `max_attempts` in the last
    /// `window_secs` seconds (i.e. auth may proceed). Returns `false` if
    /// the limit has been reached.
    pub fn check_rate_limit(
        &self,
        user: &str,
        max_attempts: u32,
        window_secs: u64,
    ) -> Result<bool> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cutoff = now - window_secs as i64;
        let count: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM rate_limit WHERE user = ?1 AND attempt_time > ?2",
                params![user, cutoff],
                |row| row.get(0),
            )
            .map_err(|e| self.err(e))?;
        Ok(count < max_attempts)
    }

    /// Delete rate-limit entries older than `window_secs` seconds.
    /// Call occasionally to prevent unbounded table growth.
    pub fn cleanup_rate_limit(&self, window_secs: u64) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cutoff = now - window_secs as i64;
        self.conn
            .execute(
                "DELETE FROM rate_limit WHERE attempt_time <= ?1",
                params![cutoff],
            )
            .map_err(|e| self.err(e))?;
        Ok(())
    }

    /// Get the embedder model used by a user's most recent enrollment.
    /// Returns `None` if the user has no models.
    pub fn get_user_embedder_model(&self, user: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT embedder_model FROM face_models WHERE user = ?1 ORDER BY created_at DESC LIMIT 1",
            params![user],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(model) => Ok(Some(model)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(self.err(e)),
        }
    }

    /// Check if a user has any models enrolled with the given embedder model.
    pub fn has_models_for_embedder(&self, user: &str, embedder_model: &str) -> Result<bool> {
        let count: u32 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_models WHERE user = ?1 AND embedder_model = ?2",
                params![user, embedder_model],
                |row| row.get(0),
            )
            .map_err(|e| self.err(e))?;
        Ok(count > 0)
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
            .map_err(|e| self.err(e))?;
        Ok(count > 0)
    }

    /// Check if any user has any stored models.
    pub fn has_any_models(&self) -> Result<bool> {
        let count: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM face_models", [], |row| row.get(0))
            .map_err(|e| self.err(e))?;
        Ok(count > 0)
    }

    /// Remove all models for all users. Returns the number of models removed.
    pub fn clear_all(&self) -> Result<u32> {
        let affected = self
            .conn
            .execute("DELETE FROM face_models", [])
            .map_err(|e| self.err(e))?;
        Ok(affected as u32)
    }
}

fn secure_database_files(db_path: &Path) -> Result<()> {
    for path in [
        db_path.to_path_buf(),
        sqlite_sidecar_path(db_path, "-wal"),
        sqlite_sidecar_path(db_path, "-shm"),
    ] {
        ensure_mode(&path, 0o600).map_err(|e| StoreError::Denied {
            detail: format!("failed to secure database file: {e}"),
            path: path.clone(),
        })?;
    }

    Ok(())
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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
        let id = store.add_model("alice", "front", &emb, "").unwrap();
        let results = store.get_user_embeddings("alice").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, id);
        for (i, (got, want)) in results[0].1.iter().zip(emb.iter()).enumerate() {
            assert_eq!(got, want, "mismatch at index {i}");
        }
    }

    #[test]
    fn test_duplicate_label() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        let result = store.add_model("alice", "front", &emb, "");
        assert!(result.is_err());
    }

    #[test]
    fn test_list_models() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        store.add_model("alice", "side", &emb, "").unwrap();
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
        let id = store.add_model("alice", "front", &emb, "").unwrap();
        assert!(store.remove_model("alice", id).unwrap());
        let models = store.list_models("alice").unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn test_clear_user() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "a", &emb, "").unwrap();
        store.add_model("alice", "b", &emb, "").unwrap();
        store.add_model("alice", "c", &emb, "").unwrap();
        let count = store.clear_user("alice").unwrap();
        assert_eq!(count, 3);
        assert!(!store.has_models("alice").unwrap());
    }

    #[test]
    fn test_multi_user() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        store.add_model("bob", "front", &emb, "").unwrap();

        let alice_models = store.list_models("alice").unwrap();
        let bob_models = store.list_models("bob").unwrap();
        assert_eq!(alice_models.len(), 1);
        assert_eq!(bob_models.len(), 1);

        store.clear_user("alice").unwrap();
        assert!(!store.has_models("alice").unwrap());
        assert!(store.has_models("bob").unwrap());
    }

    #[test]
    fn test_list_users() {
        let store = FaceStore::open_memory().unwrap();
        assert!(store.list_users().unwrap().is_empty());

        let emb = test_embedding();
        store.add_model("bob", "front", &emb, "").unwrap();
        store.add_model("alice", "front", &emb, "").unwrap();
        store.add_model("alice", "side", &emb, "").unwrap();

        // Distinct and sorted — alice appears once despite two models.
        assert_eq!(store.list_users().unwrap(), vec!["alice", "bob"]);

        store.clear_user("alice").unwrap();
        assert_eq!(store.list_users().unwrap(), vec!["bob"]);
    }

    #[test]
    fn test_has_models() {
        let store = FaceStore::open_memory().unwrap();
        assert!(!store.has_models("alice").unwrap());
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
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

        store.add_model("alice", "test", &emb, "").unwrap();
        let results = store.get_user_embeddings("alice").unwrap();
        assert_eq!(results.len(), 1);
        for (i, (got, want)) in results[0].1.iter().zip(emb.iter()).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
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

        let id = store.add_model("alice", "front", &emb1, "").unwrap();
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
        store.add_model("alice", "front", &emb, "").unwrap();
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
        let id = store.add_model("alice", "front", &emb, "").unwrap();

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

    #[test]
    fn test_migration_v3_sealed_column() {
        let store = FaceStore::open_memory().unwrap();
        // The sealed column should exist with default 0
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        let raw = store.get_user_embeddings_raw("alice").unwrap();
        assert_eq!(raw.len(), 1);
        assert!(!raw[0].2, "newly added embedding should not be sealed");
    }

    #[test]
    fn test_add_model_raw() {
        let store = FaceStore::open_memory().unwrap();
        let data = vec![0xAA; 100]; // arbitrary raw data
        let id = store.add_model_raw("bob", "test", &data, true, "").unwrap();
        assert!(id > 0);

        let raw = store.get_user_embeddings_raw("bob").unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].1, data);
        assert!(raw[0].2, "should be marked as sealed");
    }

    #[test]
    fn test_update_embedding_sealed() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();

        // Use get_all_embeddings_raw to get the embedding ID (fe.id)
        // needed by update_embedding_sealed
        let all = store.get_all_embeddings_raw().unwrap();
        let emb_id = all[0].0;

        let raw = store.get_user_embeddings_raw("alice").unwrap();
        assert!(!raw[0].2);

        // Update to sealed
        let sealed_data = vec![0x01, 0xBB, 0xCC];
        store
            .update_embedding_sealed(emb_id, &sealed_data, true)
            .unwrap();

        let raw = store.get_user_embeddings_raw("alice").unwrap();
        assert_eq!(raw[0].1, sealed_data);
        assert!(raw[0].2);

        // Update back to unsealed
        let raw_data = vec![0xDD; 2048];
        store
            .update_embedding_sealed(emb_id, &raw_data, false)
            .unwrap();

        let raw = store.get_user_embeddings_raw("alice").unwrap();
        assert_eq!(raw[0].1, raw_data);
        assert!(!raw[0].2);
    }

    #[test]
    fn test_update_embedding_sealed_nonexistent() {
        let store = FaceStore::open_memory().unwrap();
        let result = store.update_embedding_sealed(9999, &[0u8; 10], true);
        assert!(result.is_err());
    }

    #[test]
    fn test_count_sealed() {
        let store = FaceStore::open_memory().unwrap();

        let (s, u) = store.count_sealed().unwrap();
        assert_eq!(s, 0);
        assert_eq!(u, 0);

        // Add some regular (unsealed) embeddings
        let emb = test_embedding();
        store.add_model("alice", "a", &emb, "").unwrap();
        store.add_model("alice", "b", &emb, "").unwrap();

        let (s, u) = store.count_sealed().unwrap();
        assert_eq!(s, 0);
        assert_eq!(u, 2);

        // Add a sealed embedding via raw
        store
            .add_model_raw("bob", "sealed", &[0x01; 50], true, "")
            .unwrap();

        let (s, u) = store.count_sealed().unwrap();
        assert_eq!(s, 1);
        assert_eq!(u, 2);
    }

    #[test]
    fn embedding_blob_shapes_report_the_version_byte_not_the_sealed_flag() {
        let store = FaceStore::open_memory().unwrap();
        // A flag that outlived its ciphertext: sealed = 1 over a plaintext
        // 2048-byte template. `count_sealed` calls this encrypted; the blob
        // says otherwise, and a caller deciding whether a new key would
        // orphan anything needs the blob's answer.
        store
            .add_model_raw("alice", "stale-flag", &[0u8; 2048], true, "e")
            .unwrap();
        store
            .add_model_raw("bob", "software", &[0x02u8; 96], true, "e")
            .unwrap();
        store
            .add_model_raw("carol", "empty", &[], true, "e")
            .unwrap();

        let mut shapes = store.embedding_blob_shapes().unwrap();
        shapes.sort();
        assert_eq!(
            shapes,
            vec![(None, 0), (Some(0x00), 2048), (Some(0x02), 96)]
        );
        assert_eq!(store.count_sealed().unwrap(), (3, 0));
    }

    #[test]
    fn test_rate_limit_under_limit() {
        let store = FaceStore::open_memory().unwrap();
        assert!(store.check_rate_limit("alice", 3, 60).unwrap());
        store.record_auth_attempt("alice").unwrap();
        store.record_auth_attempt("alice").unwrap();
        assert!(store.check_rate_limit("alice", 3, 60).unwrap());
    }

    #[test]
    fn test_rate_limit_at_limit() {
        let store = FaceStore::open_memory().unwrap();
        store.record_auth_attempt("alice").unwrap();
        store.record_auth_attempt("alice").unwrap();
        store.record_auth_attempt("alice").unwrap();
        assert!(!store.check_rate_limit("alice", 3, 60).unwrap());
    }

    #[test]
    fn test_rate_limit_separate_users() {
        let store = FaceStore::open_memory().unwrap();
        store.record_auth_attempt("alice").unwrap();
        store.record_auth_attempt("alice").unwrap();
        assert!(!store.check_rate_limit("alice", 2, 60).unwrap());
        // Bob is unaffected
        assert!(store.check_rate_limit("bob", 2, 60).unwrap());
    }

    #[test]
    fn test_rate_limit_cleanup() {
        let store = FaceStore::open_memory().unwrap();
        store.record_auth_attempt("alice").unwrap();
        // Cleanup with a 0-second window removes everything
        store.cleanup_rate_limit(0).unwrap();
        assert!(store.check_rate_limit("alice", 1, 60).unwrap());
    }

    #[test]
    fn test_rate_limit_zero_max() {
        let store = FaceStore::open_memory().unwrap();
        // With max_attempts=0, even zero attempts should block
        assert!(!store.check_rate_limit("alice", 0, 60).unwrap());
    }

    #[test]
    fn raw_with_device_returns_device_id_per_row() {
        let store = FaceStore::open_memory().unwrap();
        // One template bound to a camera, one legacy (NULL device_id).
        store
            .add_model_raw_with_device(
                "alice",
                "cam",
                b"blob-cam",
                true,
                "w600k",
                Some("046d:085e:S"),
            )
            .unwrap();
        store
            .add_model_raw_with_device("alice", "legacy", b"blob-legacy", true, "w600k", None)
            .unwrap();

        let mut rows = store.get_user_embeddings_raw_with_device("alice").unwrap();
        rows.sort_by(|a, b| a.1.cmp(&b.1)); // by blob bytes for determinism

        assert_eq!(rows.len(), 2);
        // blob-cam sorts before blob-legacy
        assert_eq!(rows[0].1, b"blob-cam");
        assert!(rows[0].2, "sealed flag should round-trip");
        assert_eq!(rows[0].3.as_deref(), Some("046d:085e:S"));
        assert_eq!(rows[1].1, b"blob-legacy");
        assert_eq!(rows[1].3, None);
    }

    #[test]
    fn test_rate_limit_persists_across_reopen() {
        let db_path = std::env::temp_dir().join(format!(
            "facelock-rate-limit-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let store = FaceStore::create(&db_path).unwrap();
        store.record_auth_attempt("alice").unwrap();
        drop(store);

        let reopened = FaceStore::open_existing(&db_path).unwrap();
        assert!(!reopened.check_rate_limit("alice", 1, 60).unwrap());

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(sqlite_sidecar_path(&db_path, "-wal"));
        let _ = std::fs::remove_file(sqlite_sidecar_path(&db_path, "-shm"));
    }

    #[test]
    fn test_sqlite_sidecar_paths_append_to_full_filename() {
        assert_eq!(
            sqlite_sidecar_path(Path::new("/tmp/facelock.sqlite"), "-wal"),
            PathBuf::from("/tmp/facelock.sqlite-wal")
        );
        assert_eq!(
            sqlite_sidecar_path(Path::new("/tmp/facelock"), "-shm"),
            PathBuf::from("/tmp/facelock-shm")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_secure_database_files_secures_real_sqlite_sidecars() {
        let db_path = std::env::temp_dir().join(format!(
            "facelock-sidecars-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let wal_path = sqlite_sidecar_path(&db_path, "-wal");
        let shm_path = sqlite_sidecar_path(&db_path, "-shm");

        std::fs::write(&db_path, b"db").unwrap();
        std::fs::write(&wal_path, b"wal").unwrap();
        std::fs::write(&shm_path, b"shm").unwrap();

        secure_database_files(&db_path).unwrap();

        for path in [&db_path, &wal_path, &shm_path] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "unexpected mode for {}", path.display());
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&wal_path);
        let _ = std::fs::remove_file(&shm_path);
    }

    #[test]
    fn test_add_model_with_device_round_trip() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store
            .add_model_with_device("alice", "front", &emb, "", Some("046d:085e:SER"))
            .unwrap();
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].device_id.as_deref(), Some("046d:085e:SER"));
    }

    #[test]
    fn test_add_model_default_device_id_is_null() {
        // The back-compat add_model stores NULL device_id (legacy/uncoupled).
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        let models = store.list_models("alice").unwrap();
        assert_eq!(models[0].device_id, None);
    }

    #[test]
    fn test_add_model_raw_with_device_round_trip() {
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw_with_device("bob", "sealed", &[0x02; 60], true, "", Some("1234:5678:"))
            .unwrap();
        let models = store.list_models("bob").unwrap();
        assert_eq!(models[0].device_id.as_deref(), Some("1234:5678:"));
    }

    /// Distinct raw blobs standing in for one enrollment's embeddings.
    fn blobs(count: u8) -> Vec<Vec<u8>> {
        (0..count).map(|i| vec![i; 8]).collect()
    }

    #[test]
    fn replace_model_stores_every_embedding_under_one_model() {
        let store = FaceStore::open_memory().unwrap();
        let blobs = blobs(3);
        let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();

        let id = store
            .replace_model_with_embeddings(
                "alice",
                "front",
                &refs,
                true,
                "w600k",
                Some("046d:085e:S"),
            )
            .unwrap();

        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, id);
        assert_eq!(models[0].label, "front");
        assert_eq!(models[0].embedder_model, "w600k");
        assert_eq!(models[0].device_id.as_deref(), Some("046d:085e:S"));

        let mut rows = store.get_user_embeddings_raw("alice").unwrap();
        rows.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(rows.len(), 3, "every blob lands under the one model");
        for (row, blob) in rows.iter().zip(&blobs) {
            assert_eq!(row.0, id);
            assert_eq!(&row.1, blob);
            assert!(row.2, "sealed flag applies to every row");
        }
    }

    #[test]
    fn replace_model_replaces_only_the_same_user_and_label() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let old_front = store.add_model("alice", "front", &emb, "").unwrap();
        let alice_side = store.add_model("alice", "side", &emb, "").unwrap();
        let bob_front = store.add_model("bob", "front", &emb, "").unwrap();

        let blobs = blobs(3);
        let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
        let new_front = store
            .replace_model_with_embeddings("alice", "front", &refs, false, "", None)
            .unwrap();

        assert_ne!(new_front, old_front, "the replacement is a new row");
        let mut alice: Vec<(u32, String)> = store
            .list_models("alice")
            .unwrap()
            .into_iter()
            .map(|m| (m.id, m.label))
            .collect();
        alice.sort();
        assert_eq!(
            alice,
            vec![
                (alice_side, "side".to_string()),
                (new_front, "front".to_string())
            ]
        );
        assert_eq!(store.list_models("bob").unwrap()[0].id, bob_front);

        // The old model's embedding went with it; only the new blobs remain
        // under "front".
        let front_rows: Vec<_> = store
            .get_user_embeddings_raw("alice")
            .unwrap()
            .into_iter()
            .filter(|r| r.0 == new_front)
            .collect();
        assert_eq!(front_rows.len(), 3);
        let orphaned: u32 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM face_embeddings WHERE model_id = ?1",
                params![old_front],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0, "cascade removed the old model's embeddings");
    }

    #[test]
    fn replace_model_rolls_back_to_the_old_model_when_an_insert_fails() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let old_id = store.add_model("alice", "front", &emb, "").unwrap();

        // Fault: the second embedding row of any model refuses to insert. The
        // delete and the model insert have already run inside the transaction
        // by the time this fires, so a non-transactional implementation would
        // leave the old model gone and a one-embedding replacement behind.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_second_embedding BEFORE INSERT ON face_embeddings
                 WHEN (SELECT COUNT(*) FROM face_embeddings WHERE model_id = NEW.model_id) >= 1
                 BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
            )
            .unwrap();

        let blobs = blobs(3);
        let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
        let err = store
            .replace_model_with_embeddings("alice", "front", &refs, false, "", None)
            .unwrap_err();
        assert!(
            err.to_string().contains("injected insert failure"),
            "the injected failure must surface, got: {err}"
        );

        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1, "exactly the old model remains: {models:?}");
        assert_eq!(models[0].id, old_id, "the old model row survived untouched");
        let rows = store.get_user_embeddings("alice").unwrap();
        assert_eq!(rows.len(), 1, "the old model keeps its embedding");
        assert_eq!(rows[0].0, old_id);
        assert_eq!(rows[0].1[0], emb[0]);
    }

    #[test]
    fn replace_model_refuses_fewer_than_the_minimum_embeddings() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let old_id = store.add_model("alice", "front", &emb, "").unwrap();

        // Two embeddings: enough to exist, not enough to be a template. The
        // store enforces the floor itself so no caller's gate can drift.
        let blobs = blobs(2);
        let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
        let err = store
            .replace_model_with_embeddings("alice", "front", &refs, false, "", None)
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Query { .. }) && err.to_string().contains("2 embeddings"),
            "a short set is refused with the count it got, got: {err:?}"
        );
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].id, old_id,
            "a refused replacement changes nothing"
        );
        assert_eq!(store.get_user_embeddings("alice").unwrap().len(), 1);
    }

    #[test]
    fn replace_model_refuses_an_empty_embedding_set() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        let old_id = store.add_model("alice", "front", &emb, "").unwrap();

        let err = store
            .replace_model_with_embeddings("alice", "front", &[], false, "", None)
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Query { .. }),
            "an empty set is a caller bug reported as a query error, got: {err:?}"
        );
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].id, old_id,
            "a refused replacement changes nothing"
        );
    }

    #[test]
    fn test_migration_v6_device_id_column() {
        // Fresh DB is at the latest schema; device_id defaults to NULL.
        let store = FaceStore::open_memory().unwrap();
        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert!(version >= 6, "schema should be at least V6, got {version}");
    }

    #[test]
    fn test_pre_v6_db_migrates_cleanly_without_data_loss() {
        // Build a pre-V6 database by hand (schema at V5: face_models has
        // embedder_model but NOT device_id), seed a model + embedding, then
        // reopen via FaceStore::open to run migrations and confirm the row
        // survives with a NULL device_id.
        let db_path = std::env::temp_dir().join(format!(
            "facelock-prev6-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
                CREATE TABLE face_models (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    user TEXT NOT NULL,
                    label TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    embedder_model TEXT NOT NULL DEFAULT '',
                    UNIQUE(user, label)
                );
                CREATE TABLE face_embeddings (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
                    embedding BLOB NOT NULL,
                    sealed INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE rate_limit (user TEXT NOT NULL, attempt_time INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (5);
                INSERT INTO face_models (user, label, created_at, embedder_model)
                    VALUES ('legacy', 'old-face', 1700000000, 'w600k_r50.onnx');
                ",
            )
            .unwrap();
            let emb = test_embedding();
            let bytes: &[u8] = bytemuck::cast_slice(emb.as_slice());
            conn.execute(
                "INSERT INTO face_embeddings (model_id, embedding) VALUES (1, ?1)",
                params![bytes],
            )
            .unwrap();
        }

        // Reopen: migrations run, adding device_id. Via `open_existing`
        // specifically — a present-but-old database is exactly the case that
        // constructor must still migrate.
        let store = FaceStore::open_existing(&db_path).unwrap();
        let models = store.list_models("legacy").unwrap();
        assert_eq!(models.len(), 1, "legacy model must survive migration");
        assert_eq!(models[0].label, "old-face");
        assert_eq!(models[0].device_id, None, "legacy row keeps NULL device_id");

        // Embedding data must be intact.
        let embs = store.get_user_embeddings("legacy").unwrap();
        assert_eq!(embs.len(), 1);

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert!(version >= 6);

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(sqlite_sidecar_path(&db_path, "-wal"));
        let _ = std::fs::remove_file(sqlite_sidecar_path(&db_path, "-shm"));
    }

    /// The whole point of the constructor split: a read path run against an
    /// install that has no database yet must leave the path empty and report
    /// the absence, never manufacture an empty database there.
    #[test]
    fn open_existing_on_a_missing_path_errors_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("facelock.db");

        assert!(FaceStore::open_existing(&missing).is_err());
        assert!(
            !missing.exists(),
            "open_existing must not create the database it failed to find"
        );
    }

    /// A failed `open_existing` must not leave WAL/SHM sidecars behind either —
    /// their presence is enough to make a path look occupied.
    #[test]
    fn open_existing_on_a_missing_path_creates_no_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("facelock.db");

        assert!(FaceStore::open_existing(&missing).is_err());

        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(&missing, suffix);
            assert!(
                !sidecar.exists(),
                "{} must not exist after a failed open",
                sidecar.display()
            );
        }
        // And nothing else appeared in the directory under another name.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn open_existing_reads_a_real_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");

        let created = FaceStore::create(&db).unwrap();
        created
            .add_model("alice", "front", &test_embedding(), "w600k_r50.onnx")
            .unwrap();
        drop(created);

        let store = FaceStore::open_existing(&db).unwrap();
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].label, "front");
        assert_eq!(store.get_user_embeddings("alice").unwrap().len(), 1);
    }

    #[test]
    fn create_makes_a_database_at_a_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");
        assert!(!db.exists());

        let store = FaceStore::create(&db).unwrap();
        assert!(
            db.is_file(),
            "create must bring the database into existence"
        );
        // Usable, not just present.
        store
            .add_model("alice", "front", &test_embedding(), "")
            .unwrap();
        assert!(store.has_models("alice").unwrap());
    }

    #[test]
    fn database_exists_reports_presence_without_creating() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");

        assert!(!FaceStore::database_exists(&db));
        assert!(!db.exists(), "the probe itself must not create anything");

        drop(FaceStore::create(&db).unwrap());
        assert!(FaceStore::database_exists(&db));

        // A directory at the path is not a database.
        let dir = tmp.path().join("subdir");
        std::fs::create_dir(&dir).unwrap();
        assert!(!FaceStore::database_exists(&dir));
    }

    /// Whether the test process would bypass the permission checks some of
    /// the `Denied` tests rely on (root ignores file modes).
    #[cfg(unix)]
    fn running_as_root() -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self")
            .map(|m| m.uid() == 0)
            .unwrap_or(false)
    }

    /// The failure classes as they arise from a real filesystem: a missing
    /// file is `Absent` — a *state*, carrying the path a caller may then
    /// legitimately create at — never an undifferentiated error.
    #[test]
    fn missing_file_is_absent_with_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("facelock.db");

        match FaceStore::open_existing(&missing).unwrap_err() {
            StoreError::Absent { path } => assert_eq!(path, missing),
            other => panic!("a missing database must classify as Absent, got {other:?}"),
        }
        match FaceStore::open_readonly(&missing).unwrap_err() {
            StoreError::Absent { path } => assert_eq!(path, missing),
            other => panic!("open_readonly must agree on Absent, got {other:?}"),
        }
    }

    #[test]
    fn garbage_file_is_corrupt_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");
        std::fs::write(&db, b"this is not a sqlite database").unwrap();

        let err = FaceStore::open_existing(&db).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "got {err:?}");
    }

    #[test]
    fn directory_at_path_is_corrupt_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("subdir");
        std::fs::create_dir(&dir).unwrap();

        let err = FaceStore::open_existing(&dir).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "got {err:?}");
    }

    #[test]
    fn locked_database_is_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");

        // A rollback-journal database (NOT the WAL mode FaceStore sets up),
        // so BEGIN EXCLUSIVE really excludes readers.
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder
            .execute_batch("CREATE TABLE t(x); BEGIN EXCLUSIVE;")
            .unwrap();

        let err = FaceStore::open_existing(&db).unwrap_err();
        assert!(matches!(err, StoreError::Busy { .. }), "got {err:?}");
        drop(holder);
    }

    /// The distinction the type exists for: a database facelock cannot even
    /// stat must read as `Denied` — "cannot tell" — never as `Absent`, which
    /// a destructive guard is entitled to treat as "nothing to protect".
    #[cfg(unix)]
    #[test]
    fn unstatable_path_is_denied_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return; // root bypasses the permission bits this test relies on
        }

        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("locked");
        std::fs::create_dir(&parent).unwrap();
        let db = parent.join("facelock.db");
        drop(FaceStore::create(&db).unwrap());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = FaceStore::open_existing(&db);
        // Restore before asserting so the tempdir can clean up on failure.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.unwrap_err();
        assert!(
            matches!(err, StoreError::Denied { .. }),
            "an unstatable database must be Denied, never Absent: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_denied_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return; // root bypasses the permission bits this test relies on
        }

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");
        drop(FaceStore::create(&db).unwrap());
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();

        let err = FaceStore::open_existing(&db).unwrap_err();
        assert!(matches!(err, StoreError::Denied { .. }), "got {err:?}");
    }

    #[test]
    fn test_get_all_embeddings_raw() {
        let store = FaceStore::open_memory().unwrap();
        let emb = test_embedding();
        store.add_model("alice", "front", &emb, "").unwrap();
        store
            .add_model_raw("bob", "sealed", &[0x01; 50], true, "")
            .unwrap();

        let all = store.get_all_embeddings_raw().unwrap();
        assert_eq!(all.len(), 2);

        let alice_row = all.iter().find(|(_, u, _, _)| u == "alice").unwrap();
        assert!(!alice_row.3);

        let bob_row = all.iter().find(|(_, u, _, _)| u == "bob").unwrap();
        assert!(bob_row.3);
        assert_eq!(bob_row.2, vec![0x01; 50]);
    }

    /// The guarantee the encryption-key gate is built on (#231): while a
    /// section is open, an enrollment cannot commit — it waits, and lands
    /// only after the section has ended.
    ///
    /// The enrollment runs on its own connection, as the daemon's does, and
    /// starts only once the section is known to be open. The flag is cleared
    /// at the end of the closure, before the lock is released, so a write
    /// that observed it still set would have to have slipped inside.
    #[test]
    fn an_exclusive_section_holds_off_an_enrollment_until_it_ends() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");
        let holder = FaceStore::create(&db).unwrap();
        let section_open = Arc::new(AtomicBool::new(true));

        let (announce, wait_for_section) = std::sync::mpsc::channel();
        let enroller_path = db.clone();
        let observed = Arc::clone(&section_open);
        let enrollment = std::thread::spawn(move || {
            let enroller = FaceStore::open_existing(&enroller_path).unwrap();
            wait_for_section.recv().expect("section never opened");
            let started = std::time::Instant::now();
            let blobs = vec![vec![0u8; 8]; MIN_EMBEDDINGS_PER_MODEL];
            let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
            let result =
                enroller.replace_model_with_embeddings("alice", "front", &refs, false, "e", None);
            (result, observed.load(Ordering::SeqCst), started.elapsed())
        });

        holder
            .with_exclusive(|_conn| {
                announce.send(()).expect("the enrollment thread is waiting");
                // Long enough that the enrollment is certainly blocked on the
                // lock, far short of the store's busy timeout.
                std::thread::sleep(std::time::Duration::from_millis(200));
                section_open.store(false, Ordering::SeqCst);
                Ok::<(), StoreError>(())
            })
            .unwrap();

        let (result, saw_section_open, elapsed) = enrollment.join().unwrap();
        assert!(
            result.is_ok(),
            "the enrollment must wait for the section, not fail: {result:?}"
        );
        assert!(
            !saw_section_open,
            "the enrollment committed while the section was still open"
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(100),
            "the enrollment did not block on the section: {elapsed:?}"
        );
        assert_eq!(holder.list_models("alice").unwrap().len(), 1);
    }

    /// A section that ends in `Err` still ends: the transaction rolls back and
    /// the next writer gets the lock.
    #[test]
    fn a_failed_exclusive_section_releases_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("facelock.db");
        let store = FaceStore::create(&db).unwrap();

        let err = store
            .with_exclusive(|_conn| {
                Err::<(), _>(StoreError::Query {
                    path: db.clone(),
                    detail: "refused".into(),
                })
            })
            .unwrap_err();
        assert!(matches!(err, StoreError::Query { .. }), "got {err:?}");

        let blobs = vec![vec![0u8; 8]; MIN_EMBEDDINGS_PER_MODEL];
        let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
        FaceStore::open_existing(&db)
            .unwrap()
            .replace_model_with_embeddings("alice", "front", &refs, false, "e", None)
            .expect("the lock must be released when the section fails");
    }
}
