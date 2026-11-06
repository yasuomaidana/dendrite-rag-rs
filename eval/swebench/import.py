"""Imports SWE-bench Verified for file localization evaluation (DRAG-54).

Reads the Hugging Face dataset `princeton-nlp/SWE-bench_Verified` via pyarrow:
500 instances across 12 Python repositories and 499 base commits.
Extracts gold files from `patch` (`test_patch` is excluded). Gold files are the pre-image
paths (group 1 of each `diff --git a/<path> b/<path>` line), excluding files created by the
patch (a `new file mode` line follows their header): a created file cannot be localized in
the base commit. Instances left with no gold file are skipped from the output (the count is
printed).
Generates deterministic query variants (Q1, Q2, Q3) and tags leakage classes, computed once
from Q1 (the full problem statement): `path_explicit` when the statement mentions a gold file
(its full path, its basename, or its dotted module path), `traceback_symbol` when it has a
traceback, `File "..."` frames or backticked identifiers, else `opaque_nl`.
Marks the Stratified-120 subset: 16 evenly spaced (by created_at) instances from each of the
5 largest repositories (80) plus 40 evenly spaced (by created_at) from the other 7
repositories taken together. Those 7 repositories hold only 84 instances in total, so the
smallest ones (flask: 1, seaborn: 2) are rarely sampled. Pools smaller than the quota are
taken whole; empty pools yield nothing (handled without division by zero).

Usage:
    uv run --no-project --with pyarrow python -I eval/swebench/import.py eval/swebench/verified.json
    uv run --no-project --with pyarrow python -I eval/swebench/import.py eval/swebench/verified.json --repos-cache /tmp/swebench-repos --clone-repos
"""

import argparse
import io
import json
import os
import re
import subprocess
import urllib.request
from collections import Counter

HUB = "https://huggingface.co"
DATASET = "princeton-nlp/SWE-bench_Verified"

DIFF_GIT_RE = re.compile(r"^diff --git a/(.*) b/(.*)$", re.MULTILINE)
TRACEBACK_FRAME_RE = re.compile(r'File "([^"]+)", line (\d+)(?:, in (\w+))?')
IDENTIFIER_RE = re.compile(r"\b[A-Za-z_][A-Za-z0-9_.]+\b")


def download_parquet():
    """Downloads and returns pyarrow Table from Hugging Face."""
    import pyarrow.parquet as pq

    url = f"{HUB}/datasets/{DATASET}/resolve/main/data/test-00000-of-00001.parquet"
    print(f"Downloading {DATASET} test split...")
    with urllib.request.urlopen(url, timeout=300) as r:
        data = r.read()
    return pq.read_table(io.BytesIO(data))


def extract_gold_files(patch: str) -> list[str]:
    """Extracts unique changed files from a diff patch, excluding files created by the patch."""
    files = []
    matches = list(DIFF_GIT_RE.finditer(patch))
    for i, match in enumerate(matches):
        path = match.group(1).strip()
        if not path or path == "/dev/null":
            continue
        # Check if a "new file mode" line follows the diff --git header
        start = match.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(patch)
        block = patch[start:end]
        header = re.split(r"\n(?:---|\+\+\+|@@)", block, maxsplit=1)[0]
        if re.search(r"^new file mode\b", header, re.MULTILINE):
            continue
        if path not in files:
            files.append(path)
    return files


