#!/usr/bin/env python3
"""Normalize archive timestamps/owners so unchanged Runtime versions retain hashes."""
import gzip
from pathlib import Path
import sys
import tarfile


def pack(source, destination):
    def normalize(info):
        info.uid = info.gid = info.mtime = 0
        info.uname = info.gname = ""
        info.pax_headers = {}
        return info
    with destination.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                archive.add(source, arcname=source.name, filter=normalize)


if __name__ == "__main__":
    pack(Path(sys.argv[1]), Path(sys.argv[2]))
