import ctypes as C
import ctypes.util
from dataclasses import dataclass
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import stat
import tempfile


class ArchiveError(RuntimeError):
    pass


@dataclass(frozen=True)
class Limits:
    file_size: int = 512 * 1024 * 1024
    total_size: int = 2 * 1024 * 1024 * 1024
    entries: int = 30000
    deb_tar_size: int = 512 * 1024 * 1024


def require(condition, reason):
    if not condition:
        raise ArchiveError(reason)


def archive_library():
    name = ctypes.util.find_library('archive')
    require(name is not None, 'libarchive runtime required; installation is a user step')
    library = C.CDLL(name)
    signatures = {
        'archive_read_new': (C.c_void_p, []),
        'archive_read_support_format_all': (C.c_int, [C.c_void_p]),
        'archive_read_support_filter_all': (C.c_int, [C.c_void_p]),
        'archive_read_open_filename': (C.c_int, [C.c_void_p, C.c_char_p, C.c_size_t]),
        'archive_read_next_header': (C.c_int, [C.c_void_p, C.POINTER(C.c_void_p)]),
        'archive_read_data': (C.c_ssize_t, [C.c_void_p, C.c_void_p, C.c_size_t]),
        'archive_read_free': (C.c_int, [C.c_void_p]),
        'archive_entry_pathname': (C.c_char_p, [C.c_void_p]),
        'archive_entry_symlink': (C.c_char_p, [C.c_void_p]),
        'archive_entry_hardlink': (C.c_char_p, [C.c_void_p]),
        'archive_entry_filetype': (C.c_uint, [C.c_void_p]),
        'archive_entry_perm': (C.c_uint, [C.c_void_p]),
        'archive_entry_size': (C.c_int64, [C.c_void_p]),
    }
    for symbol, (result, arguments) in signatures.items():
        fn = getattr(library, symbol)
        fn.restype, fn.argtypes = result, arguments
    return library


