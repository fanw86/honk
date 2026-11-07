"""Assemble a fork release only from matching, verified architecture artifacts."""

import hashlib
import pathlib
import shutil
import sys
import tarfile

UI_SHA = "4c45065cdf47caaf3788a4c034c1ff29b18a25b6"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def assemble(incoming, output, tag, revision):
    if output.exists():
        raise ValueError("output must not already exist")
    verified = []
    sources = {}
    for target in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"):
        binary_dir = incoming / f"honk-core-{target}-full"
        source_dir = incoming / f"fork-sources-{target}"
        binary = binary_dir / f"honk-core-{tag}-{target}-full.tar.gz"
        expected = {binary.name, "fork-honk-source.tar.gz", "fork-doona-source.tar.gz"}
        lines = (binary_dir / "SHA256SUMS").read_text().splitlines()
        checksums = dict((name, sha) for sha, name in (line.split() for line in lines))
        if set(checksums) != expected or len(lines) != 3:
            raise ValueError("unexpected checksum inventory")
        for name, sha in checksums.items():
            path = binary if name == binary.name else source_dir / name
            if digest(path) != sha:
                raise ValueError(f"checksum mismatch: {path}")
        with tarfile.open(binary) as package:
            prefix = binary.name[:-7] + "/"
            names = set(package.getnames())
            required = ["honk-core", "BUILD-INFO.txt", "LICENSE", "honk-core.service", "config.min.dae", "SHADOWQUIC.md", "SHADOWQUIC.zh.md"]
            for edition in ("doona", "doona-lite"):
                required += [f"{edition}/NOTICE", f"{edition}/THIRD-PARTY-NOTICES.txt", f"{edition}/LICENSES"]
            if not all(prefix + name in names for name in required):
                raise ValueError("incomplete binary package")
            manifest = dict(line.split("=", 1) for line in package.extractfile(prefix + "BUILD-INFO.txt").read().decode().splitlines())
            if (manifest["honk_revision"], manifest["doona_revision"], manifest["target"]) != (revision, UI_SHA, target):
                raise ValueError("source/target manifest mismatch")
            if not {"ebpf", "rprx", "clash-api", "native-ui", "shadowquic", "jemalloc"}.issubset(manifest["features"].split(",")):
                raise ValueError("required release features missing")
        for name, sha in (("fork-honk-source.tar.gz", revision), ("fork-doona-source.tar.gz", UI_SHA)):
            path = source_dir / name
            with tarfile.open(path) as source:
                if source.pax_headers.get("comment") != sha:
                    raise ValueError("source archive commit mismatch")
            if name in sources and digest(path) != digest(sources[name]):
                raise ValueError("architecture source archives differ")
            sources[name] = path
        verified.append(binary)
    output.mkdir(parents=True)
    for binary in verified:
        shutil.copyfile(binary, output / binary.name)
    shutil.copyfile(sources["fork-honk-source.tar.gz"], output / f"honk-source-{tag}.tar.gz")
    shutil.copyfile(sources["fork-doona-source.tar.gz"], output / f"doona-source-{UI_SHA}.tar.gz")
    files = sorted(output.iterdir())
    (output / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.name}\n" for path in files))
    print("Verified two binaries and matching corresponding sources; release checksums generated.")


if __name__ == "__main__":
    assemble(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3], sys.argv[4])
