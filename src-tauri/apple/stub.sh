#!/bin/sh
# Stand-in for vit-apple-translate when the build machine's SDK had no Translation framework (macOS 26
# SDK needed): the app then offers no Apple engine.
if [ "$1" = "--status" ]; then echo "no-os"; exit 0; fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p')
  printf '{"id":%s,"error":"built without Apple translation"}\n' "${id:-0}"
done
