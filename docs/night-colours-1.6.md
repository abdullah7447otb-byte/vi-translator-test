# Night colours: before and after (1.6.0)

The test is a dark ramp as the PS5 sends it: studio range twice over, luma 30 (black) to 62 across 1024 px.
It goes through the display shader with the "Dark scenes" look (gamma 1.25, shadow lift 0.6), drawn in
headless Chromium (WebGL2). The numbers are the largest jump from one pixel to the next along the ramp
(averaged over 8 rows, what the eye sees as a band edge), and the black pixels clipped at its dark end.

| Pipeline | Largest step (levels) | Black clipped (px of the first 40) |
| --- | --- | --- |
| 1.5.0: integer stretch, then the look | 7 | 16 |
| 1.6.0: one float transform, dither | 4.9 | 0 |
| 1.6.0 + deband 0.4 (the default) | 1.25 | 0 |
| 1.6.0 + deband 1.0, detail in the dark 0.5 | 1.5 | 0 |

The ramp itself has 32 levels over 1024 px, so some step is always there. Below 2 levels, bands don't
show.
