import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from bundle_dylibs import bundle, load_commands, resolve_dependency


class BundleTests(unittest.TestCase):
    def test_load_commands_excludes_library_identity(self):
        output = """Load command 1
 cmd LC_ID_DYLIB
 name /opt/lib/libwebp.dylib (offset 24)
Load command 2
 cmd LC_LOAD_DYLIB
 name @rpath/libsharpyuv.dylib (offset 24)
Load command 3
 cmd LC_RPATH
 path @loader_path/../lib (offset 12)
"""
        load_commands.cache_clear()
        with patch("bundle_dylibs.subprocess.check_output", return_value=output):
            self.assertEqual(load_commands(Path("fake")),
                             (["@rpath/libsharpyuv.dylib"], ["@loader_path/../lib"]))
        load_commands.cache_clear()

    def test_transitive_loader_paths_are_resolved_at_the_original_location(self):
        self.check_graph("@loader_path/../lib/libsharpyuv.dylib", [])

    def test_transitive_rpath_uses_the_executable_search_paths(self):
        self.check_graph("@rpath/libsharpyuv.dylib", ["@executable_path/../lib"])

    def check_graph(self, dependency, rpaths):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            executable = root / "source/bin/ffmpeg"
            webp = root / "source/lib/libwebp.dylib"
            sharp = root / "source/lib/libsharpyuv.dylib"
            for path in (executable, webp, sharp):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(path.name)
            commands = {
                executable: ([str(webp), "/usr/lib/libSystem.B.dylib"], rpaths),
                webp: ([dependency], []),
                sharp: ([], []),
            }
            destination = root / "portable/bin"
            with patch("bundle_dylibs.load_commands", side_effect=lambda path: commands[path]), \
                    patch("bundle_dylibs.subprocess.run") as run:
                bundle({"ffmpeg": executable}, destination)
            self.assertEqual((destination / "lib/libsharpyuv.dylib").read_text(), sharp.name)
            self.assertFalse((destination / "lib/libSystem.B.dylib").exists())
            run.assert_any_call(["install_name_tool", "-change", dependency,
                                 "@loader_path/libsharpyuv.dylib",
                                 str(destination / "lib/libwebp.dylib")], check=True)

    def test_missing_dependency_fails_before_distribution(self):
        with self.assertRaisesRegex(RuntimeError, "Cannot resolve"):
            resolve_dependency("@rpath/missing.dylib", Path("/source/tool"), Path("/source/tool"))


if __name__ == "__main__":
    unittest.main()
