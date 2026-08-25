# Overlay files

These committed files are symlinked into the ignored Cronet-only GN view. The
native build never generates compatibility source text.

- `common/` is installed for every target.
- `android/`, `ios/`, and `ohos/` are installed only for that platform.
- Small C/C++ wrappers include an explicitly named upstream alias and confine
  macro changes to one translation unit.
- Link-time adapters and new C ABI are under `crates/tokio-cronet-sys/native`.
- Full replacements remain only where a wrapper cannot act early enough: GN
  configuration, Android's Java Looper bridge, and an upstream template
  compatibility definition.

When bumping the pinned Chromium revision, refresh only those unavoidable
replacement snapshots from the new tree:

```sh
python3 xtask/overlays/generate.py
```

Then review the resulting diff. Wrapper and cronet-rs-owned files are never
produced by that script.
