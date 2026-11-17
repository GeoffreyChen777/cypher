#!/usr/bin/env python3
"""Render the chosen Soft preview tones as embedded desktop notification WAVs.

Standard library only. Run with --check to verify the checked-in assets without
writing them. The 40 ms Web Audio scheduling lead-in is deliberately omitted.
"""

import argparse
import io
import math
from pathlib import Path
import struct
import wave


ROOT = Path(__file__).resolve().parents[1]
SAMPLE_RATE = 44_100
# Match the audition page's default volume, rather than peak-normalizing the
# files and making the shipped notifications much louder than the preview.
PREVIEW_GAIN = 0.35
TAIL = 0.02
CHIMES = {
    # Original Soft "needs input", now selected for completion.
    "done": {
        "frequencies": (783.99, 587.33),
        "spacing": 0.17,
        "duration": 0.20,
        "amplitude": 0.24,
        "attack": 0.008,
    },
    # Adjusted Soft "needs input": a gentler ascending pair.
    "request": {
        "frequencies": (659.25, 783.99),
        "spacing": 0.20,
        "duration": 0.18,
        "amplitude": 0.22,
        "attack": 0.014,
    },
}


def envelope(t, duration, amplitude, attack):
    """Web Audio linear attack, exponential decay to .0001, then silence."""
    if t < 0:
        return 0.0
    if t < attack:
        return amplitude * t / attack
    if t < duration:
        return amplitude * (0.0001 / amplitude) ** (
            (t - attack) / (duration - attack)
        )
    if t < duration + 0.01:
        return 0.0001
    return 0.0


def render(name):
    chime = CHIMES[name]
    length = chime["spacing"] + chime["duration"] + TAIL
    frames = bytearray()
    for frame in range(round(length * SAMPLE_RATE)):
        t = frame / SAMPLE_RATE
        sample = 0.0
        for index, frequency in enumerate(chime["frequencies"]):
            local_t = t - index * chime["spacing"]
            gain = envelope(
                local_t, chime["duration"], chime["amplitude"], chime["attack"]
            )
            sample += gain * math.sin(2 * math.pi * frequency * local_t)
        sample *= PREVIEW_GAIN
        if abs(sample) > 1:
            raise ValueError("Notification waveform would clip")
        frames.extend(struct.pack("<h", round(sample * 32767)))
    output = io.BytesIO()
    with wave.open(output, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(frames)
    return output.getvalue()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument(
        "--output-dir", type=Path, default=ROOT / "crates/ui/assets/sounds"
    )
    args = parser.parse_args()
    if not args.check:
        args.output_dir.mkdir(parents=True, exist_ok=True)
    mismatches = []
    for name in CHIMES:
        path = args.output_dir / (name + ".wav")
        data = render(name)
        if args.check:
            if not path.is_file() or path.read_bytes() != data:
                mismatches.append(str(path))
        else:
            path.write_bytes(data)
            print("Wrote", path)
    if mismatches:
        print("Notification assets need regeneration:", *mismatches, sep="\n")
        return 1
    if args.check:
        print("Notification assets match the selected preview tones.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
