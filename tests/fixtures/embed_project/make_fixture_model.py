# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["numpy>=1.26,<3", "safetensors>=0.4,<1", "tokenizers>=0.20,<1"]
# ///
"""Regenerate the tiny static-embedding model this fixture points `embed_project` at.

A real model (a `model2vec` / `potion` release) is tens of megabytes and arrives over
the network, which is neither of the things a test fixture may be. This writes the
same LAYOUT — `model.safetensors` holding one [vocab, dim] float tensor under the key
`embeddings`, beside a `tokenizer.json` — over the fixture corpus's own vocabulary.

The vectors are not random noise. `corpus.csv` holds two subjects, a working harbour
(the first 24 rows) and company results (the rest); each word takes the direction of
the subject it appears in, both directions when it appears in both, plus a small
seeded jitter. So the fixture behaves like a real model in the one way the test cares
about: the map it produces has structure, and that structure comes from the text.

    uv run tests/fixtures/embed_project/make_fixture_model.py

Deterministic — re-running rewrites byte-identical files.
"""
from __future__ import annotations

import csv
import pathlib

import numpy as np
from safetensors.numpy import save_file
from tokenizers import Tokenizer, models, normalizers, pre_tokenizers

HERE = pathlib.Path(__file__).parent
DIM = 16
SEED = 7
HARBOUR_ROWS = 24  # corpus.csv rows 1..24 are the harbour subject


def words(text: str) -> list[str]:
    return [w for w in "".join(c if c.isalnum() else " " for c in text.lower()).split() if w]


def main() -> None:
    subjects: dict[str, set[int]] = {}
    with (HERE / "corpus.csv").open(newline="", encoding="utf-8") as fh:
        for row in csv.DictReader(fh):
            subject = 0 if int(row["id"]) <= HARBOUR_ROWS else 1
            for word in words(row["title"]) + words(row["description"]):
                subjects.setdefault(word, set()).add(subject)

    vocab = ["[UNK]"] + sorted(subjects)
    rng = np.random.default_rng(SEED)
    directions = rng.normal(size=(2, DIM)).astype(np.float32)
    directions /= np.linalg.norm(directions, axis=1, keepdims=True)

    table = np.zeros((len(vocab), DIM), dtype=np.float32)
    for index, word in enumerate(vocab):
        if word == "[UNK]":
            continue
        table[index] = directions[sorted(subjects[word])].mean(axis=0)
    table += rng.normal(scale=0.15, size=table.shape).astype(np.float32)
    table /= np.linalg.norm(table, axis=1, keepdims=True)

    model_dir = HERE / "model"
    model_dir.mkdir(exist_ok=True)
    save_file({"embeddings": table}, model_dir / "model.safetensors")

    tokenizer = Tokenizer(models.WordLevel(vocab={w: i for i, w in enumerate(vocab)}, unk_token="[UNK]"))
    tokenizer.normalizer = normalizers.Sequence([normalizers.NFD(), normalizers.Lowercase(), normalizers.StripAccents()])
    tokenizer.pre_tokenizer = pre_tokenizers.Whitespace()
    tokenizer.save(str(model_dir / "tokenizer.json"), pretty=True)

    print(f"{len(vocab)} tokens x {DIM} dims -> {model_dir}")


if __name__ == "__main__":
    main()
