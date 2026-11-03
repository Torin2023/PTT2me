"""No recordings, network, models or Apple audio tools are used by these tests."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "fleurs_corpus", Path(__file__).resolve().parents[1] / "scripts/prepare-fleurs-corpus.py")
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)


def row(identifier, filename, frames):
    return f"{identifier}\t{filename}.wav\tfixture text\tfixture text\tunused\t{frames}\tunused\n"


class CorpusSelectionTests(unittest.TestCase):
    def test_selection_uses_first_eligible_recording_per_distinct_sentence(self):
        data = row(0, 1, 15999) + row(0, 2, 16000) + row(0, 3, 32000)
        data += row(1, 4, 400001) + row(1, 5, 400000)
        data += "".join(row(i, 100+i, 16000) for i in range(2, 30))
        selected = corpus.select_rows(data)
        self.assertEqual(len(selected), 16)
        self.assertEqual(list(selected)[:2], ["2.wav", "5.wav"])
        self.assertEqual([v["sentence_id"] for v in selected.values()], list(map(str, range(16))))

    def test_short_or_malformed_inputs_fail(self):
        for data in [row(1, 1, 16000), "1\t../secret.wav\tx\tx\tx\t16000\tx\n", "wrong schema"]:
            with self.assertRaises(ValueError):
                corpus.select_rows(data)

    def test_verification_rejects_changed_or_symlinked_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input"
            path.write_bytes(b"public fixture")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            corpus.verify(path, digest)
            alias = Path(directory) / "alias"
            alias.symlink_to(path)
            with self.assertRaises(ValueError):
                corpus.verify(alias, digest)
            path.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                corpus.verify(path, digest)

    def test_bad_archive_never_creates_output(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input"
            path.write_bytes(b"not pinned archive")
            output = Path(directory) / "output"
            with self.assertRaises(ValueError):
                corpus.prepare(path, path, output)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
