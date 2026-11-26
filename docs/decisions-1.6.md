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
