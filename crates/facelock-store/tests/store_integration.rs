//! Integration tests for the face store lifecycle.

use facelock_core::types::FaceEmbedding;
use facelock_store::FaceStore;

/// Create a deterministic test embedding where each element is `seed + index / 512.0`.
fn make_embedding(seed: f32) -> FaceEmbedding {
    let mut e = [0.0f32; 512];
    for (i, v) in e.iter_mut().enumerate() {
        *v = seed + (i as f32 / 512.0);
    }
    e
}

// ---------------------------------------------------------------------------
// Full lifecycle
// ---------------------------------------------------------------------------

#[test]
fn full_lifecycle_add_list_get_remove() {
    let store = FaceStore::open_memory().unwrap();
    let emb = make_embedding(1.0);

    // Add a model
    let model_id = store.add_model("alice", "front", &emb, "").unwrap();
    assert!(model_id > 0);

    // List models
    let models = store.list_models("alice").unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, model_id);
    assert_eq!(models[0].user, "alice");
    assert_eq!(models[0].label, "front");

    // Get embeddings
    let embeddings = store.get_user_embeddings("alice").unwrap();
    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].0, model_id);

    // Remove model
    let removed = store.remove_model("alice", model_id).unwrap();
    assert!(removed);

    // Verify empty
    let models = store.list_models("alice").unwrap();
    assert!(models.is_empty());
    let embeddings = store.get_user_embeddings("alice").unwrap();
    assert!(embeddings.is_empty());
}

// ---------------------------------------------------------------------------
// Multi-user isolation
// ---------------------------------------------------------------------------

#[test]
fn multi_user_isolation() {
    let store = FaceStore::open_memory().unwrap();
    let emb_a = make_embedding(1.0);
    let emb_b = make_embedding(2.0);

    let alice_id = store.add_model("alice", "default", &emb_a, "").unwrap();
    let bob_id = store.add_model("bob", "default", &emb_b, "").unwrap();

    // Each user sees only their own models
    let alice_models = store.list_models("alice").unwrap();
    assert_eq!(alice_models.len(), 1);
    assert_eq!(alice_models[0].id, alice_id);

    let bob_models = store.list_models("bob").unwrap();
    assert_eq!(bob_models.len(), 1);
    assert_eq!(bob_models[0].id, bob_id);

    // Alice cannot remove Bob's model
    let removed = store.remove_model("alice", bob_id).unwrap();
    assert!(!removed, "alice should not be able to remove bob's model");

    // Bob's model should still exist
    assert!(store.has_models("bob").unwrap());

    // Clearing alice does not affect bob
    store.clear_user("alice").unwrap();
    assert!(!store.has_models("alice").unwrap());
    assert!(store.has_models("bob").unwrap());
}

// ---------------------------------------------------------------------------
// Embedding round-trip: bit-exact f32 comparison
// ---------------------------------------------------------------------------

#[test]
fn embedding_round_trip_bit_exact() {
    let store = FaceStore::open_memory().unwrap();

    // Use interesting float values
    let mut emb = [0.0f32; 512];
    emb[0] = 1.0;
    emb[1] = -1.0;
    emb[2] = std::f32::consts::PI;
    emb[3] = std::f32::consts::E;
    emb[4] = f32::MIN_POSITIVE;
    emb[5] = f32::EPSILON;
    emb[100] = 0.123_456_79;
    emb[511] = 42.0;

    store.add_model("alice", "precision-test", &emb, "").unwrap();

    let results = store.get_user_embeddings("alice").unwrap();
    assert_eq!(results.len(), 1);

    for i in 0..512 {
        assert_eq!(
            results[0].1[i].to_bits(),
            emb[i].to_bits(),
            "bit-exact mismatch at index {i}: stored={}, retrieved={}",
            emb[i],
            results[0].1[i]
        );
    }
}

// ---------------------------------------------------------------------------
// Clear user
// ---------------------------------------------------------------------------

#[test]
fn clear_user_removes_all_models() {
    let store = FaceStore::open_memory().unwrap();
    let emb = make_embedding(0.5);

    store.add_model("alice", "model-a", &emb, "").unwrap();
    store.add_model("alice", "model-b", &emb, "").unwrap();
    store.add_model("alice", "model-c", &emb, "").unwrap();

    assert_eq!(store.list_models("alice").unwrap().len(), 3);

    let removed_count = store.clear_user("alice").unwrap();
    assert_eq!(removed_count, 3);

    assert!(store.list_models("alice").unwrap().is_empty());
    assert!(store.get_user_embeddings("alice").unwrap().is_empty());
}

#[test]
fn clear_nonexistent_user_returns_zero() {
    let store = FaceStore::open_memory().unwrap();
    let count = store.clear_user("nobody").unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// has_models
// ---------------------------------------------------------------------------

#[test]
fn has_models_reflects_state_changes() {
    let store = FaceStore::open_memory().unwrap();

    // Initially false
    assert!(!store.has_models("alice").unwrap());

    // True after adding
    let emb = make_embedding(0.0);
    store.add_model("alice", "first", &emb, "").unwrap();
    assert!(store.has_models("alice").unwrap());

    // Still true after adding a second
    store.add_model("alice", "second", &emb, "").unwrap();
    assert!(store.has_models("alice").unwrap());

    // False after clearing
    store.clear_user("alice").unwrap();
    assert!(!store.has_models("alice").unwrap());
}

// ---------------------------------------------------------------------------
// Duplicate label
// ---------------------------------------------------------------------------

#[test]
fn duplicate_label_same_user_errors() {
    let store = FaceStore::open_memory().unwrap();
    let emb = make_embedding(0.0);

    store.add_model("alice", "front", &emb, "").unwrap();
    let result = store.add_model("alice", "front", &emb, "");
    assert!(
        result.is_err(),
        "adding duplicate label for same user should fail"
    );
}

#[test]
fn same_label_different_users_succeeds() {
    let store = FaceStore::open_memory().unwrap();
    let emb = make_embedding(0.0);

    store.add_model("alice", "front", &emb, "").unwrap();
    // Different user with the same label should be fine
    let result = store.add_model("bob", "front", &emb, "");
    assert!(
        result.is_ok(),
        "same label for different users should succeed"
    );
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn remove_nonexistent_model_returns_false() {
    let store = FaceStore::open_memory().unwrap();
    let removed = store.remove_model("alice", 9999).unwrap();
    assert!(!removed);
}

#[test]
fn empty_store_queries_succeed() {
    let store = FaceStore::open_memory().unwrap();
    assert!(store.list_models("alice").unwrap().is_empty());
    assert!(store.get_user_embeddings("alice").unwrap().is_empty());
    assert!(!store.has_models("alice").unwrap());
}

#[test]
fn model_metadata_has_reasonable_timestamp() {
    let store = FaceStore::open_memory().unwrap();
    let emb = make_embedding(0.0);
    store.add_model("alice", "timestamped", &emb, "").unwrap();

    let models = store.list_models("alice").unwrap();
    assert_eq!(models.len(), 1);

    // created_at should be a recent Unix timestamp (after 2024-01-01)
    let min_timestamp: u64 = 1_704_067_200; // 2024-01-01 00:00:00 UTC
    assert!(
        models[0].created_at >= min_timestamp,
        "created_at ({}) should be a recent Unix timestamp",
        models[0].created_at
    );
}
