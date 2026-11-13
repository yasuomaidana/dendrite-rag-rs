//! Diesel table definitions of the SQLite ledger; the DDL lives in `migrations/sqlite`.
//! The tables mirror the redb tables: a file's `entry`, a chunk's `record` and a community's
//! `record` are JSON text, and an embedding's `vec` is little-endian f32.

use diesel::table;

table! {
    files (path) {
        path -> Text,
        entry -> Text,
    }
}

table! {
    chunks (id) {
        id -> Text,
        path -> Text,
        content_hash -> Text,
        record -> Text,
    }
}

table! {
    embeddings (model, hash) {
        model -> Text,
        hash -> Text,
        vec -> Binary,
    }
}

table! {
    meta (key) {
        key -> Text,
        value -> Text,
    }
}

table! {
    community_files (path) {
        path -> Text,
        cid -> Integer,
    }
}

table! {
    community_chunks (chunk) {
        chunk -> Text,
        cid -> Integer,
    }
}

table! {
    communities (id) {
        id -> Integer,
        record -> Text,
    }
}

table! {
    llm_cache (model, prompt_hash) {
        model -> Text,
        prompt_hash -> Text,
        value -> Text,
    }
}

table! {
    concepts (path) {
        path -> Text,
        value -> Text,
    }
}
