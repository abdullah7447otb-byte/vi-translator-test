# Decisions taken while building 1.6.0 (the owner was away)

Each choice that was open, what was chosen, and why.

1. **HEVC is an option, H.264 stays the default.** HEVC can only be tried on the CI Mac (Intel, a clip
   made with ffmpeg), not with a real PS5 stream. A wrong default would leave the owner without a picture.
   HEVC falls back to H.264 by itself, so it is safe to try from the setting.
2. **`core/pages/gta6.js` is not edited.** New page logic goes in `core/pages/gta6-plus.js`, run after the
   base script.
3. **The hardware decoder gives up only on real failures**: 3 key frames in a row, or 60 frames in a row.
   Frames broken on the network go on to chiaki's corrupt-frame report as before. Waiting for a key frame
   after every broken frame would freeze the picture often, because the PS5 recovers with reference
   invalidation, not key frames.
4. **The character played (Jason or Lucia) is a setting, Jason by default.** Telling it from the picture
   needs real frames of a character switch, which we don't have. The instructions' Arabic is made
   feminine for Lucia by a closed list of the game's imperative verbs: a word not in the list is never
   guessed. «قد» is changed only before where to drive («قد إلى…»), never the particle («قد يكون»).
5. **Two new reading areas (notifications bottom left, prompts on the right), only strict text.** The
   notifications need the game's box around them. The prompts need a button icon. Both take reviewed
   lines or a new line that holds steady and reads as English. Their exact place in GTA VI is from
   gameplay descriptions, not frames: the check mode shows what they read and drop, so they can be tuned.
6. **Shared memory (two Mac users) is off by default.** It lives in /Users/Shared and takes effect after a
   restart. The memory files are append-only lines, so two players writing at once don't corrupt them.
7. **The bigger offline model (opus-mt-tc-big-en-ar) is not in 1.6.0.** It has no ONNX build anywhere we
   could check: Hugging Face is blocked here, and no ONNX copy shows in search. Making one needs an
   export pipeline, a place to host about 250 MB, and quality and speed checks, and none of these can be
   checked here. The offline translator gets a warm start instead, so the first line isn't slow, plus the
   name glossary and the instant memory path it already had. Apple's translation is the stronger one on
   macOS 26 and it heals itself now.
8. **The phone's Arabic defaults to the steady panel.** Following the phone's sway is what made it hard
   to read. «On the phone» stays an option, and so does «Both». The in-place drawing is unchanged (no new
   dead-zone): changing its easing without real phone frames on a Mac could make it lag the phone.
9. **No native video layer (AVSampleBufferDisplayLayer) in 1.6.0.** Putting a native layer under Tauri's
   web view can only be checked on a real Mac with a display. The CI Mac has no GPU and no screen, so a
   mistake there would leave the owner with a black picture. The current path already draws each frame
   at the screen's refresh (requestAnimationFrame, a queue of one or two frames). The gains in 1.6.0 come
   from HEVC, the float colour path and the decoder not dropping to the CPU.
10. **No FSR upscaling in 1.6.0.** The owner's OBS setup used a 1920×1080 canvas, so his monitor is most
    likely 1080p: the stream already fills it pixel for pixel, and an upscaler would add nothing.
11. **Keyboard shortcuts instead of controller combos.** The controller's buttons go to the game. Taking
    combos out of them without a real PS5 to test on could swallow a move in the middle of a fight. In
    the app window the shortcuts are: Ctrl/⌘+Shift+E to correct a line, Ctrl/⌘+Shift+D for the check
    mode, Ctrl/⌘+Shift+L to switch Jason/Lucia.
12. **The check mode is off at every start and not saved.** The owner streams the Barqawi window through
    OBS. A check mode left on would show in the stream, so it never comes back on by itself.
13. **Left out of 1.6.0: profanity softening, a separate mission-log page.** The live list (the iPad page)
    and the SRT export already keep every line, missions included. A softening list for Arabic insults
    needs care to avoid changing meaning, and is better done with the owner's choices.
14. **Map: the objective card in any colour; the left details panel and menu "erase and redraw" left
    out.** The card's shape is known (core/pages/gta6.js reads it in pink), so `gta6-plus.js` tries the
    other colours with the same reading. The mission details panel on the left of the map, and erasing
    the English in menus, need real frames of those screens to tune. A wrong guess there shows half-erased
    or misplaced text, which the owner wants least of all. The check mode and check report will bring the
    frames needed to do them next.
