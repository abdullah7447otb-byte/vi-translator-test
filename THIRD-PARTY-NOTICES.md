# Third-party notices

This program is free software under the GNU Affero General Public License version 3 (see LICENSE).
Parts of it come from, or are ported from, the projects below. Their notices are kept here.

## CouchSubs

- Copyright (C) 2026 Sinan Yorulmaz.
- Source: https://github.com/yorulmazsinan/couchsubs
- License: GNU Affero General Public License version 3.
- Ported from commit `bc54ff70256812747189421ee9bde3aea33ba5bf`, Swift to Rust and JavaScript. Each ported file names its source file in its header:
  - the phone finder and tracker (`core/src/phone_pose.rs`);
  - the screen anchor;
  - the in-place phone drawing;
  - the phone chat rules.
- Its real-frame regression set (`testdata/frames`, screenshots of gameplay videos) is used for tests only. It is downloaded at build time and is not part of this repository.

## chiaki-ng

- Source: `third-party/chiaki-ng`.
- License: GNU Affero General Public License version 3. See its own COPYING file.

## Mbed TLS

- Source: `third-party/mbedtls`.
- License: Apache-2.0 or GPL-2.0-or-later. See its own LICENSE file.

## GTA VI Landmarks

- Source: map.gtadb.org, as listed on gta6map.city.
- License: Creative Commons Attribution 4.0 (CC BY 4.0).
- Place names only, in `core/src/glossary_ar.tsv` and `core/src/glossary_ar_lines.tsv`. The Arabic for each name is ours.
