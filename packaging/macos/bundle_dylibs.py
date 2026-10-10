#!/usr/bin/env python3
"""Bundle the complete Mach-O dependency graph, resolving paths before copying."""

import argparse
from functools import lru_cache
from pathlib import Path
import re
import shutil
import subprocess


def system_library(name):
    return name.startswith(("/usr/lib/", "/System/Library/"))


@lru_cache(maxsize=None)
def load_commands(binary):
    output = subprocess.check_output(["otool", "-l", str(binary)], text=True)
    dependencies, rpaths = [], []
    command = None
    for line in output.splitlines():
        fields = line.strip().split(maxsplit=1)
        if len(fields) != 2:
            continue
        key, value = fields
        if key == "cmd":
            command = value
        elif key == "name" and command in {
            "LC_LOAD_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_REEXPORT_DYLIB", "LC_LAZY_LOAD_DYLIB"
        }:
            dependencies.append(re.sub(r" \(offset \d+\)$", "", value))
        elif key == "path" and command == "LC_RPATH":
            rpaths.append(re.sub(r" \(offset \d+\)$", "", value))
    return dependencies, rpaths


def expand_path(name, loader, executable):
    for prefix, directory in [("@loader_path/", loader.parent),
                              ("@executable_path/", executable.parent)]:
        if name.startswith(prefix):
            return directory / name[len(prefix):]
    return Path(name) if name.startswith("/") else None


def resolve_dependency(name, loader, executable, inherited_rpaths=()):
    direct = expand_path(name, loader, executable)
    if direct is not None and direct.is_file():
        return direct.resolve()
    if name.startswith("@rpath/"):
        relative = name[len("@rpath/"):]
        candidates = [directory / relative for directory in inherited_rpaths]
        candidates.append(loader.parent / relative)
        for candidate in candidates:
            if candidate.is_file():
                return candidate.resolve()
    raise RuntimeError(f"Cannot resolve {name!r}, required by {loader}")


def bundle(binaries, bin_dir):
    lib_dir = bin_dir / "lib"
    lib_dir.mkdir(parents=True, exist_ok=True)
    copied = {}
    visited = set()

    def visit(original, destination, executable, inherited_rpaths=()):
        original = original.resolve()
        key = (original, destination)
        if key in visited:
            return
        visited.add(key)
        dependencies, rpaths = load_commands(original)
        search_paths = tuple(
            candidate for item in rpaths
            if (candidate := expand_path(item, original, executable)) is not None
        ) + tuple(inherited_rpaths)
        for name in dependencies:
            if system_library(name):
                continue
            dependency = resolve_dependency(name, original, executable, search_paths)
            basename = Path(name).name
            target = lib_dir / basename
            if basename in copied and copied[basename] != dependency:
                raise RuntimeError(f"Conflicting bundled libraries named {basename}")
            if basename not in copied:
                copied[basename] = dependency
                shutil.copy2(dependency, target)
                target.chmod(target.stat().st_mode | 0o200)
                subprocess.run(["install_name_tool", "-id", f"@rpath/{basename}", str(target)], check=True)
                visit(dependency, target, executable, search_paths)
            prefix = "@loader_path" if destination.parent == lib_dir else "@executable_path/lib"
            subprocess.run(["install_name_tool", "-change", name, f"{prefix}/{basename}", str(destination)], check=True)

    for name, source in binaries.items():
        source = source.resolve()
        destination = bin_dir / name
        if not destination.exists():
            shutil.copy2(source, destination)
        destination.chmod(destination.stat().st_mode | 0o200)
        visit(source, destination, source)
    print(f"Bundled {len(copied)} FFmpeg libraries in {lib_dir}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--ffmpeg", required=True, type=Path)
    parser.add_argument("--ffprobe", required=True, type=Path)
    parser.add_argument("--bin-dir", required=True, type=Path)
    args = parser.parse_args()
    bundle({"ffmpeg": args.ffmpeg, "ffprobe": args.ffprobe}, args.bin_dir.resolve())
