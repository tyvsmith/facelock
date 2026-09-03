//! Schema-level fault injection for the store's failure paths.
//!
//! Several guards in facelock are only meaningful when a query *fails* on a
//! database that opened successfully — "the store answered no" and "the store
//! could not answer" are different facts, and code that conflates them is what
//! historically made destructive paths fail open. Proving that distinction
//! needs a database that is openable but broken, which no public store API can
//! produce. Hence raw SQL.
//!
//! These helpers live here rather than beside their callers because each one
//! encodes non-obvious coupling to the schema — migration version gating, the
//! exact column set a given query selects, SQLite's autoindex naming — and two
//! private copies means a future schema change breaks in two places, each
//! requiring the same non-obvious re-derivation. One copy, documented, is the
//! trade the reviewers preferred over a store trait seam.
//!
//! All helpers panic on failure: a fault injector that silently fails to
//! inject would turn the test it serves into a test of the happy path.

use std::path::Path;

/// Make every query that walks `face_models` (or its indexes) report
/// corruption, while leaving a database that still opens and migrates cleanly.
///
/// The trick is a page-type mismatch. We allocate donor pages of the
/// *opposite* btree type — an index page to stand in for the table, table
/// pages to stand in for its indexes — orphan the donors, and repoint
/// `face_models` and its indexes at them through `writable_schema`. Every
/// rootpage stays unique and inside the file, so opening the database and
/// running migrations both succeed; the corruption only surfaces when SQLite
/// actually walks the btree and finds the wrong page type.
///
/// **Schema coupling:** the repointed names must match the current schema.
/// `sqlite_autoindex_face_models_1` is SQLite's own name for the implicit
/// index behind the table's UNIQUE constraint — if that constraint is dropped
/// the name disappears and the `UPDATE` silently matches nothing, so this
/// helper asserts each repoint landed. Add new indexes on `face_models` here
/// when the schema grows them, or a query using the new index will succeed
/// where the test expects failure.
pub fn break_face_models_table(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open database for fault injection");
    conn.execute_batch("CREATE TABLE d1(x); CREATE INDEX di1 ON d1(x); CREATE TABLE d2(x);")
        .expect("create donor pages");

    let page = |name: &str| -> i64 {
        conn.query_row(
            "SELECT rootpage FROM sqlite_master WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("donor page {name} has no rootpage: {e}"))
    };
    let (p_d1, p_di1, p_d2) = (page("d1"), page("di1"), page("d2"));

    conn.execute_batch(&format!(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_master SET rootpage={p_di1} WHERE name='face_models';
         UPDATE sqlite_master SET rootpage={p_d1} WHERE name='idx_face_models_user';
         UPDATE sqlite_master SET rootpage={p_d2} WHERE name='sqlite_autoindex_face_models_1';
         DELETE FROM sqlite_master WHERE name IN ('d1','di1','d2');
         PRAGMA writable_schema=OFF;",
    ))
    .expect("repoint face_models rootpages");

    // Fail loudly if the schema moved out from under us: an UPDATE that
    // matched nothing leaves a *working* table, and the test that depends on
    // this would then silently assert nothing.
    for name in [
        "face_models",
        "idx_face_models_user",
        "sqlite_autoindex_face_models_1",
    ] {
        let repointed: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "schema object {name} is gone, so the fault was not injected \
                     — update break_face_models_table for the current schema: {e}"
                )
            });
        assert!(
            [p_d1, p_di1, p_d2].contains(&repointed),
            "{name} was not repointed at a donor page; the schema changed and \
             break_face_models_table needs re-deriving"
        );
    }
}

/// Make every query that walks `face_embeddings` (or its index) report
/// corruption, while leaving a database that still opens and migrates cleanly.
///
/// Same page-type mismatch as [`break_face_models_table`], aimed at the other
/// table. Its callers are the guards that must distinguish "the store says
/// there are no encrypted templates" from "the store could not be asked":
/// minting an encryption key on the second answer is what orphans real
/// enrollments, so the failing-query path needs a test of its own and no
/// public store API can produce a database that fails one query.
///
/// **Schema coupling:** `face_embeddings` has one explicit index,
/// `idx_face_embeddings_model`. It has no `sqlite_autoindex_*` — its
/// uniqueness comes from `INTEGER PRIMARY KEY`, which is the rowid itself.
/// Add new indexes here when the schema grows them, or a query served
/// entirely from a new index will succeed where the test expects failure.
pub fn break_face_embeddings_table(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open database for fault injection");
    conn.execute_batch("CREATE TABLE e1(x); CREATE INDEX ei1 ON e1(x);")
        .expect("create donor pages");

    let page = |name: &str| -> i64 {
        conn.query_row(
            "SELECT rootpage FROM sqlite_master WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("donor page {name} has no rootpage: {e}"))
    };
    let (p_e1, p_ei1) = (page("e1"), page("ei1"));

    conn.execute_batch(&format!(
        "PRAGMA writable_schema=ON;
         UPDATE sqlite_master SET rootpage={p_ei1} WHERE name='face_embeddings';
         UPDATE sqlite_master SET rootpage={p_e1} WHERE name='idx_face_embeddings_model';
         DELETE FROM sqlite_master WHERE name IN ('e1','ei1');
         PRAGMA writable_schema=OFF;",
    ))
    .expect("repoint face_embeddings rootpages");

    // An UPDATE that matched nothing leaves a *working* table, and the test
    // that depends on this would then silently assert nothing.
    for name in ["face_embeddings", "idx_face_embeddings_model"] {
        let repointed: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "schema object {name} is gone, so the fault was not injected \
                     — update break_face_embeddings_table for the current schema: {e}"
                )
            });
        assert!(
            [p_e1, p_ei1].contains(&repointed),
            "{name} was not repointed at a donor page; the schema changed and \
             break_face_embeddings_table needs re-deriving"
        );
    }
}

/// Remove the `embedder_model` column from `face_models`, so a query that
/// selects it fails while the database as a whole stays open and usable.
///
/// Its only caller is the daemon's
/// `authenticate_storage_failure_is_error_and_charges_no_rate_limit`
/// integration test.
///
/// **Schema coupling, two ways.** First, the column must actually be in the
/// select list of the query under test — drop a column nothing reads and the
/// injection is a no-op. Second, this survives re-opening only because
/// migrations do not put the column back: the `CREATE TABLE IF NOT EXISTS`
/// statements no-op on the still-present table, and the migration that adds
/// `embedder_model` is gated behind a schema version this database is already
/// past. If that gating changes, a re-open will heal the fault and the test
/// will pass for the wrong reason.
pub fn drop_embedder_model_column(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open database for fault injection");
    conn.execute_batch("ALTER TABLE face_models DROP COLUMN embedder_model;")
        .expect("drop embedder_model column");

    let present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('face_models') WHERE name = 'embedder_model'",
            [],
            |r| r.get(0),
        )
        .expect("inspect face_models columns");
    assert_eq!(
        present, 0,
        "embedder_model is still present, so no fault was injected"
    );
}
