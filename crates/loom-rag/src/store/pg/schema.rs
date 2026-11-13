//! Diesel table definitions of the shared tier; the DDL lives in `migrations/pg`.

use diesel::table;
use pgvector::sql_types::Vector;

table! {
    rag_schema (version) {
        version -> Int4,
    }
}

table! {
    rag_repos (id) {
        id -> Int4,
        name -> Text,
    }
}

table! {
    rag_files (repo, path) {
        repo -> Int4,
        path -> Text,
        entry -> Jsonb,
    }
}

table! {
    rag_chunks (repo, id) {
        repo -> Int4,
        id -> Text,
        path -> Text,
        chunk -> Jsonb,
    }
}

table! {
    use diesel::sql_types::*;
    use super::Vector;

    rag_embeddings (model, hash) {
        model -> Text,
        hash -> Text,
        vec -> Vector,
    }
}

table! {
    rag_meta (repo, key) {
        repo -> Int4,
        key -> Text,
        value -> Text,
    }
}

table! {
    use diesel::sql_types::*;
    use super::Vector;

    rag_vectors (repo, id) {
        repo -> Int4,
        id -> Text,
        tag -> Text,
        vec -> Vector,
    }
}

table! {
    rag_community_files (repo, path) {
        repo -> Int4,
        path -> Text,
        cid -> Int4,
    }
}

table! {
    rag_community_chunks (repo, chunk) {
        repo -> Int4,
        chunk -> Text,
        cid -> Int4,
    }
}

table! {
    rag_communities (repo, id) {
        repo -> Int4,
        id -> Int4,
        record -> Jsonb,
    }
}

table! {
    rag_docs (repo, chunk) {
        repo -> Int4,
        chunk -> Text,
        path -> Text,
        len_text -> Int4,
        len_symbols -> Int4,
        len_path -> Int4,
    }
}

table! {
    rag_postings (repo, term, field, chunk) {
        repo -> Int4,
        term -> Text,
        field -> Int2,
        chunk -> Text,
        tf -> Int4,
    }
}

table! {
    rag_commits (repo, sha) {
        repo -> Int4,
        sha -> Text,
        parents -> Array<Text>,
    }
}

table! {
    rag_branch_heads (repo, branch) {
        repo -> Int4,
        branch -> Text,
        sha -> Text,
        updated_at -> Timestamptz,
    }
}

table! {
    rag_snapshots (repo, sha, chunk) {
        repo -> Int4,
        sha -> Text,
        chunk -> Text,
        path -> Text,
        hash -> Text,
    }
}

table! {
    rag_llm_cache (model, prompt_hash) {
        model -> Text,
        prompt_hash -> Text,
        value -> Text,
    }
}

table! {
    rag_concepts (repo, path) {
        repo -> Int4,
        path -> Text,
        concepts -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    rag_repos,
    rag_files,
    rag_chunks,
    rag_embeddings,
    rag_vectors,
    rag_commits,
    rag_snapshots,
);
