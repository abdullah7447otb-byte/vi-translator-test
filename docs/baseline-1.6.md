# Baseline before 1.6.0 (branch test-faster-ps5 at bdc3b57, version 1.5.0)

Measured on the Linux dev shell, 2026-11-26.

| Check | Result |
| --- | --- |
| `cargo test -p vi-translator-core --release` | 174 passed, 2 ignored, 0 failed |
| `cargo test -p vi-translator-ps --release` | 12 passed, 1 ignored, 0 failed |
| i18n missing-strings check (`tools/ui_mock/missing.js`) | 4, all false positives: index.html keys that contain markup (`<b>…</b>`), whose Arabic exists under the full key |

## Help box anchor (finding 3), measured on the code

Synthetic two-line help text at the GTA VI help box's place (1080p), moved up or down:

| Shift | Result |
| --- | --- |
| −30 px | first line lost (only the second line read) |
| −15 … +30 px | read |
| +50 px and more | dropped |

`reads_as_sentences`: "Press L1 to run." yes; "Hold R2 Accelerate", "Aim", "Use the phone" no (dropped).

## Not measurable here

Vision, VideoToolbox, the Translation framework and the native window exist only on the macOS CI runner
(Intel, no GPU). Their numbers come from CI in the release run.
