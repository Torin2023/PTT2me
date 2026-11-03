#!/usr/bin/env python3
"""Prepare a fixed 16-utterance Russian FLEURS test subset, entirely offline.

Inputs are pinned public CC BY 4.0 data. No model, microphone, or network access.
Output contains licensed reference text/audio and must remain outside Git.
"""
import argparse
from contextlib import contextmanager
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import stat
import tarfile
import tempfile
import wave

REVISION = "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
ARCHIVE_SHA256 = "8a0d6a0d23c3421f50c575bcf65d875cd19dadae2f7cabb415024f81b65178b1"
TSV_SHA256 = "cd54f261220f49afbb4c128633a737eca4a22f6c0a8233d3cc891478d06676e6"
ATTRIBUTION = "Google FLEURS, Conneau et al. (2022), ru_ru/test, CC BY 4.0"


@contextmanager
def verified_input(path, digest, limit):
    # Parse exactly the bytes that were verified. Private unlinked temporary
    # storage also isolates a concurrent rewrite of the original inode.
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit:
            raise ValueError("input must be a bounded regular non-symlink file")
        with tempfile.TemporaryFile(mode="w+b") as snapshot:
            hasher, count = hashlib.sha256(), 0
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                count += len(block)
                if count > limit:
                    raise ValueError("input exceeds pinned size limit")
                hasher.update(block)
                snapshot.write(block)
            if count != metadata.st_size or hasher.hexdigest() != digest:
                raise ValueError("pinned corpus input SHA-256 mismatch")
            snapshot.seek(0)
            yield snapshot


def select_rows(data):
    selected, sentences = {}, set()
    for row in csv.reader(io.StringIO(data), delimiter="\t"):
        if len(row) != 7 or not re.fullmatch(r"[0-9]+\.wav", row[1]):
            raise ValueError("unexpected pinned TSV schema")
        frames = int(row[5])
        if row[0] in sentences or not 16_000 <= frames <= 400_000:
            continue
        selected[row[1]] = {"sentence_id": row[0], "reference": row[2], "frames": frames}
        sentences.add(row[0])
        if len(selected) == 16:
            return selected
    raise ValueError("fewer than 16 distinct eligible sentences")


def prepare(archive, tsv, output):
    with verified_input(tsv, TSV_SHA256, 735_258) as metadata:
        rows = select_rows(metadata.read().decode("utf-8"))
    with verified_input(archive, ARCHIVE_SHA256, 433_142_634) as archive_snapshot:
        prepare_verified(archive_snapshot, rows, output)


def prepare_verified(archive_snapshot, rows, output):
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    manifest = []
    converted = set()
    with tempfile.TemporaryDirectory(prefix="fleurs-pcm-") as temporary:
        with tarfile.open(fileobj=archive_snapshot, mode="r|gz") as source:
            for member in source:
                filename = Path(member.name).name
                if filename not in rows:
                    continue
                if filename in converted or not member.isfile() or not 0 < member.size <= 4_000_000:
                    raise ValueError("unexpected selected archive member")
                # Never extract archive paths, links, permissions, or metadata.
                stream = source.extractfile(member)
                if stream is None:
                    raise ValueError("selected audio is missing")
                data = stream.read(4_000_001)
                if len(data) != member.size:
                    raise ValueError("selected audio size mismatch")
                raw = Path(temporary) / "input.wav"
                raw.write_bytes(data)
                destination = output / filename
                subprocess.run(["/usr/bin/afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1",
                                str(raw), str(destination)], check=True, capture_output=True)
                with wave.open(str(destination), "rb") as wav:
                    if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth(), wav.getnframes()) != (
                            16_000, 1, 2, rows[filename]["frames"]):
                        raise ValueError("converted audio format/frame count mismatch")
                manifest.append({
                    "id": "fleurs-ru-" + filename[:-4], "wav": filename,
                    "reference": rows[filename]["reference"],
                    "duration_seconds": rows[filename]["frames"] / 16_000,
                    "source": ATTRIBUTION,
                    "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
                    "frames": rows[filename]["frames"], "format": "mono 16000 Hz PCM16 WAV",
                })
                converted.add(filename)
    if converted != set(rows):
        raise ValueError("archive is missing selected utterances")
    manifest.sort(key=lambda case: case["id"])
    (output / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    (output / "provenance.json").write_text(json.dumps({
        "source": "https://huggingface.co/datasets/google/fleurs", "revision": REVISION,
        "license": "https://creativecommons.org/licenses/by/4.0/", "attribution": ATTRIBUTION,
        "archive_sha256": ARCHIVE_SHA256, "tsv_sha256": TSV_SHA256,
        "selection": "first 16 distinct sentence IDs in pinned test.tsv with 1..25 seconds; first eligible recording per ID",
        "conversion": "afconvert float WAV to mono 16000 Hz PCM16, no trimming, normalization or denoising",
        "cases": [{"file": name, "sentence_id": value["sentence_id"]} for name, value in rows.items()],
    }, indent=2) + "\n")
    print(f"Prepared {len(manifest)} licensed Russian test utterances; references/audio remain in output directory")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--tsv", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new directory outside Git; parent must exist")
    args = parser.parse_args()
    try:
        prepare(args.archive, args.tsv, args.output)
    except (OSError, ValueError, tarfile.TarError, subprocess.CalledProcessError) as error:
        # Native stderr may contain paths. Do not echo it or corpus text.
        raise SystemExit(f"Corpus preparation failed ({type(error).__name__}); no benchmark was run") from None
