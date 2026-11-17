"""Keep the chosen note pairs, envelopes and embedded WAV assets in sync."""

import importlib.util
import io
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import wave


ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts/generate-notification-sounds.py"
spec = importlib.util.spec_from_file_location("notification_sounds", GENERATOR)
sounds = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sounds)


def samples(name):
    with wave.open(io.BytesIO(sounds.render(name)), "rb") as wav:
        return struct.unpack("<%dh" % wav.getnframes(), wav.readframes(wav.getnframes()))


class NotificationSounds(unittest.TestCase):
    def test_checked_in_assets_match_generator(self):
        for name in sounds.CHIMES:
            with self.subTest(name=name):
                self.assertEqual(
                    (ROOT / "crates/ui/assets/sounds" / (name + ".wav")).read_bytes(),
                    sounds.render(name),
                )

    def test_pcm_format_duration_and_quiet_endpoints(self):
        for name, duration in [("done", 0.39), ("request", 0.40)]:
            with self.subTest(name=name):
                with wave.open(io.BytesIO(sounds.render(name)), "rb") as wav:
                    self.assertEqual(wav.getnchannels(), 1)
                    self.assertEqual(wav.getsampwidth(), 2)
                    self.assertEqual(wav.getframerate(), 44100)
                    self.assertEqual(wav.getnframes(), round(duration * 44100))
                pcm = samples(name)
                self.assertEqual(pcm[0], 0)
                self.assertTrue(all(value == 0 for value in pcm[-400:]))
                peak = max(abs(value) for value in pcm) / 32767
                self.assertGreater(peak, 0.06)
                self.assertLess(peak, 0.09)

    def test_rendered_note_order_matches_the_selected_pair(self):
        for name, spacing, frequencies in [
            ("done", 0.17, (783.99, 587.33)),
            ("request", 0.20, (659.25, 783.99)),
        ]:
            pcm = samples(name)
            for index, expected in enumerate(frequencies):
                with self.subTest(name=name, note=index):
                    start = round((index * spacing + 0.035) * 44100)
                    end = round((index * spacing + 0.105) * 44100)
                    segment = pcm[start:end]
                    crossings = sum(
                        left <= 0 < right
                        for left, right in zip(segment, segment[1:])
                    )
                    self.assertAlmostEqual(
                        crossings / ((end - start) / 44100), expected, delta=16
                    )

    def test_preview_attack_decay_and_gain_are_preserved(self):
        self.assertEqual(sounds.PREVIEW_GAIN, 0.35)
        for name, attack, duration, amplitude in [
            ("done", 0.008, 0.20, 0.24),
            ("request", 0.014, 0.18, 0.22),
        ]:
            with self.subTest(name=name):
                chime = sounds.CHIMES[name]
                self.assertEqual(chime["attack"], attack)
                self.assertEqual(chime["duration"], duration)
                self.assertEqual(chime["amplitude"], amplitude)
                self.assertEqual(sounds.envelope(-0.01, duration, amplitude, attack), 0)
                self.assertEqual(sounds.envelope(0, duration, amplitude, attack), 0)
                self.assertAlmostEqual(
                    sounds.envelope(attack, duration, amplitude, attack), amplitude
                )
                self.assertAlmostEqual(
                    sounds.envelope(duration, duration, amplitude, attack), 0.0001
                )

    def test_check_detects_drift_without_overwriting_it(self):
        with tempfile.TemporaryDirectory() as directory:
            command = [sys.executable, str(GENERATOR), "--output-dir", directory]
            subprocess.run(command, check=True, capture_output=True)
            subprocess.run(command + ["--check"], check=True, capture_output=True)
            path = Path(directory) / "request.wav"
            path.write_bytes(b"changed")
            check = subprocess.run(command + ["--check"], capture_output=True)
            self.assertNotEqual(check.returncode, 0)
            self.assertEqual(path.read_bytes(), b"changed")


if __name__ == "__main__":
    unittest.main()
