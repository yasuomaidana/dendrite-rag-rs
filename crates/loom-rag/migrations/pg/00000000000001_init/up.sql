DO $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('loom-rag:vector-extension'));
    CREATE EXTENSION IF NOT EXISTS vector;
END
$$;
CREATE TABLE IF NOT EXISTS rag_schema (version int NOT NULL);
CREATE TABLE IF NOT EXISTS rag_repos (id serial PRIMARY KEY, name text UNIQUE NOT NULL);
CREATE TABLE IF NOT EXISTS rag_files (
    repo int NOT NULL, path text NOT NULL, entry jsonb NOT NULL, PRIMARY KEY (repo, path));
CREATE TABLE IF NOT EXISTS rag_chunks (
    repo int NOT NULL, id text NOT NULL, path text NOT NULL, chunk jsonb NOT NULL,
    PRIMARY KEY (repo, id));
CREATE INDEX IF NOT EXISTS rag_chunks_path ON rag_chunks (repo, path);
CREATE TABLE IF NOT EXISTS rag_embeddings (
    model text NOT NULL, hash text NOT NULL, vec vector NOT NULL, PRIMARY KEY (model, hash));
CREATE TABLE IF NOT EXISTS rag_meta (
    repo int NOT NULL, key text NOT NULL, value text NOT NULL, PRIMARY KEY (repo, key));
CREATE TABLE IF NOT EXISTS rag_vectors (
    repo int NOT NULL, id text NOT NULL, tag text NOT NULL, vec vector NOT NULL,
    PRIMARY KEY (repo, id));
CREATE TABLE IF NOT EXISTS rag_community_files (
    repo int NOT NULL, path text NOT NULL, cid int NOT NULL, PRIMARY KEY (repo, path));
CREATE TABLE IF NOT EXISTS rag_community_chunks (
    repo int NOT NULL, chunk text NOT NULL, cid int NOT NULL, PRIMARY KEY (repo, chunk));
CREATE TABLE IF NOT EXISTS rag_communities (
    repo int NOT NULL, id int NOT NULL, record jsonb NOT NULL, PRIMARY KEY (repo, id));
CREATE TABLE IF NOT EXISTS rag_docs (
    repo int NOT NULL, chunk text NOT NULL, path text NOT NULL,
    len_text int NOT NULL, len_symbols int NOT NULL, len_path int NOT NULL,
    PRIMARY KEY (repo, chunk));
CREATE INDEX IF NOT EXISTS rag_docs_path ON rag_docs (repo, path);
CREATE TABLE IF NOT EXISTS rag_postings (
    repo int NOT NULL, term text NOT NULL, field smallint NOT NULL, chunk text NOT NULL,
    tf int NOT NULL, PRIMARY KEY (repo, term, field, chunk));
CREATE INDEX IF NOT EXISTS rag_postings_chunk ON rag_postings (repo, chunk);
CREATE INDEX IF NOT EXISTS rag_chunks_hash ON rag_chunks ((chunk->>'content_hash'));
CREATE TABLE IF NOT EXISTS rag_commits (
    repo int NOT NULL, sha text NOT NULL, parents text[] NOT NULL, PRIMARY KEY (repo, sha));
CREATE TABLE IF NOT EXISTS rag_branch_heads (
    repo int NOT NULL, branch text NOT NULL, sha text NOT NULL, updated_at timestamptz NOT NULL,
    PRIMARY KEY (repo, branch));
CREATE TABLE IF NOT EXISTS rag_snapshots (
    repo int NOT NULL, sha text NOT NULL, chunk text NOT NULL, path text NOT NULL, hash text NOT NULL,
    PRIMARY KEY (repo, sha, chunk));
CREATE INDEX IF NOT EXISTS rag_snapshots_hash ON rag_snapshots (hash);
CREATE TABLE IF NOT EXISTS rag_llm_cache (
    model text NOT NULL, prompt_hash text NOT NULL, value text NOT NULL,
    PRIMARY KEY (model, prompt_hash));
CREATE TABLE IF NOT EXISTS rag_concepts (
    repo int NOT NULL, path text NOT NULL, concepts text NOT NULL, PRIMARY KEY (repo, path));