def gold_identifiers(gold_files: list[str]) -> list[str]:
    """Collects full paths, basenames, and dotted module paths for gold files."""
    candidates = []
    for f in gold_files:
        p = f.strip()
        while p.startswith("./"):
            p = p[2:]
        if not p:
            continue
        # 1. Full path
        candidates.append(p)
        # 2. Basename
        base = os.path.basename(p)
        if base:
            candidates.append(base)
        # 3. Dotted module paths
        if p.endswith(".py"):
            mod = p[:-3]
            candidates.append(mod.replace("/", "."))
            if mod.endswith("/__init__"):
                candidates.append(mod[:-9].replace("/", "."))
            # Also handle src/ prefix
            if p.startswith("src/"):
                no_src = p[4:]
                candidates.append(no_src)
                no_src_base = os.path.basename(no_src)
                if no_src_base:
                    candidates.append(no_src_base)
                if no_src.endswith(".py"):
                    no_src_mod = no_src[:-3]
                    candidates.append(no_src_mod.replace("/", "."))
                    if no_src_mod.endswith("/__init__"):
                        candidates.append(no_src_mod[:-9].replace("/", "."))
    # Deduplicate preserving order, keep identifiers of length >= 3
    seen = set()
    result = []
    for c in candidates:
        if c and len(c) >= 3 and c not in seen:
            seen.add(c)
            result.append(c)
    return result


def classify_leakage(text: str, gold_files: list[str]) -> str:
    """Tags query with a leakage class: path_explicit, traceback_symbol, or opaque_nl."""
    # 1. Check path_explicit: statement mentions a gold file (full path, basename, or dotted module path)
    for ident in gold_identifiers(gold_files):
        pattern = r"(?<![A-Za-z0-9_])" + re.escape(ident) + r"(?![A-Za-z0-9_])"
        if re.search(pattern, text):
            return "path_explicit"

    # 2. Check traceback_symbol: traceback line, File "...", line N frames, or backticked identifiers
    if (
        "Traceback (most recent call last)" in text
        or re.search(r'File "[^"]+", line \d+', text)
        or re.search(r"`[A-Za-z_][A-Za-z0-9_.]*`", text)
    ):
        return "traceback_symbol"

    # 3. Otherwise opaque_nl
    return "opaque_nl"


def build_q2(problem_statement: str) -> str:
    """Q2: Title and first paragraph (<= 256 tokens)."""
    paragraphs = [p.strip() for p in problem_statement.split("\n\n") if p.strip()]
    if not paragraphs:
        paragraphs = [problem_statement.strip()]
    
    q2_text = paragraphs[0]
    if len(paragraphs) > 1 and len(q2_text.split()) < 64:
        q2_text = f"{q2_text}\n\n{paragraphs[1]}"

    words = q2_text.split()
    if len(words) > 256:
        words = words[:256]
        q2_text = " ".join(words)
    return q2_text


def build_q3(problem_statement: str) -> str:
    """Q3: Q1 plus identifiers and traceback frames extracted by regex."""
    frames = []
    for m in TRACEBACK_FRAME_RE.finditer(problem_statement):
        f = m.group(1)
        line = m.group(2)
        func = m.group(3) or ""
        frames.append(f"{f}:{line} {func}".strip())
    
    # Backtick code identifiers or symbol-like names
    code_blocks = re.findall(r"`([^`]+)`", problem_statement)
    symbols = []
    for b in code_blocks:
        for ident in IDENTIFIER_RE.findall(b):
            if len(ident) > 2 and ident not in symbols:
                symbols.append(ident)

    extra_parts = []
    if frames:
        extra_parts.append("Traceback frames:\n" + "\n".join(dict.fromkeys(frames)))
    if symbols:
        extra_parts.append("Key symbols: " + ", ".join(symbols[:30]))

    if extra_parts:
        return f"{problem_statement}\n\n" + "\n\n".join(extra_parts)
    return problem_statement


def clone_repo(repo: str, repos_cache: str):
    """Clones repository into repos_cache if not already cloned."""
    target_dir = os.path.join(repos_cache, repo)
    if os.path.isdir(target_dir):
        return target_dir
    os.makedirs(os.path.dirname(target_dir), exist_ok=True)
    git_url = f"https://github.com/{repo}.git"
    print(f"Cloning {git_url} into {target_dir}...")
    subprocess.run(["git", "clone", git_url, target_dir], check=True)
    return target_dir


