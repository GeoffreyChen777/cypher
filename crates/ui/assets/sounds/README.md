# Desktop notification sounds

Original synthesized tones, selected from Cypher's **Soft** audition on
2026-11-17. No Apple or third-party recordings are included.

| Event | Selected audition | Notes | Note spacing | Decay endpoint | Attack | Peak envelope |
| --- | --- | --- | --- | --- | --- | --- |
| Done | Original “needs input” | G5 → D5 (783.99 → 587.33 Hz) | 170 ms | 200 ms | 8 ms | 0.24 |
| Request | Adjusted “needs input” | E5 → G5 (659.25 → 783.99 Hz) | 200 ms | 180 ms | 14 ms | 0.22 |

Each note is a sine wave, with a linear attack and exponential decay to 0.0001,
silenced 10 ms after the decay endpoint. Each oscillator starts at zero phase.
The mix uses the audition's default **35% master gain**, with no additional
normalization. These assets are 44.1 kHz mono, 16-bit PCM WAV; the final files
are 390 ms and 400 ms respectively. The browser's 40 ms scheduling lead-in is
not part of the audio.

Regenerate or verify from the repository root:

```sh
python3 scripts/generate-notification-sounds.py
python3 scripts/generate-notification-sounds.py --check
python3 -m unittest discover -s scripts/ci -p 'test_notification_sounds.py'
```

The generator uses only Python's standard library. The UI embeds the WAV files;
no synthesizer runs at app startup. The session eligibility rules remain
unchanged: child agents and internal/background threads do not independently
trigger these sounds.
