#!/usr/bin/env python3
"""Renders icon.svg to Spotui.icns (needs rsvg-convert). Run from this directory."""
import struct
import subprocess

# ICNS entry type -> pixel size (all PNG payloads)
ENTRIES = [
    ("icp4", 16), ("icp5", 32), ("icp6", 64),
    ("ic07", 128), ("ic08", 256), ("ic09", 512), ("ic10", 1024),
    ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512),
]

chunks = b""
for kind, size in ENTRIES:
    png = subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), "icon.svg"],
        check=True, capture_output=True,
    ).stdout
    chunks += kind.encode() + struct.pack(">I", len(png) + 8) + png

with open("Spotui.icns", "wb") as f:
    f.write(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)
