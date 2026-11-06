"""DRAG-48: contextual-retrieval prefixes per chunk, written by a model through the `agy`
CLI (one call per file: the whole document and its chunks, Anthropic's contextual
retrieval prompt adapted to a batch), cached per file.

    loom-eval export-chunks <root> --index-dir DIR --out chunks.jsonl
    python3 -I eval/contextual/gen.py <root> chunks.jsonl contexts.json <cache_dir> [model] [workers]
    loom-eval quality <root> <golden…> --index-dir FRESH_DIR --contexts contexts.json --json B.json
"""

import concurrent.futures as cf
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

root, chunks_path, out_path, cache = sys.argv[1:5]
model = sys.argv[5] if len(sys.argv) > 5 else "gemini-3.8-flash-medium"
workers = int(sys.argv[6]) if len(sys.argv) > 6 else 6
os.makedirs(cache, exist_ok=True)
DOC_CHARS = 20_000
PROMPT = """Do not use any tools. Answer only with a JSON object.

<document path="{path}">
{doc}
</document>

Here are chunks of this document, each with an id:
{chunks}

For each chunk, write a short succinct context (one or two sentences, at most 60 words) that situates the chunk within the overall document, for the purposes of improving search retrieval of the chunk: what component it belongs to, what it does in that file, and the key names a developer would search for. Answer with a JSON object mapping each chunk id to its context, nothing else."""

files = [json.loads(line) for line in Path(chunks_path).read_text().splitlines()]
files = [
    f
    for f in files
    if not f["path"].startswith("~") and os.path.exists(os.path.join(root, f["path"]))
]
stats = {
    "calls": 0,
    "cached": 0,
    "failed": 0,
    "skipped": 0,
    "doc_chars": 0,
    "chunk_chars": 0,
    "out_chars": 0,
}


def work(f):
    path = os.path.join(root, f["path"])
    doc = Path(path).read_text(encoding="utf-8", errors="replace")
    if len(f["chunks"]) < 2 and len(doc) < 2000:
        return "skipped", {}
    doc = doc[:DOC_CHARS]
    key = hashlib.sha256(
        (
            model + "\0" + doc + "\0" + json.dumps([c["id"] for c in f["chunks"]])
        ).encode()
    ).hexdigest()[:24]
    cp = os.path.join(cache, key + ".json")
    if os.path.exists(cp):
        return "cached", json.loads(Path(cp).read_text())
    chunks = "\n".join(
        f'<chunk id="{c["id"]}" lines="{c["start_line"]}-{c["end_line"]}">\n{c["content"][:4000]}\n</chunk>'
        for c in f["chunks"]
    )
    prompt = PROMPT.format(path=f["path"], doc=doc, chunks=chunks)
    for attempt in range(3):
        try:
            r = subprocess.run(
                [
                    "agy",
                    "--model",
                    model,
                    "--disable-slash-commands",
                    "--print-timeout",
                    "170s",
                    "-p=" + prompt,
                ],
                capture_output=True,
                text=True,
                check=False,
                timeout=180,
                cwd=cache,
            )
            m = re.search(r"\{.*\}", r.stdout, re.DOTALL)
            if m:
                ctx = {
                    k: v
                    for k, v in json.loads(m.group(0)).items()
                    if isinstance(v, str)
                }
                ids = {c["id"] for c in f["chunks"]}
                ctx = {k: v.strip() for k, v in ctx.items() if k in ids}
                if ctx:
                    Path(cp).write_text(json.dumps(ctx))
                    stats["doc_chars"] += len(doc)
                    stats["chunk_chars"] += len(chunks)
                    stats["out_chars"] += sum(map(len, ctx.values()))
                    return "calls", ctx
            if "quota" in (r.stdout + r.stderr).lower():
                print("QUOTA:", (r.stdout + r.stderr)[-300:], file=sys.stderr)
                return "failed", {}
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError) as e:
            print("error", f["path"], e, file=sys.stderr)
        time.sleep(2 + attempt * 5)
    return "failed", {}


contexts = {}
t = time.time()
with cf.ThreadPoolExecutor(workers) as ex:
    for i, (kind, ctx) in enumerate(ex.map(work, files)):
        stats[kind] += 1
        contexts.update(ctx)
        if i % 5 == 0:
            print(
                f"{i}/{len(files)} {stats} {time.time() - t:.0f}s",
                file=sys.stderr,
                flush=True,
            )
Path(out_path).write_text(json.dumps(contexts))
stats["contexts"] = len(contexts)
stats["seconds"] = round(time.time() - t)
print(json.dumps(stats))