def main():
    p = argparse.ArgumentParser(description="Import SWE-bench Verified dataset")
    p.add_argument("out", help="Output JSON file for SWE-bench instances")
    p.add_argument("--repos-cache", help="Directory where repositories are cloned/cached")
    p.add_argument("--clone-repos", action="store_true", help="Clone repositories into repos-cache")
    args = p.parse_args()

    table = download_parquet()
    raw_rows = table.to_pylist()
    print(f"Loaded {len(raw_rows)} raw instances from SWE-bench Verified.")

    # Filter instances: gold files exclude created files; skip instances with no gold file remaining
    valid_rows = []
    skipped_no_gold = 0
    for r in raw_rows:
        gold_files = extract_gold_files(r["patch"])
        if not gold_files:
            skipped_no_gold += 1
            continue
        r["_gold_files"] = gold_files
        valid_rows.append(r)

    print(f"Skipped {skipped_no_gold} instances with no gold files (created files only or empty diff).")
    print(f"Retained {len(valid_rows)} instances with gold files.")

    # Group instances by repository
    repo_instances = {}
    for r in valid_rows:
        repo_instances.setdefault(r["repo"], []).append(r)

    # Sort repositories by count descending
    sorted_repos = sorted(repo_instances.keys(), key=lambda r: len(repo_instances[r]), reverse=True)

    # Select Stratified-120: 16 evenly spaced (by created_at) from each of top 5 repos + 40 evenly spaced from other 7
    stratified_120_ids = set()
    for repo in sorted_repos[:5]:
        insts = repo_instances[repo]
        insts.sort(key=lambda x: x.get("created_at") or "")
        take = min(16, len(insts))
        if take > 0:
            step = len(insts) / take
            for i in range(take):
                idx = int(i * step)
                stratified_120_ids.add(insts[idx]["instance_id"])

    other_insts = []
    for repo in sorted_repos[5:]:
        other_insts.extend(repo_instances[repo])
    other_insts.sort(key=lambda x: x.get("created_at") or "")

    # 40 from other 7 repos taken together
    take_other = min(40, len(other_insts))
    if take_other > 0:
        step = len(other_insts) / take_other
        for i in range(take_other):
            idx = int(i * step)
            stratified_120_ids.add(other_insts[idx]["instance_id"])

    processed = []
    single_file_count = 0
    multi_file_count = 0
    leakage_counts = Counter()

    for r in valid_rows:
        instance_id = r["instance_id"]
        repo = r["repo"]
        base_commit = r["base_commit"]
        prob = r["problem_statement"]
        gold_files = r["_gold_files"]

        if len(gold_files) == 1:
            single_file_count += 1
        elif len(gold_files) > 1:
            multi_file_count += 1

        q1 = prob
        leakage = classify_leakage(q1, gold_files)
        leakage_counts[leakage] += 1

        q2 = build_q2(prob)
        q3 = build_q3(prob)

        processed.append({
            "instance_id": instance_id,
            "repo": repo,
            "base_commit": base_commit,
            "created_at": r.get("created_at"),
            "expected_files": gold_files,
            "q1": q1,
            "q2": q2,
            "q3": q3,
            "leakage_class": leakage,
            "stratified_120": instance_id in stratified_120_ids,
        })

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(processed, f, indent=2)

    print(f"Wrote {len(processed)} instances to {args.out}")
    print(f"Single-file: {single_file_count}, Multi-file: {multi_file_count}")
    print(f"Stratified-120 count: {len(stratified_120_ids)}")
    print(f"Leakage classes: {dict(leakage_counts)}")

    if args.clone_repos and args.repos_cache:
        print(f"Cloning repositories into {args.repos_cache}...")
        for repo in sorted_repos:
            clone_repo(repo, args.repos_cache)
        print("All repositories cloned successfully.")


if __name__ == "__main__":
    main()
