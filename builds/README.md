# MRLess app builds

Artifacts from `scripts/build.sh` and `scripts/deploy-ios.sh` land here.

```
builds/
  ios/
    latest/          → current iOS .app (symlink or copy)
    2026-10-13_1815/ → timestamped snapshot
  macos/
    latest/
    …
```

Do not commit large binaries; this folder is gitignored except this README.
