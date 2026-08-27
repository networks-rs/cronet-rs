#!/usr/bin/env python3
"""Cronet-only wrapper around Chromium's Visual Studio toolchain helper."""

import importlib.util
import os
import sys


def _load_upstream():
    overlay_dir = os.path.dirname(os.path.abspath(__file__))
    if overlay_dir not in sys.path:
        sys.path.insert(0, overlay_dir)
    path = os.path.join(overlay_dir, "vs_toolchain_upstream.py")
    spec = importlib.util.spec_from_file_location("cronet_rs_vs_toolchain", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed to load Chromium toolchain helper from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_UPSTREAM = _load_upstream()


def __getattr__(name):
    return getattr(_UPSTREAM, name)


def main():
    if sys.argv[1:2] == ["copy_dlls"]:
        # Chromium copies SDK debugger DLLs for isolates and installers. Cronet
        # only produces libraries, so retain the VC runtime copy while avoiding
        # an optional x64 Debugging Tools dependency on native Windows ARM.
        _UPSTREAM._CopyDebugger = lambda _target_dir, _target_cpu: None
    return _UPSTREAM.main()


if __name__ == "__main__":
    sys.exit(main())
