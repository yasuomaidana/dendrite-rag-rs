"""Materializes a CoIR retrieval dataset as a corpus directory and a golden set for
`loom-eval quality` (DRAG-52): one file per corpus document, the test queries with their
relevant documents as expected files. Reads the dataset's Parquet files from the Hugging
Face Hub with pyarrow, which `uv` provides for the run:

    uv run --no-project --with pyarrow python -I eval/coir/import.py cosqa /tmp/coir/cosqa --ext py --queries 500
    loom-eval quality /tmp/coir/cosqa /tmp/coir/cosqa.golden.json --index-dir /tmp/coir/cosqa-idx

Documents land in `<out>/docs/<id>.<ext>`; the golden set is written beside the corpus
(`<out>.golden.json`) so it is not indexed with it.
"""

import argparse
import io
import json
import os
import urllib.request

HUB = "https://huggingface.co"


def parquet_rows(dataset, prefix):
    """Every row of the dataset's Parquet files whose name starts with `prefix`
    (`corpus/`, `queries/`, `data/test`)."""
    import pyarrow.parquet as pq

    with urllib.request.urlopen(f"{HUB}/api/datasets/{dataset}", timeout=60) as r:
        files = [s["rfilename"] for s in json.load(r)["siblings"]]
    for name in sorted(
        f for f in files if f.startswith(prefix) and f.endswith(".parquet")
    ):
        with urllib.request.urlopen(
            f"{HUB}/datasets/{dataset}/resolve/main/{name}", timeout=600
        ) as r:
            table = pq.read_table(io.BytesIO(r.read()))
        yield from table.to_pylist()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("name", help="CoIR dataset name, e.g. cosqa")
    p.add_argument("out", help="corpus directory to create")
    p.add_argument("--ext", default="py", help="file extension of corpus documents")
    p.add_argument("--queries", type=int, default=500, help="test queries to keep")
    a = p.parse_args()
    dataset = f"CoIR-Retrieval/{a.name}"

    qrels = {}
    for r in parquet_rows(dataset, "data/test"):
        if r.get("score", 1) > 0:
            qrels.setdefault(r["query-id"], []).append(r["corpus-id"])
    wanted = dict(list(qrels.items())[: a.queries])
    queries = {
        r["_id"]: r["text"]
        for r in parquet_rows(dataset, "queries/")
        if r["_id"] in wanted
    }

    docs = os.path.join(a.out, "docs")
    os.makedirs(docs, exist_ok=True)
    n = 0
    for r in parquet_rows(dataset, "corpus/"):
        with open(
            os.path.join(docs, f"{r['_id']}.{a.ext}"), "w", encoding="utf-8"
        ) as f:
            f.write(r["text"])
        n += 1

    golden = {
        "corpus": f"CoIR {a.name} ({dataset}), {n} documents, one file each",
        "note": "Imported by eval/coir/import.py; expected files are the test qrels.",
        "queries": [
            {"q": queries[qid], "expect": [f"docs/{d}.{a.ext}" for d in ds]}
            for qid, ds in wanted.items()
            if qid in queries
        ],
    }
    with open(a.out.rstrip("/") + ".golden.json", "w", encoding="utf-8") as f:
        json.dump(golden, f, indent=1)
    print(f"{n} documents, {len(golden['queries'])} queries in {a.out}")


if __name__ == "__main__":
    main()
