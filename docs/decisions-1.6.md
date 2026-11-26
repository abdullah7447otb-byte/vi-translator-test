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