class SafeExtractor:
    def __init__(self, root, limits=None):
        self.root = Path(root).absolute()
        require(self.root.is_dir() and self.root.resolve() == self.root,
                'extraction root must be a real directory without symlink parents')
        self.limits = limits or Limits()
        self.count = self.total = 0
        self.kinds = {}
        self.links = {}

    def relative(self, name):
        require(isinstance(name, str) and name and '\\' not in name and ':' not in name
                and '\0' not in name and not name.startswith('/')
                and '..' not in name.split('/'), 'unsafe archive path')
        return PurePosixPath(name).as_posix()

    def parents(self, relative):
        current = self.root
        for part in PurePosixPath(relative).parts[:-1]:
            current /= part
            rel = current.relative_to(self.root).as_posix()
            require(rel not in self.links and not current.is_symlink(), 'link in archive parent path')
            if not current.exists():
                current.mkdir(mode=0o700)
            require(current.is_dir(), 'non-directory archive parent')

    def destination(self, name, kind, mode):
        relative = self.relative(name)
        require(not mode & 0o6000, 'privileged archive mode')
        require(relative != '.' or kind == 'dir', 'invalid archive root entry')
        self.count += 1
        require(self.count <= self.limits.entries, 'archive entry limit')
        previous = self.kinds.get(relative)
        require(previous is None or previous == kind == 'dir', 'duplicate archive entry')
        self.parents(relative)
        path = self.root / relative
        require(not path.is_symlink(), 'existing symlink destination')
        if kind == 'dir':
            require(not path.exists() or path.is_dir(), 'directory conflicts with existing entry')
            path.mkdir(mode=0o700, exist_ok=True)
        else:
            require(not path.exists(), 'existing archive destination')
        self.kinds[relative] = kind
        return relative, path

    def extract(self, source, kind):
        require(kind in ('7z', 'tar'), 'unsupported archive format')
        lib = archive_library()
        reader = lib.archive_read_new()
        require(reader, 'cannot create archive reader')
        try:
            require(lib.archive_read_support_format_all(reader) == 0
                    and lib.archive_read_support_filter_all(reader) == 0, 'archive reader setup')
            require(lib.archive_read_open_filename(reader, os.fsencode(source), 65536) == 0,
                    'cannot open archive')
            entry = C.c_void_p()
            while True:
                status = lib.archive_read_next_header(reader, C.byref(entry))
                if status == 1:
                    break
                require(status == 0, 'invalid or truncated archive header')
                name = lib.archive_entry_pathname(entry)
                require(name is not None, 'missing archive path')
                name = os.fsdecode(name)
                mode = lib.archive_entry_perm(entry)
                filetype = lib.archive_entry_filetype(entry)
                hard = lib.archive_entry_hardlink(entry)
                symbolic = lib.archive_entry_symlink(entry)
                entry_kind = ('hardlink' if hard else 'symlink' if symbolic
                              else 'dir' if filetype == stat.S_IFDIR
                              else 'file' if filetype == stat.S_IFREG else 'special')
                require(entry_kind != 'special', 'special archive entry')
                relative, path = self.destination(name, entry_kind, mode)
                if entry_kind == 'dir':
                    if relative != '.':
                        path.chmod(mode & 0o777)
                elif entry_kind in ('hardlink', 'symlink'):
                    value = os.fsdecode(hard or symbolic)
                    require(value and not value.startswith('/') and '\\' not in value
                            and ':' not in value, 'unsafe archive link')
                    target = posixpath.normpath(value if hard else
                                               posixpath.join(posixpath.dirname(relative), value))
                    require(target != '..' and not target.startswith('../'), 'archive link escapes root')
                    self.links[relative] = (entry_kind, value, target)
                else:
                    size = lib.archive_entry_size(entry)
                    require(0 <= size <= self.limits.file_size
                            and self.total + size <= self.limits.total_size, 'archive size limit')
                    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                    with os.fdopen(fd, 'wb') as output:
                        buffer = C.create_string_buffer(65536)
                        written = 0
                        while True:
                            length = lib.archive_read_data(reader, buffer, len(buffer))
                            require(length >= 0, 'invalid or truncated archive data')
                            if length == 0:
                                break
                            written += length
                            require(written <= size, 'archive data exceeds declared size')
                            output.write(buffer.raw[:length])
                        require(written == size, 'archive size mismatch')
                    self.total += written
                    path.chmod(mode & 0o777)
        finally:
            lib.archive_read_free(reader)

    def finish(self):
        def resolve(relative, visiting):
            require(relative not in visiting, 'cyclic archive link')
            if relative in self.links:
                return resolve(self.links[relative][2], visiting | {relative})
            parts = PurePosixPath(relative).parts
            for index in range(1, len(parts)):
                parent = '/'.join(parts[:index])
                require(parent not in self.links, 'link in archive link target parent')
            path = self.root / relative
            require(path.exists() and not path.is_symlink()
                    and path.resolve().is_relative_to(self.root), 'missing or unsafe archive link target')
            return path

        resolved = {relative: resolve(relative, set()) for relative in self.links}
        for relative, (kind, value, _) in self.links.items():
            destination = self.root / relative
            self.parents(relative)
            if kind == 'hardlink':
                require(resolved[relative].is_file(), 'hardlink target is not a file')
                os.link(resolved[relative], destination, follow_symlinks=False)
            else:
                destination.symlink_to(value)
        self.links.clear()

    def extract_deb(self, source, temporary):
        data_path = None
        try:
            with Path(source).open('rb') as stream:
                require(stream.read(8) == b'!<arch>\n', 'invalid deb container')
                end = os.fstat(stream.fileno()).st_size
                version = False
                while stream.tell() < end:
                    header = stream.read(60)
                    require(len(header) == 60 and header[58:] == b'`\n', 'invalid deb member')
                    name = header[:16].decode('ascii').strip().removesuffix('/')
                    raw_size = header[48:58].decode('ascii').strip()
                    require(raw_size.isdecimal(), 'invalid deb member size')
                    size = int(raw_size)
                    offset = stream.tell()
                    require(offset + size <= end, 'truncated deb member')
                    if name == 'debian-binary':
                        require(not version and size == 4 and stream.read(size) == b'2.0\n',
                                'invalid deb version')
                        version = True
                    elif re.fullmatch(r'data\.tar(?:\.(?:xz|gz|bz2|zst|lzma))?', name):
                        require(data_path is None and size <= self.limits.deb_tar_size,
                                'duplicate or oversized deb data')
                        fd, path = tempfile.mkstemp(prefix='.data-', suffix='.tar', dir=temporary)
                        data_path = Path(path)
                        with os.fdopen(fd, 'wb') as output:
                            remaining = size
                            while remaining:
                                block = stream.read(min(65536, remaining))
                                require(block, 'truncated deb data')
                                output.write(block)
                                remaining -= len(block)
                    stream.seek(offset + size + size % 2)
                require(version and data_path is not None, 'missing deb data or version')
            self.extract(data_path, 'tar')
        finally:
            if data_path is not None:
                data_path.unlink()
