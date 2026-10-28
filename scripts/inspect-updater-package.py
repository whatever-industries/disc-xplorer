"""Inspect signed updater bytes without extracting or executing their contents."""
import hashlib
import json
import plistlib
import posixpath
import struct
import sys
import tarfile


def inspect(platform, filename, version):
    if platform.startswith("windows-"):
        with open(filename, "rb") as stream:
            data = stream.read()
        if len(data) < 64 or data[:2] != b"MZ":
            raise ValueError("Windows updater must be a raw EXE, not a ZIP")
        offset = struct.unpack_from("<I", data, 60)[0]
        if data[offset:offset + 4] != b"PE\0\0" or b"NullsoftInst" not in data:
            raise ValueError("Windows updater must be an NSIS installer")
        # Both NSIS installers use an x86 launcher. The build matrix controls
        # the app payload architecture; it cannot be inferred from this launcher.
        return {"format": "nsis"}

    with tarfile.open(filename, "r:gz") as archive:
        members = archive.getmembers()
        names = [member.name for member in members]
        if len(names) != len(set(names)):
            raise ValueError("Duplicate archive paths")
        for member in members:
            if member.name.startswith("/") or ".." in member.name.split("/"):
                raise ValueError("Unsafe archive path")
            if not (member.isfile() or member.isdir() or member.issym()):
                raise ValueError("Unsupported archive entry type")
        if platform.startswith("darwin-"):
            root = "Disc Xplorer.app"
            for member in members:
                if member.name.rstrip("/") != root and not member.name.startswith(root + "/"):
                    raise ValueError("Unexpected macOS bundle layout")
                if member.issym():
                    target = posixpath.normpath(posixpath.join(posixpath.dirname(member.name), member.linkname))
                    if member.linkname.startswith("/") or not target.startswith(root + "/"):
                        raise ValueError("Symlink leaves the macOS bundle")
            plist = archive.getmember(root + "/Contents/Info.plist")
            if not plist.isfile():
                raise ValueError("Bundle Info.plist is not a regular file")
            info = plistlib.loads(archive.extractfile(plist).read())
            if info.get("CFBundleShortVersionString") != version:
                raise ValueError("macOS bundle version differs from manifest")
            if info.get("CFBundleIdentifier") != "com.jaguar.optical-disc-explorer":
                raise ValueError("Wrong macOS bundle identifier")
            binary_name = info["CFBundleExecutable"]
            if "/" in binary_name or binary_name in ("", ".", ".."):
                raise ValueError("Invalid bundle executable name")
            binary = archive.getmember(root + "/Contents/MacOS/" + binary_name)
            expected = 0x100000C if platform == "darwin-aarch64" else 0x1000007
            header = archive.extractfile(binary).read(8)
            if len(header) != 8 or struct.unpack("<II", header) != (0xFEEDFACF, expected):
                raise ValueError("Wrong Mach-O architecture")
            archive.getmember(root + "/Contents/_CodeSignature/CodeResources")
        else:
            suffix = "ARM" if platform == "linux-aarch64" else "x64"
            expected_name = f"Disc Xplorer_Linux_{suffix}_v{version}.AppImage"
            if names != [expected_name]:
                raise ValueError("Linux updater must contain exactly one root-level AppImage")
            binary = members[0]
            stream = archive.extractfile(binary)
            header = stream.read(20)
            expected = 183 if platform == "linux-aarch64" else 62
            if len(header) != 20 or header[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", header, 18)[0] != expected:
                raise ValueError("Wrong AppImage ELF architecture")
        if not binary.isfile() or not binary.mode & 0o111:
            raise ValueError("Updater binary lacks executable permissions")
        digest = hashlib.sha256()
        with archive.extractfile(binary) as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        return {"binarySha256": digest.hexdigest()}


if __name__ == "__main__":
    print(json.dumps(inspect(*sys.argv[1:])))
