# VI Translator 1.5.0 in Barqawi

VI Translator Desktop 1.5.0 (tag `v1.5.0`, 2026-11-27, by Fouad Almalki) merged into Barqawi
1.6.1 on the branch `barqawi-upstream-1.5`, for the next Barqawi (1.6.2). Not published yet.

## The fixes (all five)

| Fix | Upstream | In Barqawi |
| --- | --- | --- |
| PS5 Unlink did nothing on macOS (`window.confirm` does nothing in its webview) | e5a7be8 | Unlink asks in the page (`ask`) |
| Overlays on a Retina screen at half their place and size | e2d60f0 | `overlay::place` in points on macOS |
| A PS5 put into rest mode woken again after its stream ended | 93894de | `may_wake`: it waits for Wake or its controller |
| A subtitle's round icon read as letters ("0000", "cocoa pooo") | 4cc5063 | `ocr::drop_icon_misreads` |
| A short dropout ended a subtitle (100 ms) | e63e974 | `DROPOUT_FLOOR_MS` 250 ms, instructions 350 ms |

## The features

- Translator: «Use VI Translator's ready translations first» (5265bcb).
- Game pages: what's translated, part by part (phone, pause menu and map parts, help and notices,
  dialogue only), none covering another (5265bcb, e7ee88f; `core/pages/gta6.js` pages-version 12,
  upstream's own file; Barqawi's `gta6-plus.js` still loads after it).
- Each output switched on or off while translating: this computer's overlays, each TV, OBS (320cd71).
- Full screen: the pointer and the buttons (Barqawi's picture, fix and check-report ones too) hide
  after a moment without moving the mouse; F11 (ef7b463).
- Sliders: the number, the dot and the saved value agree (6a1ff84).
- The player's own profiles keep their names and fonts in any script (4a88bd7).
- Arabic: the Saudi dialect and the player's own notes for Claude and ChatGPT (f5281a5, 1387f0c).
- Xbox Remote Play inside the app (eeb4a46; new `xbox/` crate).
- TVs found on every home network the computer is on (f871d76); Windows: HDR colours, no yellow
  border, 4K capture cards (54e1240, bfe7b57, ed70737); phones' reading pace (dd6b8bd).

## How it was merged

- Conflicts kept both sides: Barqawi's ChatGPT server address, phone panel, character, performance
  floor, GTA VI cast and glossary; upstream's dialect, timing defaults, notes and ready translations.
  The translation cache key holds the notes, the line's context and the glossary.
- Barqawi's rules applied to the new code: every lock poison-safe (`unwrap_or_else(PoisonError::into_inner)`),
  logging through `vlog!`. Four one-shot threads of upstream's stay plain `std::thread::spawn`, as
  older code has.
- The Xbox picture goes to the preview as YUY2 with normal levels (`layout: Yuy2, stretch: false`).
- Settings migrations don't collide: upstream's stayed at 7, Barqawi's is 8.

## Checked (Linux dev shell)

- `cargo check --workspace --all-targets`; core, PS5 and Xbox tests: all pass (core 203).
- The interface: no new string without Arabic; the play view's five buttons, same size, the check
  report pressed (check mode on, the report, check mode off).
- Night bands 1.00 / 1.25 levels, black clipped 0; phone panel renders.
- Still to run before publishing: the full Test build on macOS and Windows.
