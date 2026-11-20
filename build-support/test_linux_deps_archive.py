"""Adversarial archive tests; no network, package install, or GUI programs."""
import io
import os
from pathlib import Path
import stat
import tarfile
import tempfile
import unittest

import linux_deps_archive as archive


def tar_bytes(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as tar:
        for name, kind, value, mode in entries:
            info = tarfile.TarInfo(name)
            info.mode = mode
            info.type = {"file": tarfile.REGTYPE, "dir": tarfile.DIRTYPE,
                         "symlink": tarfile.SYMTYPE, "hardlink": tarfile.LNKTYPE,
                         "fifo": tarfile.FIFOTYPE}[kind]
            if kind == "file":
                info.size = len(value)
                tar.addfile(info, io.BytesIO(value))
            else:
                if kind in ("symlink", "hardlink"):
                    info.linkname = value
                tar.addfile(info)
    return output.getvalue()


def file(name, data=b"content", mode=0o644):
    return name, "file", data, mode


def link(name, target, kind="symlink"):
    return name, kind, target, 0o777 if kind == "symlink" else 0o644


def deb_bytes(members):
    result = bytearray(b"!<arch>\n")
    for name, data in members:
        header = f"{name + '/':<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n"
        result.extend(header.encode("ascii") + data)
        if len(data) % 2:
            result.extend(b"\n")
    return result


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.output = self.root / "out"
        self.output.mkdir(mode=0o700)

    def extract(self, members, **limits):
        source = self.root / "sample.tar"
        source.write_bytes(tar_bytes(members))
        extractor = archive.SafeExtractor(self.output, archive.Limits(**limits))
        extractor.extract(source, "tar")
        extractor.finish()

    def test_rejects_paths_links_conflicts_special_modes_and_resource_abuse(self):
        cases = [
            ([file("../escape")], {}), ([file("/absolute")], {}),
            ([file("a/../escape")], {}), ([file("C:/escape")], {}),
            ([file("a\\escape")], {}),
            ([link("bad", "../../outside")], {}),
            ([link("bad", "/etc/passwd")], {}),
            ([link("bad", "../outside", "hardlink")], {}),
            ([link("dir", "target"), file("dir/escape")], {}),
            ([file("dir/escape"), link("dir", "target")], {}),
            ([link("a", "b"), link("b", "a")], {}),
            ([link("a", "missing")], {}),
            ([file("same"), file("./same")], {}),
            ([("same", "dir", "", 0o755), file("same")], {}),
            ([file("setuid", mode=0o4755)], {}),
            ([file("setgid", mode=0o2755)], {}),
            ([("pipe", "fifo", "", 0o644)], {}),
            ([file("large", b"12345")], {"file_size": 4}),
            ([file("a", b"12"), file("b", b"34")], {"total_size": 3}),
            ([file("a"), file("b")], {"entries": 1}),
        ]
        for i, (entries, limits) in enumerate(cases):
            with self.subTest(case=i):
                self.output = self.root / f"out-{i}"
                self.output.mkdir(mode=0o700)
                with self.assertRaises(archive.ArchiveError):
                    self.extract(entries, **limits)
        self.assertFalse((self.root / "escape").exists())

    def test_safe_forward_links_and_duplicate_directories_are_preserved(self):
        self.extract([("lib", "dir", "", 0o755), ("./lib/", "dir", "", 0o755),
                      link("lib/name.so", "name.so.1"),
                      link("copy", "lib/name.so.1", "hardlink"),
                      file("lib/name.so.1", mode=0o755)])
        self.assertEqual((self.output / "lib/name.so").read_bytes(), b"content")
        self.assertEqual((self.output / "copy").stat().st_ino,
                         (self.output / "lib/name.so.1").stat().st_ino)
        self.assertEqual(stat.S_IMODE((self.output / "copy").stat().st_mode), 0o755)

    def test_existing_symlink_parent_or_file_is_never_followed_or_replaced(self):
        external = self.root / "external"
        external.mkdir()
        (self.output / "dir").symlink_to(external, target_is_directory=True)
        with self.assertRaises(archive.ArchiveError):
            self.extract([file("dir/escape")])
        self.assertEqual(list(external.iterdir()), [])
        (self.output / "kept").write_bytes(b"original")
        with self.assertRaises(archive.ArchiveError):
            self.extract([file("kept", b"replacement")])
        self.assertEqual((self.output / "kept").read_bytes(), b"original")

    def test_link_chain_cannot_escape_after_intermediate_resolution(self):
        with self.assertRaises(archive.ArchiveError):
            self.extract([("d", "dir", "", 0o755), link("d/a", ".."),
                          link("bad", "d/a/../outside")])

    def test_deb_only_extracts_data_and_merges_development_and_runtime(self):
        script = tar_bytes([file("postinst", b"exit 99", 0o755)])
        packages = [
            [file("usr/include/XRes.h"), link("usr/lib/libXRes.so", "libXRes.so.1"),
             file("usr/share/doc/libxres-dev/copyright")],
            [file("usr/lib/libXRes.so.1"), file("usr/share/doc/libxres1/copyright")],
        ]
        extractor = archive.SafeExtractor(self.output)
        for i, entries in enumerate(packages):
            path = self.root / f"package-{i}.deb"
            path.write_bytes(deb_bytes([("debian-binary", b"2.0\n"),
                                        ("control.tar", script),
                                        ("data.tar", tar_bytes(entries))]))
            extractor.extract_deb(path, self.root)
        extractor.finish()
        self.assertFalse((self.output / "postinst").exists())
        self.assertTrue((self.output / "usr/lib/libXRes.so").is_file())
        self.assertFalse(list(self.root.glob(".data-*.tar")))

    def test_deb_missing_duplicate_or_oversized_data_is_rejected(self):
        data = tar_bytes([file("valid")])
        for members, limit in [([], 100000), ([("data.tar", data)] * 2, 100000),
                               ([("data.tar", data)], 100)]:
            with self.subTest(members=len(members), limit=limit):
                path = self.root / "bad.deb"
                path.write_bytes(deb_bytes([("debian-binary", b"2.0\n"), *members]))
                extractor = archive.SafeExtractor(self.output,
                                                  archive.Limits(deb_tar_size=limit))
                with self.assertRaises(archive.ArchiveError):
                    extractor.extract_deb(path, self.root)
                self.assertFalse(list(self.root.glob(".data-*.tar")))

    def test_truncated_archive_is_rejected(self):
        source = self.root / "broken.tar"
        source.write_bytes(tar_bytes([file("large", b"x" * 4096)])[:600])
        with self.assertRaises(archive.ArchiveError):
            extractor = archive.SafeExtractor(self.output)
            extractor.extract(source, "tar")
            extractor.finish()

    def test_symlink_extraction_root_is_rejected(self):
        redirected = self.root / "redirected"
        redirected.symlink_to(self.output, target_is_directory=True)
        with self.assertRaises(archive.ArchiveError):
            archive.SafeExtractor(redirected)


if __name__ == "__main__":
    unittest.main()
