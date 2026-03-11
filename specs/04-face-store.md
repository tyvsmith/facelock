# Spec 04: Face Store (SQLite)

**Phase**: 2 (Components) | **Crate**: facelock-store | **Depends on**: 01 | **Parallel with**: 02, 03

## Goal

Persistent storage for face embeddings using SQLite. Supports CRUD operations, multi-user isolation, and embedding serialization.

## Dependencies

- `facelock-core` (for `FaceEmbedding`, `FaceModelInfo`, `FacelockError`)
- `rusqlite` with `bundled` feature (statically linked SQLite)
- `bytemuck` with `derive` feature (zero-copy embedding serialization)

## Modules

### `db.rs` -- Database Operations

```rust
pub struct FaceStore {
    conn: rusqlite::Connection,
}

impl FaceStore {
    /// Open database, run migrations, return store
    pub fn open(db_path: &Path) -> Result<Self>;

    /// Open in-memory database (for testing)
    pub fn open_memory() -> Result<Self>;

    /// Add a face model with its embedding. Returns model ID.
    pub fn add_model(
        &self,
        user: &str,
        label: &str,
        embedding: &FaceEmbedding,
    ) -> Result<u32>;

    /// Get all embeddings for a user (for matching)
    pub fn get_user_embeddings(&self, user: &str) -> Result<Vec<(u32, FaceEmbedding)>>;

    /// List face models for a user (metadata only)
    pub fn list_models(&self, user: &str) -> Result<Vec<FaceModelInfo>>;

    /// Remove a specific model by ID
    pub fn remove_model(&self, user: &str, model_id: u32) -> Result<bool>;

    /// Remove all models for a user
    pub fn clear_user(&self, user: &str) -> Result<u32>;  // returns count removed

    /// Check if a user has any models
    pub fn has_models(&self, user: &str) -> Result<bool>;
}
```

### `migrations.rs` -- Schema Management

```rust
/// Run all pending migrations
pub fn run_migrations(conn: &rusqlite::Connection) -> Result<()>;
```

**Schema** (from `docs/contracts.md`):

```sql
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
```

### Embedding Serialization

Use `bytemuck` for zero-copy conversion between `[f32; 512]` and `[u8; 2048]`:

```rust
// Store: cast f32 array to byte slice
let bytes: &[u8] = bytemuck::cast_slice(embedding.as_slice());
// → INSERT INTO face_embeddings (model_id, embedding) VALUES (?, ?)

// Load: cast byte slice back to f32 array
let floats: &[f32] = bytemuck::cast_slice(blob);
let mut embedding = [0f32; 512];
embedding.copy_from_slice(floats);
```

### Transaction Safety

- `add_model`: INSERT face_models + INSERT face_embeddings in a transaction
- `clear_user`: DELETE face_models WHERE user = ? in a transaction (CASCADE deletes embeddings)
- `remove_model`: verify user owns model, then DELETE

## Tests

- Add model, retrieve embedding, verify identical values
- Add model with duplicate label for same user: error
- List models: returns metadata without embedding data
- Remove model: gone from subsequent queries
- Clear user: all models removed, count correct
- Multi-user: alice and bob don't interfere
- has_models: true after add, false after clear
- Empty store: list returns empty, has_models returns false
- Embedding round-trip: bytemuck cast preserves all 512 float values

## Acceptance Criteria

1. CRUD operations work correctly
2. Embedding round-trip is bit-exact
3. Multi-user isolation (users can't see/modify each other's data)
4. Duplicate label produces clear error
5. CASCADE delete removes embeddings when model deleted
6. In-memory database works for testing
7. All unit tests pass

## Verification

```bash
cargo test -p facelock-store
cargo clippy -p facelock-store
```
