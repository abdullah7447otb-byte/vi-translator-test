# Barqawi 1.7: decisions

The choices made for 1.7 and why, next to `docs/decisions-1.6.md`.

1. **Mac only.** Every change was made and checked for macOS. Windows builds from the same commit get the
   shared changes (core, UI), with no Windows-specific work and no Windows tuning.
2. **The character setting is gone.** The owner plays both Jason and Lucia and switches often. Instructions use
   the general form every Arabic game uses («اضغط»). Subtitles still take the gender from the context in the
   language models' prompt. An old settings file's `character` is read and ignored.
3. **Words placed by their ink** (`ocr::words_placed_by_ink`), in the engine rather than in the GTA VI script.
   When Vision gives a line's words one shared box, each word gets the line's own runs of ink, so every page
   script benefits. It acts only when the boxes overlap.
4. **The tab-row splitter is limited to the hub** (Map, Brief, Progress, Jason & Lucia). On the boxed Settings
   and Help rows, with an icon at each end and a tab cut by the fade, matching by widths put a name one box off
   now and then, and Vision reads those rows well on its own.
5. **Page scripts' word lists are built at load**, not in the first read. Built in the first read, they pushed
   it past its time limit now and then, and the first menu opened showed nothing.
6. **`beside` pages.** A small card next to the game (the WAiNK car card) keeps the read areas and the phone
   running. Pages stay full screen by default.
7. **The heavier picture looks in two passes** (sharpening, dark detail): the frame is turned to half-float RGB
   once, then the looks run on it. Debanding alone stays one pass, where two would cost more than they save.
   Without float pictures to draw into, the app keeps one pass.
8. **Barqawi's own copy of chiaki's `takion.c`** (`ps/native/takion.c`), built instead of the submodule's.
   It sets a 4 MB socket receive buffer and user-interactive QoS for the receive thread on macOS. The window
   advertised to the console is unchanged.
9. **Motion smoothing is off by default.** It shows the picture one frame later (about 17 ms) plus its own
   time. VideoToolbox's classes are looked up at run time, so any Xcode builds the app and Macs before macOS 26
   simply don't offer the switch. It turns itself off when slower than 12 ms a frame, or after 30 failures.
10. **The network check only reads.** It uses `ifconfig` and `networksetup` and gives advice once per session.
    Nothing runs as root, and AWDL is not turned off by the app.
11. **Whole-line glossary entries apply to every translator** for Arabic: a phone button reads the same with
    Apple, Claude or ChatGPT. A name a language model leaves in English is replaced; a name it spells its own way
    is logged, not retried, so the line isn't delayed.
12. **Stream mode keeps what it changed** in `stream_saved` and restores exactly that.
