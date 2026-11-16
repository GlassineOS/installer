#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""The PC check on virtual UEFI PCs (spec 003 SC-001).

Each case boots an x86-64 QEMU machine with OVMF and a writable variable store,
the disks of the case (made here with sgdisk, mkfs and our own bytes, never
committed) and a USB stick holding the static glassine-install. The guest
(tests/qemu/init, on Alpine's kernel and initramfs) runs inspect, check and
plan with --pc and prints their records. The host asserts:

- every disk image and the variable store are byte-identical between the
  guest's @@READY (booted, idle, before the installer runs) and @@DONE; the
  disks also from before QEMU starts to after it exits (the firmware itself
  writes its store while booting, so the store is compared over the
  installer's run only);
- the general PC description fits; every disk and system is listed; the
  installer's own disk is named and refused as a target; a disk too small for
  GlassineOS's partitions (the description's sizes; the release manifest
  fixture on the stick states a least that fits them) is refused with the
  space it lacks;
- a control run that writes one disk byte and one variable is caught;
- with Secure Boot on (OVMF's Secure Boot build, a key made for the run
  enrolled, the guest's kernel signed with it) the check reads it as on and
  every plan is refused with the reason and how to switch it off, writing
  nothing (boot 010 FR-036, SC-016); without it (the other cases) it reads off;
- the same firmware refuses to start an unsigned program from the stick and
  starts the signed one (SC-016: Secure Boot on starts no unsigned medium).

The install cases (install-*) write instead: they install GlassineOS on the
case's disk and check the disk, the firmware's variables and that the
machine then starts the new boot entry (see the install section below).

Needs: qemu-system-x86_64, OVMF (with its Secure Boot build for those cases),
sgdisk, mkfs.ext4, mkfs.fat, mkntfs, ntfscp, mcopy, mmd, unsquashfs, openssl,
sbsign, virt-fw-vars; for the install cases mkfs.btrfs and fsck.fat. Downloads (pinned, hash-checked) go to --cache. The keys are
made in --work for each run and never kept. Runs without KVM if there is none
(slower)."""

import argparse
import gzip
import hashlib
import json
import os
import queue
import shutil
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(HERE))
import placeholder  # noqa: E402
# The release manifest the plans read (spec 002 FR-019a): its base's least
# system partition decides what the small disk lacks.
RELEASE = HERE.parent.parent / "core" / "tests" / "fixtures" / "releases" / "base-8gib.json"
MIB = 1 << 20
GIB = 1 << 30

ALPINE = "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot-3.24.0"
FILES = {
    "vmlinuz-virt": "1e6bf9027720c75c3ed0d79171f21b5791ee40ca9795d07c7c6e04dc5ea2ae90",
    "initramfs-virt": "64258649a54c3983a62f3d46c2c7f646161aaccaa4fad15503a30c0d68875494",
    "modloop-virt": "a77c8eef2536f24d38893d35b0ece10e61188be625ed566ce6bdd162c582057d",
}
# Modules the guest loads that the initramfs lacks; their dependencies come too.
MODULES = ["sd_mod", "nvme", "efivarfs", "usb-storage", "xhci-pci", "virtio_blk", "vfat"]

# OVMF's Secure Boot build: it needs SMM, and an enrolled key to enforce.
OVMF_SECBOOT_CANDIDATES = [
    ("/usr/share/OVMF/OVMF_CODE_4M.secboot.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
    ("/usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd", "/usr/share/edk2/x64/OVMF_VARS.4m.fd"),
]
# The owner GUID the run's key is enrolled under (any GUID will do).
KEY_OWNER = "6b2b5e1c-1f3e-4b55-9a5e-2c4f1d5e7a10"

OVMF_CANDIDATES = [
    ("/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
    ("/usr/share/edk2/x64/OVMF_CODE.4m.fd", "/usr/share/edk2/x64/OVMF_VARS.4m.fd"),
    ("/usr/share/OVMF/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
]

FAILURES = []


def check(ok, what):
    print(("  ok   " if ok else "  FAIL ") + what, flush=True)
    if not ok:
        FAILURES.append(what)
    return ok


def run(*cmd, **kw):
    subprocess.run([str(c) for c in cmd], check=True, stdout=subprocess.DEVNULL, **kw)


# ---- downloads and the guest's boot files ------------------------------------


def fetch(cache: Path) -> dict:
    cache.mkdir(parents=True, exist_ok=True)
    out = {}
    for name, sha in FILES.items():
        p = cache / name
        if not p.exists() or hashlib.sha256(p.read_bytes()).hexdigest() != sha:
            tmp = cache / (name + ".part")
            with urllib.request.urlopen(f"{ALPINE}/{name}", timeout=120) as r, open(tmp, "wb") as f:
                shutil.copyfileobj(r, f)
            got = hashlib.sha256(tmp.read_bytes()).hexdigest()
            if got != sha:
                sys.exit(f"{name}: sha256 {got}, expected {sha}")
            tmp.rename(p)
        out[name] = p
    return out


def newc(entries) -> bytes:
    """A newc cpio archive of (name, mode, data) entries."""
    out = bytearray()

    def one(ino, name, mode, data):
        n = name.encode() + b"\0"
        fields = [ino, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(n), 0]
        out.extend(b"070701" + b"".join(b"%08x" % f for f in fields) + n)
        out.extend(b"\0" * (-len(out) % 4))
        out.extend(data)
        out.extend(b"\0" * (-len(out) % 4))

    for i, (name, mode, data) in enumerate(entries, 1):
        one(i, name, mode, data)
    one(0, "TRAILER!!!", 0, b"")
    return bytes(out)


def initramfs(files: dict, work: Path) -> Path:
    """Alpine's initramfs with our /init and the modules it lacks appended."""
    ml = work / "modloop"
    if ml.exists():
        shutil.rmtree(ml)
    run("unsquashfs", "-q", "-n", "-d", ml, files["modloop-virt"])
    (kver,) = [d for d in (ml / "modules").iterdir() if (d / "modules.dep").is_file()]
    deps = {}
    for line in (kver / "modules.dep").read_text().splitlines():
        mod, _, rest = line.partition(":")
        deps[mod] = rest.split()
    by_name = {Path(m).name.split(".ko")[0].replace("-", "_"): m for m in deps}
    want = set()
    stack = [by_name[m.replace("-", "_")] for m in MODULES if m.replace("-", "_") in by_name]
    while stack:
        m = stack.pop()
        if m not in want:
            want.add(m)
            stack.extend(deps.get(m, []))
    # /lib is a link to usr/lib there: an entry named lib would replace it.
    base = f"usr/lib/modules/{kver.name}"
    entries, dirs = [], set()

    def add_dirs(rel):
        parts = Path(rel).parent.parts
        for i in range(1, len(parts) + 1):
            d = "/".join(parts[:i])
            if d not in dirs:
                dirs.add(d)
                entries.append((d, 0o040755, b""))

    for f in sorted(kver.iterdir()):
        if f.is_file() and f.name.startswith("modules."):
            add_dirs(f"{base}/{f.name}")
            entries.append((f"{base}/{f.name}", 0o100644, f.read_bytes()))
    for m in sorted(want):
        add_dirs(f"{base}/{m}")
        entries.append((f"{base}/{m}", 0o100644, (kver / m).read_bytes()))
    entries.append(("init", 0o100755, (HERE / "init").read_bytes()))
    out = work / "initramfs"
    alpine = files["initramfs-virt"].read_bytes()
    # The kernel reads concatenated archives only from a 4-byte boundary.
    alpine += b"\0" * (-len(alpine) % 512)
    out.write_bytes(alpine + newc(entries))
    return out


# ---- disks --------------------------------------------------------------------


def sparse(p: Path, size: int):
    with open(p, "wb") as f:
        f.truncate(size)


def put(p: Path, offset: int, data: bytes):
    with open(p, "r+b") as f:
        f.seek(offset)
        f.write(data)


def part_start(p: Path, n: int) -> tuple:
    out = subprocess.run(["sgdisk", "-i", str(n), str(p)], check=True, capture_output=True, text=True).stdout
    first = last = None
    for line in out.splitlines():
        if line.startswith("First sector:"):
            first = int(line.split()[2])
        if line.startswith("Last sector:"):
            last = int(line.split()[2])
    return first * 512, (last - first + 1) * 512


def make_blank(p: Path, size: int):
    sparse(p, size)


# The disks that take GlassineOS have its 26 GiB free: the boot (2 GiB) and
# system (16 GiB) partitions and userdata's least (8 GiB), as
# devices/uefi-pc.toml sizes them.
NEEDS = 26 * GIB


def make_linux(p: Path, work: Path):
    sparse(p, 40 * GIB)
    run("sgdisk", "-n1:1MiB:+512MiB", "-t1:ef00", "-c1:EFI system partition", "-n2:0:+8GiB", "-t2:8304", "-c2:root", p)
    off, size = part_start(p, 1)
    run("mkfs.fat", "-F", "32", "--offset", off // 512, p, size // 1024)
    efi = work / "efi"
    (efi / "EFI" / "linux").mkdir(parents=True, exist_ok=True)
    (efi / "EFI" / "linux" / "loader.efi").write_bytes(b"\0" * 300000)
    run("mcopy", "-s", "-i", f"{p}@@{off}", efi / "EFI", "::/")
    off, size = part_start(p, 2)
    root = work / "rootfs"
    for d in ["etc", "usr/bin", "home/someone"]:
        (root / d).mkdir(parents=True, exist_ok=True)
    (root / "etc" / "os-release").write_text('NAME="Another Linux"\n')
    # About 200 MiB of data, so its used bytes are clearly more than metadata.
    with open(root / "usr" / "bin" / "data", "wb") as f:
        f.write(os.urandom(200 * MIB))
    run("mkfs.ext4", "-q", "-F", "-E", f"offset={off}", "-d", root, p, f"{size // 1024}k")


def copy_in(src: Path, dst: Path, at: int):
    """Copies src into dst at offset at, skipping blocks of zeros (dst stays sparse)."""
    with open(src, "rb") as s, open(dst, "r+b") as d:
        pos = 0
        while b := s.read(MIB):
            if b.count(0) != len(b):
                d.seek(at + pos)
                d.write(b)
            pos += len(b)


def make_ntfs(p: Path, n: int, work: Path, files: dict):
    """A real NTFS (mkntfs) on partition n, holding files (name: bytes)."""
    off, size = part_start(p, n)
    vol = work / f"ntfs{n}.img"
    sparse(vol, size)
    run("mkntfs", "-q", "-F", "-Q", vol)
    for name, data in files.items():
        f = work / f"ntfs-file-{name}"
        f.write_bytes(data)
        run("ntfscp", "-f", vol, f, name)
        f.unlink()
    copy_in(vol, p, off)
    vol.unlink()


def make_windows(p: Path, work: Path):
    sparse(p, 40 * GIB)
    run(
        "sgdisk",
        "-n1:1MiB:+100MiB", "-t1:ef00", "-c1:EFI system partition",
        "-n2:0:+16MiB", "-t2:0c01", "-c2:Microsoft reserved partition",
        "-n3:0:+12GiB", "-t3:0700", "-c3:Basic data partition",
        "-n4:0:+600MiB", "-t4:2700",
        p,
    )
    off, size = part_start(p, 1)
    run("mkfs.fat", "-F", "32", "--offset", off // 512, p, size // 1024)
    boot = work / "efi-win"
    (boot / "EFI" / "Microsoft" / "Boot").mkdir(parents=True, exist_ok=True)
    (boot / "EFI" / "Microsoft" / "Boot" / "bootmgfw.efi").write_bytes(os.urandom(300000))
    run("mcopy", "-s", "-i", f"{p}@@{off}", boot / "EFI", "::/")
    # Windows' volume with about 64 MiB of files, and its recovery volume
    make_ntfs(p, 3, work, {"pagefile.sys": os.urandom(48 * MIB), "notes.txt": b"the person's files\n" * 1000})
    make_ntfs(p, 4, work, {"winre.wim": os.urandom(16 * MIB)})


def make_stick(p: Path, binary: Path):
    sparse(p, 256 * MIB)
    run("sgdisk", "-n1:1MiB:0", "-t1:0700", "-c1:GLINSTALL", p)
    off, size = part_start(p, 1)
    run("mkfs.fat", "-F", "32", "-n", "GLINSTALL", "--offset", off // 512, p, size // 1024)
    run("mcopy", "-i", f"{p}@@{off}", binary, "::/glassine-install")
    run("mcopy", "-i", f"{p}@@{off}", RELEASE, "::/release.json")


# Each disk: (file, how QEMU attaches it, its size, what is on it).
CASES = {
    "blank": [("nvme", "blank32")],
    "linux": [("virtio", "linux")],
    "windows": [("sata", "windows")],
    "small": [("virtio", "small")],
    "two-disks": [("nvme", "linux"), ("sata", "blank32")],
    "control": [("virtio", "blank32")],
    "secure-boot-on": [("sata", "windows")],
    "secure-boot-medium": [],
}
# The cases on OVMF's Secure Boot build, with the run's key enrolled.
SECURE = {"secure-boot-on", "secure-boot-medium"}
KIND = {"nvme": "nvme", "virtio": "virtio", "sata": "sata"}


def build_disk(what: str, p: Path, work: Path):
    if what == "data1g":
        # 1 GiB with 8 MiB of data at 64 MiB: what the backup must keep
        sparse(p, 1 * GIB)
        put(p, 64 * MIB, os.urandom(8 * MIB))
    elif what == "blank32":
        make_blank(p, 32 * GIB)
    elif what == "small":
        make_blank(p, 4 * GIB)
    elif what == "linux":
        make_linux(p, work)
    elif what == "windows":
        make_windows(p, work)


def digest(p: Path) -> str:
    """A sha256 of the file's bytes that reads only its data: its size, then
    each data region's offset and bytes. The holes of a sparse file read as
    zeros, so equal digests mean equal bytes (a write, even of zeros into a
    hole, changes the regions and so the digest)."""
    h = hashlib.sha256()
    with open(p, "rb") as f:
        size = os.fstat(f.fileno()).st_size
        h.update(b"size %d\n" % size)
        pos = 0
        while pos < size:
            try:
                start = os.lseek(f.fileno(), pos, os.SEEK_DATA)
            except OSError:
                break  # no data after pos
            end = os.lseek(f.fileno(), start, os.SEEK_HOLE)
            h.update(b"data %d %d\n" % (start, end))
            f.seek(start)
            left = end - start
            while left:
                b = f.read(min(left, 8 * MIB))
                if not b:
                    break
                h.update(b)
                left -= len(b)
            pos = end
    return h.hexdigest()


def sha(paths):
    """The digest of each file, in parallel."""
    with ThreadPoolExecutor(max_workers=max(1, len(paths))) as ex:
        return dict(zip(paths, ex.map(digest, paths)))


# ---- one machine ----------------------------------------------------------------


class Guest:
    def __init__(self, cmd, log: Path):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.lines = queue.Queue()
        self.log = open(log, "w")
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in self.p.stdout:
            line = raw.decode("utf-8", "replace").replace("\r", "").rstrip("\n")
            self.log.write(line + "\n")
            self.log.flush()
            self.lines.put(line)
        self.lines.put(None)

    def until(self, marker, timeout):
        """The lines up to the first holding `marker` (or one of a tuple)."""
        markers = (marker,) if isinstance(marker, str) else marker
        got, end = [], time.monotonic() + timeout
        while True:
            try:
                line = self.lines.get(timeout=max(0.1, end - time.monotonic()))
            except queue.Empty:
                line = None
                if time.monotonic() < end:
                    continue
            if line is None:
                raise RuntimeError(f"no {marker} within {timeout} s (or QEMU ended)")
            got.append(line)
            # Markers can follow the echo of the host's line or kernel text.
            if any(m in line for m in markers):
                return got
            if time.monotonic() > end:
                raise RuntimeError(f"no {marker} within {timeout} s")

    def send(self, text):
        self.p.stdin.write(text.encode())
        self.p.stdin.flush()

    def stop(self, timeout):
        try:
            self.p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait()
            raise RuntimeError("QEMU didn't power off")


def records(lines):
    """The JSON between @@BEGIN name and @@END name, and each one's exit code."""
    out, name, buf = {}, None, []
    for line in lines:
        if line.startswith("@@BEGIN "):
            name, buf = line[8:].strip(), []
        elif name and line.startswith("@@EXIT "):
            code = int(line.split()[1])
            text = "\n".join(buf)
            if name.startswith("log-"):
                out[name] = (code, text)
                continue
            try:
                out[name] = (code, json.loads(text))
            except json.JSONDecodeError:
                out[name] = (code, None)
                print(f"  record {name} isn't JSON:\n{text[:2000]}")
        elif name and line.startswith("@@END"):
            name = None
        elif name:
            buf.append(line)
    return out


def qemu_cmd(args, kernel, initrd, vars_fd, disks, stick, mode, secure=False):
    code = args.ovmf_secboot_code if secure else args.ovmf_code
    cmd = [
        "qemu-system-x86_64",
        "-machine", "q35,smm=on" if secure else "q35",
        "-m", "1024",
        "-smp", "2",
        "-accel", "kvm", "-accel", "tcg",
        "-display", "none", "-monitor", "none", "-serial", "stdio",
        "-no-reboot",
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_fd}",
    ]
    if secure:
        cmd += ["-global", "driver=cfi.pflash01,property=secure,value=on"]
    if kernel:
        cmd += [
            "-kernel", str(kernel), "-initrd", str(initrd),
            "-append", f"console=ttyS0 quiet loglevel=1 panic=-1 glassine.disks={len(disks) + 1} glassine.mode={mode}",
        ]
    else:
        cmd += ["-net", "none"]
    cmd += [
        "-device", "qemu-xhci,id=xhci",
    ]
    if stick:
        cmd += [
            "-drive", f"if=none,id=stick,format=raw,file={stick}",
            "-device", "usb-storage,bus=xhci.0,drive=stick,removable=on",
        ]
    cmd += ["-device", "ahci,id=ahci"]
    for i, (bus, p) in enumerate(disks):
        cmd += ["-drive", f"if=none,id=d{i},format=raw,file={p}"]
        if bus == "nvme":
            cmd += ["-device", f"nvme,drive=d{i},serial=glassine-ci-{i}"]
        elif bus == "sata":
            cmd += ["-device", f"ide-hd,drive=d{i},bus=ahci.{i}"]
        else:
            cmd += ["-device", f"virtio-blk-pci,drive=d{i}"]
    return cmd


def secure_boot(args, files, work: Path) -> dict:
    """A key made for this run, a variable store with it enrolled (PK, KEK
    and db) and Secure Boot on, and the guest's kernel signed with it."""
    k = work / "secure-boot"
    if k.exists():
        shutil.rmtree(k)
    k.mkdir(parents=True)
    key, cert = k / "db.key", k / "db.pem"
    run("openssl", "req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert,
        "-subj", "/CN=GlassineOS installer CI (made for one run)", "-days", "1", stderr=subprocess.DEVNULL)
    vars_fd = k / "OVMF_VARS.secure.fd"
    run("virt-fw-vars", "-i", args.ovmf_secboot_vars, "-o", vars_fd,
        "--set-pk", KEY_OWNER, cert, "--add-kek", KEY_OWNER, cert, "--add-db", KEY_OWNER, cert, "--sb",
        stderr=subprocess.DEVNULL)
    signed = k / "vmlinuz-signed"
    run("sbsign", "--key", key, "--cert", cert, "--output", signed, files["vmlinuz-virt"], stderr=subprocess.DEVNULL)
    return {"vars": vars_fd, "signed": signed, "unsigned": files["vmlinuz-virt"]}


def medium(args, sb, work: Path):
    """SC-016: with Secure Boot on, the firmware starts the stick's program
    only when it is signed with an enrolled key. Each stick holds Alpine's
    kernel as the removable-media program (\\EFI\\BOOT\\BOOTX64.EFI), unsigned
    and signed, and the firmware's own boot messages tell what it did."""
    print("case secure-boot-medium:", flush=True)
    cw = work / "secure-boot-medium"
    if cw.exists():
        shutil.rmtree(cw)
    cw.mkdir(parents=True)
    for which in ("unsigned", "signed"):
        stick = cw / f"stick-{which}.img"
        sparse(stick, 64 * MIB)
        run("sgdisk", "-n1:1MiB:0", "-t1:ef00", stick)
        off, size = part_start(stick, 1)
        run("mkfs.fat", "-F", "32", "--offset", off // 512, stick, size // 1024)
        run("mmd", "-i", f"{stick}@@{off}", "::/EFI", "::/EFI/BOOT")
        run("mcopy", "-i", f"{stick}@@{off}", sb[which], "::/EFI/BOOT/BOOTX64.EFI")
        vars_fd = cw / f"OVMF_VARS-{which}.fd"
        shutil.copyfile(sb["vars"], vars_fd)
        g = Guest(qemu_cmd(args, None, None, vars_fd, [], stick, None, secure=True), cw / f"serial-{which}.log")
        try:
            # The firmware's boot manager says which it did with the stick.
            lines = g.until(("starting Boot", "Access Denied"), args.boot_timeout)
        finally:
            g.p.kill()
            g.p.wait()
        said = [x for x in lines if "BdsDxe" in x]
        if which == "unsigned":
            check(any("failed to load" in x and "Access Denied" in x for x in said), f"the unsigned program isn't started: {said}")
        else:
            check(any("starting Boot" in x for x in said) and not any("Access Denied" in x for x in said), f"the signed one is: {said}")


def case(name, args, boot, work: Path, sb=None):
    print(f"case {name}:", flush=True)
    secure = name in SECURE
    cw = work / name
    if cw.exists():
        shutil.rmtree(cw)
    cw.mkdir(parents=True)
    disks = []
    for i, (bus, what) in enumerate(CASES[name]):
        p = cw / f"disk{i}-{what}.img"
        build_disk(what, p, cw)
        disks.append((bus, p))
    stick = cw / "stick.img"
    make_stick(stick, args.binary)
    vars_fd = cw / "OVMF_VARS.fd"
    shutil.copyfile(sb["vars"] if secure else args.ovmf_vars, vars_fd)
    kernel = sb["signed"] if secure else boot["kernel"]
    images = [p for _, p in disks] + [stick]
    before_boot = sha(images)

    mode = "control" if name == "control" else "check"
    g = Guest(qemu_cmd(args, kernel, boot["initrd"], vars_fd, disks, stick, mode, secure), cw / "serial.log")
    try:
        g.until("@@READY", args.boot_timeout)
        at_ready = sha(images + [vars_fd])
        g.send("go\n")
        lines = g.until("@@DONE", args.run_timeout)
        at_done = sha(images + [vars_fd])
        g.send("off\n")
        g.stop(120)
    except Exception:
        g.p.kill()
        raise
    after_exit = sha(images)
    (cw / "hashes.json").write_text(
        json.dumps(
            {
                "before-boot": {p.name: h for p, h in before_boot.items()},
                "ready": {p.name: h for p, h in at_ready.items()},
                "done": {p.name: h for p, h in at_done.items()},
                "after-exit": {p.name: h for p, h in after_exit.items()},
            },
            indent=1,
        )
    )
    if mode == "control":
        target = disks[0][1]
        check(at_ready[target] != at_done[target], "control: the byte written to a disk changes its hash")
        check(at_ready[vars_fd] != at_done[vars_fd], "control: the variable written changes the store's hash")
        return
    for p in images:
        check(at_ready[p] == at_done[p], f"{p.name} unchanged while the installer ran")
        check(before_boot[p] == after_exit[p], f"{p.name} unchanged from before QEMU started to after it ended")
    check(at_ready[vars_fd] == at_done[vars_fd], "the firmware's variable store unchanged while the installer ran")

    rec = records(lines)
    for k, (code, _) in sorted(rec.items()):
        (cw / f"{k}.json").write_text(json.dumps(rec[k][1], indent=1) + "\n")
    facts = rec.get("facts", (None, None))[1]
    if not check(facts is not None, "inspect --pc gave facts"):
        return
    st = facts["storage"]
    sticks = [d for d in st["disks"] if d["kind"] == "usb"]
    check(len(st["disks"]) == len(disks) + 1, f"every disk listed ({len(disks)} and the stick)")
    check(len(sticks) == 1 and sticks[0]["removable"], "the stick is listed as a removable USB disk")
    own = st.get("installer_disk") or {}
    check(bool(sticks) and own.get("index") == sticks[0]["index"] and own.get("by") == "executable", f"the installer's own disk is named: {own}")
    fw = facts.get("firmware", {})
    check(fw.get("kind") == "uefi" and fw.get("variables") == "readable", f"UEFI with a readable variable store: {fw.get('kind')}, {fw.get('variables')}")
    want_sb = "on" if secure else "off"
    check(fw.get("secure_boot") == want_sb, f"secure boot {want_sb}: {fw.get('secure_boot')}")
    check(facts.get("cpu", {}).get("arch") == "x86-64", "x86-64")
    by_kind = {}
    for d in st["disks"]:
        if d["kind"] != "usb":
            by_kind.setdefault(d["kind"], []).append(d)
    for bus, _ in disks:
        check(KIND[bus] in by_kind, f"a {KIND[bus]} disk is listed")

    chk = rec.get("check", (None, None))[1] or {}
    if secure:
        # Every plan is refused for Secure Boot alone (a developer's too),
        # saying why and how to switch it off; the check report says so.
        sb_reason = lambda p: [r for r in p.get("reasons", []) if r.startswith("Secure Boot is on")]
        plans = {k: v[1] or {} for k, v in rec.items() if k == "plan" or k.startswith("plan-disk")}
        for k, p in sorted(plans.items()):
            if p.get("status") == "refused" and any("the installer runs from" in r for r in p.get("reasons", [])):
                continue  # the stick: refused for that too
            r = sb_reason(p)
            check(p.get("status") == "refused" and bool(r), f"{k} refused for Secure Boot: {p.get('status')} {p.get('reasons')}")
            check(bool(r) and "set Secure Boot to Disabled" in r[0] and "set-up" in r[0], f"{k} says how to switch it off")
        check(chk.get("supported") is False and any(r.startswith("Secure Boot is on") for r in chk.get("reasons", [])), "the check report gives the reason")
        check(chk.get("machine", {}).get("firmware", {}).get("secure_boot") == "on", "the check report shows Secure Boot on")
        return
    check(chk.get("recognition", {}).get("device") == "uefi-pc", f"the general PC description fits: {chk.get('recognition', {}).get('result')}")
    systems = facts.get("systems", [])
    want_systems = sum(1 for _, w in CASES[name] if w in ("linux", "windows"))
    check(len(systems) == want_systems, f"every system listed: {[(s['kind'], s['disk']) for s in systems]}")
    check(len(chk.get("machine", {}).get("systems", [])) == want_systems, "the check report lists them too")

    plan = rec.get("plan", (None, None))[1] or {}
    check(plan.get("description") == "uefi-pc", "the plan is the general PC description's")
    check(len(plan.get("systems", [])) == want_systems, "the plan names every system")
    stick_i = sticks[0]["index"] if sticks else None
    own_plan = rec.get(f"plan-disk{stick_i}", (None, None))[1] or {}
    check(
        own_plan.get("status") == "refused" and any("the installer runs from" in r for r in own_plan.get("reasons", [])),
        f"the installer's own disk is refused as a target: {own_plan.get('reasons')}",
    )
    target = (plan.get("space") or {}).get("disk") or {}
    check(target.get("disk") != stick_i, "the default target isn't the installer's disk")

    for i, (bus, what) in enumerate(CASES[name]):
        d = by_kind.get(KIND[bus], [None])[0]
        if d is None:
            continue
        p = rec.get(f"plan-disk{d['index']}", (None, None))[1] or {}
        sp = (p.get("space") or {}).get("disk") or {}
        if what == "small":
            # The fixed boot and system partitions and userdata's least; the
            # release's least system partition fits the fixed one.
            least = json.loads(RELEASE.read_text())["artifacts"]["base"]["min_partition_bytes"]
            assert least <= 16 * GIB
            needs, free = NEEDS, d["free"][0] if d["free"] else 0
            check(p.get("status") == "refused", f"the small disk is refused: {p.get('reasons')}")
            check(sp.get("lacks") == needs - free and sp.get("lacks", 0) > 0, f"with the space it lacks: {sp}")
            check(any("too small" in r and "lacks" in r for r in p.get("reasons", [])), "the reason says so")
        else:
            check(p.get("status") == "ready", f"disk {d['index']} ({what}) can take GlassineOS: {p.get('status')} {p.get('reasons')}")
        if what == "linux":
            fs = [x.get("fs") or {} for x in d["partitions"]]
            ext = [f for f in fs if f.get("kind") == "ext4"]
            check(bool(ext) and (ext[0].get("used") or 0) > 200 * MIB, f"the ext4 tells its used bytes: {ext}")
            imgs = (p.get("backup") or {}).get("images", [])
            check(bool(imgs) and imgs[0]["estimate_bytes"] < d["size"] - 7 * GIB, f"its image bound is below the disk's size: {imgs[:1]}")
            check(any(s["kind"] == "linux" and s["disk"] == d["index"] for s in systems), "the other Linux is found")
        if what == "windows":
            kinds = [(x.get("fs") or {}).get("kind") for x in d["partitions"]]
            check(kinds == ["vfat", None, "ntfs", "ntfs"], f"ESP, MSR, NTFS, recovery: {kinds}")
            win = [s for s in systems if s["kind"] == "windows"]
            check(bool(win) and win[0]["partitions"] == [2, 3, 4], f"Windows is found: {win}")


# ---- installs (the PC's write steps) ----------------------------------------------
#
# Each install case installs GlassineOS on the case's disk from a release
# folder on the stick: a btrfs pool image made here with mkfs.btrfs and a
# marker file (the base), Alpine's kernel as the kernel, a small initramfs
# fixture, and the placeholder loader (placeholder.py) as the boot loader. Its
# first step backs the disk up onto the stick; the host reads that image
# (backup format 2, read_image below) and checks every range it holds against
# the disk as it was, and that it stores about the disk's used bytes. The
# host then checks the disk and the firmware's variables, and starts the
# machine from its disk alone: the firmware must start our entry, which
# starts the placeholder loader. The restore case then puts the backup back
# (restore --pc) and the disk must be the disk as it was, byte for byte, and
# the boot order the one it had. The backup's and the restore's failure
# cases are tested on the host (core/tests/pc_execute.rs, pcwrite/tests).

GLOBAL = "8be4df61-93ca-11d2-aa0d-00e098032b8c"
ROOT_X86 = "4f68bce3-e8cd-4db1-96e7-fbcaf984b709"
LINUX_DATA = "0fc63daf-8483-4772-8e79-3d69d8477de4"
ESP_TYPE = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
POOL_MARKER = b"GlassineOS test pool image made by tests/qemu/run.py"

# name: (the disk, the choices, cut it while the base is written and resume,
# the description's sizes scaled down (SMALL, for the 1 GiB disk), restore
# the backup afterwards). Every case backs its disk up first, as the
# installer does by default: the image holds only what is in use, so the
# 32-40 GiB disks store about their used bytes.
INSTALL_CASES = {
    "install-blank": (("nvme", "blank32"), [], False, False, False),
    "install-windows-kept": (("sata", "windows"), [], False, False, True),
    "install-resume": (("virtio", "blank32"), [], True, False, False),
    "install-backup": (("virtio", "data1g"), [], False, True, False),
}
# The description's sizes (boot, system, userdata's least) and the scaled-down ones.
FULL = (2 * GIB, 16 * GIB, 8 * GIB)
SMALL = (300 * MIB, 256 * MIB, 64 * MIB)


def small_description(out: Path):
    t = (HERE.parent.parent / "devices" / "uefi-pc.toml").read_text()
    for old, new in (('size = "2GiB"', 'size = "300MiB"'), ('size = "16GiB"', 'size = "256MiB"'), ('at_least = "8GiB"', 'at_least = "64MiB"')):
        assert old in t, old
        t = t.replace(old, new)
    out.write_text(t)


def guid_text(b: bytes) -> str:
    return (
        b[0:4][::-1].hex() + "-" + b[4:6][::-1].hex() + "-" + b[6:8][::-1].hex() + "-" + b[8:10].hex() + "-" + b[10:16].hex()
    )


def guid_bytes(g: str) -> bytes:
    h = bytes.fromhex(g.replace("-", ""))
    return h[0:4][::-1] + h[4:6][::-1] + h[6:8][::-1] + h[8:16]


def gpt(p: Path) -> list:
    """The primary table's used entries: (slot, raw 128 bytes, type, unique GUID, first, last, name)."""
    with open(p, "rb") as f:
        f.seek(512)
        h = f.read(92)
        assert h[0:8] == b"EFI PART", "no GPT"
        lba, n, size = struct.unpack_from("<QII", h, 72)
        f.seek(lba * 512)
        arr = f.read(n * size)
    out = []
    for i in range(n):
        e = arr[i * size : (i + 1) * size]
        if e[0:16] == b"\0" * 16:
            continue
        first, last = struct.unpack_from("<QQ", e, 32)
        name = e[56:128].decode("utf-16-le").rstrip("\0")
        out.append((i + 1, e, guid_text(e[0:16]), guid_text(e[16:32]), first, last, name))
    return out


def range_digest(p: Path, off: int, length: int) -> str:
    """The sha256 of a byte range of a sparse file, reading only its data."""
    h = hashlib.sha256()
    end = off + length
    with open(p, "rb") as f:
        pos = off
        while pos < end:
            try:
                start = os.lseek(f.fileno(), pos, os.SEEK_DATA)
            except OSError:
                start = end
            start = min(start, end)
            if start > pos:
                zero = start - pos
                while zero:
                    n = min(zero, 8 * MIB)
                    h.update(b"\0" * n)
                    zero -= n
            if start >= end:
                break
            stop = min(os.lseek(f.fileno(), start, os.SEEK_HOLE), end)
            f.seek(start)
            left = stop - start
            while left:
                b = f.read(min(left, 8 * MIB))
                h.update(b)
                left -= len(b)
            pos = stop
    return h.hexdigest()


def load_option(label: str, hd: tuple, path: str) -> bytes:
    """An efivarfs Boot#### file: attributes, then EFI_LOAD_OPTION with an
    HD() node (partition number, start, sectors, GPT unique GUID) and a file path."""
    num, start, sectors, guid = hd
    hdnode = struct.pack("<BBHIQQ16sBB", 4, 1, 42, num, start, sectors, guid_bytes(guid), 2, 2)
    fp = path.encode("utf-16-le") + b"\0\0"
    fnode = struct.pack("<BBH", 4, 4, 4 + len(fp)) + fp
    end = b"\x7f\xff\x04\x00"
    dp = hdnode + fnode + end
    body = struct.pack("<IH", 1, len(dp)) + (label.encode("utf-16-le") + b"\0\0") + dp
    return struct.pack("<I", 7) + body


def make_release(d: Path, files: dict, small: bool) -> dict:
    """The release folder on the stick, and its artifacts' bytes by role."""
    d.mkdir(parents=True)
    tree = d.parent / "pool-tree"
    (tree / "glassine").mkdir(parents=True, exist_ok=True)
    (tree / "glassine" / "marker").write_bytes(POOL_MARKER)
    pool = d / "base.img"
    sparse(pool, 160 * MIB)
    run("mkfs.btrfs", "-q", "-L", "glassine", "-r", tree, pool)
    shutil.copyfile(files["vmlinuz-virt"], d / "vmlinuz")
    (d / "initramfs").write_bytes(b"test initramfs\n" * 4096)
    (d / "loader.efi").write_bytes(placeholder.efi_app())
    arts = {
        "base": ("base.img", "btrfs-pool-image", 200 * MIB if small else 2 * GIB),
        "kernel": ("vmlinuz", "efi-kernel", 256 * MIB),
        "initramfs": ("initramfs", "initramfs", None),
        "boot-loader": ("loader.efi", "efi-application", None),
    }
    m, out = {}, {}
    for role, (file, kind, least) in arts.items():
        b = (d / file).read_bytes()
        out[role] = b
        e = {"file": file, "sha256": hashlib.sha256(b).hexdigest(), "size": len(b), "kind": kind}
        if least:
            e["min_partition_bytes"] = least
        if role == "kernel":
            e["cmdline"] = "console=ttyS0"
        m[role] = e
    (d / "release.json").write_text(json.dumps({"release_format": 1, "artifacts": m}, indent=1) + "\n")
    return out


def read_image(bdir: Path, file: str, disk: Path) -> dict:
    """Reads a backup image (format 2: header, then D/Z records, then E)
    from its parts, checking each data range's checksum and that it, and
    each zero range, is what the disk held. Returns its counts."""
    parts = [bdir / (file + ".gz")]
    while (bdir / f"{file}.gz.{len(parts)}").exists():
        parts.append(bdir / f"{file}.gz.{len(parts)}")
    raw = b"".join(p.read_bytes() for p in parts)
    data = gzip.decompress(raw)
    assert data[:6] == b"GLIMG\0" and struct.unpack_from("<H", data, 6)[0] == 2, "not a format-2 image"
    size = struct.unpack_from("<Q", data, 8)[0]
    out = {"disk": size, "data": 0, "zero": 0, "records": 0, "stored": len(raw), "mismatch": [], "sha256": hashlib.sha256(data).hexdigest()}
    at, nxt = 20, 0
    with open(disk, "rb") as d:
        while True:
            tag, off, length = struct.unpack_from("<cQQ", data, at)
            at += 17
            if tag == b"E":
                out["end"] = (off, length)
                break
            assert off >= nxt, f"a record out of order at {off}"
            nxt = off + length
            out["records"] += 1
            if tag == b"D":
                b = data[at : at + length]
                assert hashlib.sha256(b).digest() == data[at + length : at + length + 32], f"the range at {off}"
                at += length + 32
                out["data"] += length
                d.seek(off)
                if d.read(length) != b:
                    out["mismatch"].append(off)
            else:
                out["zero"] += length
                if not zero_range(disk, off, length):
                    out["mismatch"].append(off)
    assert at == len(data), "bytes after the end record"
    return out


def zero_range(p: Path, off: int, length: int) -> bool:
    """Whether a range of a sparse file reads as zeros: only its data regions are read."""
    end = off + length
    with open(p, "rb") as f:
        pos = off
        while pos < end:
            try:
                start = os.lseek(f.fileno(), pos, os.SEEK_DATA)
            except OSError:
                return True
            if start >= end:
                return True
            stop = min(os.lseek(f.fileno(), start, os.SEEK_HOLE), end)
            f.seek(start)
            left = stop - start
            while left:
                b = f.read(min(left, 8 * MIB))
                if b.count(0) != len(b):
                    return False
                left -= len(b)
            pos = stop
    return True


def used_bytes(facts: dict, index: int = 0) -> int:
    """The bytes the file systems of a disk say they use (inspect's facts)."""
    for d in (facts.get("storage") or {}).get("disks", []):
        if d.get("index") == index:
            return sum(((p.get("fs") or {}).get("used") or 0) for p in d.get("partitions", []))
    return 0


def make_install_stick(p: Path, binary: Path, cw: Path, files: dict, choices: list, seed: bytes = None, small: bool = False) -> dict:
    sparse(p, 1 * GIB)
    run("sgdisk", "-n1:1MiB:0", "-t1:0700", "-c1:GLINSTALL", p)
    off, size = part_start(p, 1)
    run("mkfs.fat", "-F", "32", "-n", "GLINSTALL", "--offset", off // 512, p, size // 1024)
    rel = cw / "release"
    arts = make_release(rel, files, small)
    img = f"{p}@@{off}"
    run("mcopy", "-i", img, binary, "::/glassine-install")
    desc = cw / "uefi-pc.toml"
    if small:
        small_description(desc)
    else:
        shutil.copyfile(HERE.parent.parent / "devices" / "uefi-pc.toml", desc)
    run("mcopy", "-i", img, desc, "::/uefi-pc.toml")
    run("mcopy", "-s", "-i", img, rel, "::/release")
    args = cw / "install-args"
    extra = ["--backup", "/media/stick/backup"]
    args.write_text("".join(f"{a}\n" for a in extra) + "".join(f"--choice\n{c}\n" for c in choices))
    run("mcopy", "-i", img, args, "::/install-args")
    if seed:
        # Another system's entry, first in the boot order (as Windows leaves it).
        (cw / "seed-Boot0007").write_bytes(seed)
        (cw / "seed.sh").write_text(
            f"V=/sys/firmware/efi/efivars\nG={GLOBAL}\n"
            'cat /media/stick/seed-Boot0007 > "$V/Boot0007-$G"\n'
            '{ printf \'\\007\\000\\000\\000\\007\\000\'; tail -c +5 "$V/BootOrder-$G"; } > /tmp/bo\n'
            'chattr -i "$V/BootOrder-$G" 2>/dev/null\n'
            'cat /tmp/bo > "$V/BootOrder-$G"\n'
        )
        run("mcopy", "-i", img, cw / "seed-Boot0007", "::/seed-Boot0007")
        run("mcopy", "-i", img, cw / "seed.sh", "::/seed.sh")
    return arts


def boot_vars(lines, when):
    """The guest's Boot#### and BootOrder files (hex) as it printed them."""
    out = {}
    for line in lines:
        i = line.find("@@VAR ")
        if i < 0:
            continue
        f = line[i:].split()
        if len(f) >= 3 and f[1] == when:
            out[f[2]] = f[3] if len(f) > 3 else ""
    return out


def run_guest(args, boot, vars_fd, disks, stick, mode, log, kill_at=None):
    g = Guest(qemu_cmd(args, boot["kernel"], boot["initrd"], vars_fd, disks, stick, mode), log)
    try:
        g.until("@@READY", args.boot_timeout)
        g.send("go\n")
        if kill_at:
            lines = g.until(kill_at, args.run_timeout)
            g.p.kill()
            g.p.wait()
            return lines
        lines = g.until("@@DONE", args.run_timeout)
        g.send("off\n")
        g.stop(120)
        return lines
    except Exception:
        g.p.kill()
        raise


def install_case(name, args, boot, files, work: Path):
    print(f"case {name}:", flush=True)
    (bus, what), choices, cut, small, restore = INSTALL_CASES[name]
    sizes = SMALL if small else FULL
    cw = work / name
    if cw.exists():
        shutil.rmtree(cw)
    cw.mkdir(parents=True)
    disk = cw / f"disk0-{what}.img"
    build_disk(what, disk, cw)
    before = gpt(disk) if what == "windows" else []
    kept = {e[0]: (e[1], range_digest(disk, e[4] * 512, (e[5] - e[4] + 1) * 512)) for e in before}
    seed = None
    if what == "windows":
        esp = before[0]
        seed = load_option("Windows Boot Manager", (1, esp[4], esp[5] - esp[4] + 1, esp[3]), "\\EFI\\Microsoft\\Boot\\bootmgfw.efi")
    disk_sha = range_digest(disk, 0, disk.stat().st_size)
    original = cw / "disk-before.img"
    if restore:
        subprocess.run(["cp", "--sparse=always", str(disk), str(original)], check=True)
    stick = cw / "stick.img"
    arts = make_install_stick(stick, args.binary, cw, files, choices, seed, small)
    vars_fd = cw / "OVMF_VARS.fd"
    shutil.copyfile(args.ovmf_vars, vars_fd)
    disks = [(bus, disk)]

    if cut:
        # Cut while the base is written: QEMU is killed, as a power cut would.
        lines = run_guest(args, boot, vars_fd, disks, stick, "install", cw / "serial-install.log", kill_at="write-base: 30%")
        first = boot_vars(lines, "before")
        check(not any("@@END log-install" in x for x in lines), "the install was cut while it wrote the base")
        lines = run_guest(args, boot, vars_fd, disks, stick, "resume", cw / "serial-resume.log")
        rec = records(lines)
        code, text = rec.get("log-resume", (None, ""))
        check(code == 0, f"resume finished the install: exit {code}\n{(text or '')[-3000:]}")
    else:
        lines = run_guest(args, boot, vars_fd, disks, stick, "install", cw / "serial-install.log")
        first = boot_vars(lines, "before")
        rec = records(lines)
        code, text = rec.get("log-install", (None, ""))
        check(code == 0, f"install finished: exit {code}\n{(text or '')[-3000:]}")
    after = boot_vars(lines, "after")
    for k in ("facts-before", "facts-after"):
        if k in rec:
            (cw / f"{k}.json").write_text(json.dumps(rec[k][1], indent=1) + "\n")
    # the stick: the record, its journal
    rec_file = cw / "record.json"
    subprocess.run(["mcopy", "-n", "-i", f"{stick}@@{1 * MIB}", "::/rec/record.json", rec_file], check=False)
    record = json.loads(rec_file.read_text()) if rec_file.exists() else {}
    # The backup: the disk as it was, only what is in use, its manifest last.
    bdir = cw / "backup"
    bdir.mkdir()
    subprocess.run(["mcopy", "-n", "-s", "-i", f"{stick}@@{1 * MIB}", "::/backup", str(cw)], check=False)
    man = json.loads((bdir / "manifest.json").read_text()) if (bdir / "manifest.json").exists() else {"disks": [{}]}
    file = str(man["disks"][0].get("file", "disk?.img"))
    if check(len(man["disks"]) == 1 and (bdir / (file + ".gz")).exists(), f"the backup holds the disk's image and its manifest: {sorted(p.name for p in bdir.iterdir())}"):
        m = (bdir / "manifest.json").read_bytes()
        check(json.loads(m).get("backup_format") == 2, "backup format 2")
        check((record.get("backup") or {}).get("manifest_sha256") == hashlib.sha256(m).hexdigest(), "the record rests on that backup")
        ref = original if restore else None
        if ref is None:
            # the disk as it was is gone (installed over); its bytes outside
            # GlassineOS's partitions are unchanged, checked below by the image
            ref = disk
        im = read_image(bdir, file, ref)
        check(json.loads(m)["disks"][0]["sha256"] == im["sha256"], "its manifest names the stored stream's sha256")
        check(im["end"] == (im["data"], im["records"]), f"the image's end record matches what it holds: {im['end']}")
        used = used_bytes(rec["facts-before"][1]) if "facts-before" in rec and rec["facts-before"][1] else 0
        print(f"  @@SIZES {name}: disk {im['disk']} bytes, file systems use {used}, image holds {im['data']} bytes of data "
              f"+ {im['zero']} of zeros in {im['records']} records, stored {im['stored']} bytes compressed", flush=True)
        check(im["data"] <= used + 64 * MIB, f"the image stores about the used bytes ({im['data']} for {used} used)")
        if restore:
            check(not im["mismatch"], f"every range of the image is the disk as it was: {im['mismatch'][:5]}")
        else:
            # installed over: only GlassineOS's partitions changed since
            t = gpt(disk)
            ours = [(e[4] * 512, (e[5] + 1) * 512) for e in t if e[6].startswith("glassine-")]
            outside = [o for o in im["mismatch"] if not any(a - 64 * MIB <= o < b for a, b in ours) and o >= 1 * MIB and o < im["disk"] - 1 * MIB]
            check(not outside, f"the image's ranges outside GlassineOS's partitions and tables are the disk's: {outside[:5]}")
        if what == "data1g":
            check(im["data"] >= 8 * MIB, "the 8 MiB of data outside any partition are in the image")
    steps = [e["step"] for e in record.get("journal", []) if (e.get("outcome") or {}).get("result") == "done"]
    check(steps[:2] == ["back-up", "create-partitions"], f"the backup was done before the table was written: {steps}")
    if cut:
        interrupted = [e for e in record.get("journal", []) if (e.get("outcome") or {}).get("result") == "interrupted"]
        check(any(e["step"] == "write-base" for e in interrupted), f"the record holds write-base as cut, run again: {[e['step'] for e in interrupted]}")

    # the disk: GlassineOS's three partitions
    t = gpt(disk)
    by = {e[6]: e for e in t}
    ok = check(all(n in by for n in ("glassine-boot", "glassine-system", "glassine-userdata")), f"the three partitions are there: {[e[6] for e in t]}")
    if not ok:
        return
    bootp, sysp, datap = by["glassine-boot"], by["glassine-system"], by["glassine-userdata"]
    check(bootp[2] == ESP_TYPE and sysp[2] == ROOT_X86 and datap[2] == LINUX_DATA, "their types: ESP, x86-64 root, Linux data")
    check((bootp[5] - bootp[4] + 1) * 512 == sizes[0] and (sysp[5] - sysp[4] + 1) * 512 == sizes[1], f"the boot and system partitions have the description's sizes {sizes[:2]}")
    check((datap[5] - datap[4] + 1) * 512 >= sizes[2], "userdata has at least its least size")
    with open(disk, "rb") as f:
        f.seek(sysp[4] * 512)
        got = f.read(len(arts["base"]))
    check(got == arts["base"], "the system partition holds the release's pool image byte for byte")
    check(POOL_MARKER in got, "and in it the pool's marker file")
    check(range_digest(disk, datap[4] * 512, MIB) == hashlib.sha256(b"\0" * MIB).hexdigest(), "userdata is left unformatted for the first start")

    # the boot partition: a sound FAT, the loader at both paths, the records
    bootimg = cw / "boot.img"
    run("dd", f"if={disk}", f"of={bootimg}", "bs=4M", f"skip={bootp[4] * 512}", f"count={(bootp[5] - bootp[4] + 1) * 512}",
        "iflag=skip_bytes,count_bytes", "conv=sparse", stderr=subprocess.DEVNULL)
    fsck = subprocess.run(["fsck.fat", "-n", "-v", str(bootimg)], capture_output=True, text=True)
    check(fsck.returncode == 0, f"fsck.fat finds the boot partition sound: {fsck.returncode} {fsck.stdout[-800:]} {fsck.stderr[-400:]}")

    def fat_file(path):
        out = cw / ("fat-" + path.replace("/", "_"))
        r = subprocess.run(["mcopy", "-n", "-i", str(bootimg), "::/" + path, str(out)], capture_output=True)
        return out.read_bytes() if r.returncode == 0 and out.exists() else None

    check(fat_file("EFI/glassine/loader.efi") == arts["boot-loader"], "the loader at \\EFI\\glassine\\loader.efi")
    check(fat_file("EFI/BOOT/BOOTX64.EFI") == arts["boot-loader"], "and at the removable-media path")
    lr = (fat_file("glassine/loader.rec") or b"").decode()
    ident = f"glassine.system=PARTUUID={sysp[3]} glassine.userdata=PARTUUID={datap[3]}"
    check(ident in lr, f"the kernel set's command line names the system and userdata by PARTUUID: {lr}")
    kid = next((x[8:] for x in lr.splitlines() if x.startswith("default=")), "")
    check(fat_file(f"glassine/{kid}/vmlinuz") == arts["kernel"], "the first kernel set's kernel")
    check(fat_file(f"glassine/{kid}/initrd") == arts["initramfs"], "and its initramfs")
    bootimg.unlink()

    # Windows kept: its entries and partitions byte for byte
    if what == "windows":
        for slot, (raw, h) in kept.items():
            e = next((x for x in t if x[0] == slot), None)
            check(e is not None and e[1] == raw, f"partition {slot}'s table entry unchanged")
            check(e is not None and range_digest(disk, e[4] * 512, (e[5] - e[4] + 1) * 512) == h, f"partition {slot}'s bytes unchanged")
        check(bootp[4] * 512 >= max(e[5] for e in before) * 512, "GlassineOS is in the free space after Windows")

    # the firmware: one new entry, first; every other entry and its place kept
    bo = f"BootOrder-{GLOBAL}"
    order = lambda vs: [int.from_bytes(bytes.fromhex(vs.get(bo, ""))[4:][i : i + 2], "little") for i in range(0, max(0, len(bytes.fromhex(vs.get(bo, ""))) - 4), 2)]
    o1, o2 = order(first), order(after)
    new = [k for k in after if k not in first]
    check(len(new) == 1 and new[0].startswith("Boot"), f"one new boot entry: {new}")
    if seed:
        check(first.get(f"Boot0007-{GLOBAL}") == seed.hex(), "the seeded Windows entry was there before")
        check(o1[:1] == [7], f"and first in the order before: {o1}")
    for k, v in first.items():
        if k != bo:
            check(after.get(k) == v, f"{k} unchanged")
    ours = int(new[0][4:8], 16) if new else None
    check(o2 == [ours] + [n for n in o1 if n != ours], f"ours first, the others in their order: {o1} -> {o2}")
    if new:
        ob = bytes.fromhex(after[new[0]])
        check("GlassineOS".encode("utf-16-le") in ob and "\\EFI\\glassine\\loader.efi".encode("utf-16-le") in ob, "our entry: GlassineOS, \\EFI\\glassine\\loader.efi")
        check(guid_bytes(bootp[3]) in ob, "on the boot partition (by its unique GUID)")

    # the machine starts from its disk alone (the stick taken out)
    g = Guest(qemu_cmd(args, None, None, vars_fd, disks, None, None), cw / "serial-boot.log")
    try:
        lines = g.until((placeholder.MARKER, "Access Denied", "No bootable"), args.boot_timeout)
        try:
            g.stop(60)
        except RuntimeError:
            pass
    finally:
        g.p.kill()
        g.p.wait()
    said = [x for x in lines if "BdsDxe" in x]
    want = f'starting Boot{ours:04X} "GlassineOS"' if ours is not None else "?"
    check(any(want in x for x in said), f"the firmware starts our entry first: {said}")
    check(any(placeholder.MARKER in x for x in lines), "and it starts the placeholder loader (its serial marker)")

    if restore:
        # Back to how it was: restore --pc from the stick's backup.
        lines = run_guest(args, boot, vars_fd, disks, stick, "restore", cw / "serial-restore.log")
        rr = records(lines)
        code, text = rr.get("log-restore", (None, ""))
        check(code == 0, f"restore finished: exit {code}\n{(text or '')[-3000:]}")
        print("  " + "\n  ".join(x for x in (text or "").splitlines() if x.startswith(("restored", "backup", "  holds"))), flush=True)
        check(range_digest(disk, 0, disk.stat().st_size) == disk_sha, "the restored disk is the disk as it was, byte for byte")
        check(gpt(disk) == before, "with its partition table")
        restored = boot_vars(lines, "after")
        check(order(restored) == o1, f"the boot order is the one it had: {o1} -> {order(restored)}")
        for k, v in first.items():
            check(restored.get(k) == v, f"{k} as it was")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", type=Path, required=True, help="the static glassine-install")
    ap.add_argument("--work", type=Path, required=True, help="where the disks and logs go")
    ap.add_argument("--cache", type=Path, required=True, help="where the downloads are kept")
    ap.add_argument("--ovmf-code", type=Path)
    ap.add_argument("--ovmf-vars", type=Path)
    ap.add_argument("--ovmf-secboot-code", type=Path, help="OVMF's Secure Boot build (needs SMM)")
    ap.add_argument("--ovmf-secboot-vars", type=Path, help="an empty variable store for it")
    ap.add_argument("--boot-timeout", type=int, default=600)
    ap.add_argument("--run-timeout", type=int, default=600)
    ap.add_argument("cases", nargs="*", default=list(CASES) + list(INSTALL_CASES))
    args = ap.parse_args()
    if not args.ovmf_code:
        for c, v in OVMF_CANDIDATES:
            if Path(c).exists() and Path(v).exists():
                args.ovmf_code, args.ovmf_vars = Path(c), Path(v)
                break
        else:
            sys.exit("no OVMF found: pass --ovmf-code and --ovmf-vars")
    secure = SECURE & set(args.cases)
    if secure and not args.ovmf_secboot_code:
        for c, v in OVMF_SECBOOT_CANDIDATES:
            if Path(c).exists() and Path(v).exists():
                args.ovmf_secboot_code, args.ovmf_secboot_vars = Path(c), Path(v)
                break
        else:
            sys.exit("no OVMF Secure Boot build found: pass --ovmf-secboot-code and --ovmf-secboot-vars")
    tools = ["qemu-system-x86_64", "sgdisk", "mkfs.ext4", "mkfs.fat", "mcopy", "unsquashfs", "mkntfs", "ntfscp"]
    if INSTALL_CASES.keys() & set(args.cases):
        tools += ["mkfs.btrfs", "fsck.fat"]
    if secure:
        tools += ["mmd", "openssl", "sbsign", "virt-fw-vars"]
    for tool in tools:
        if not shutil.which(tool):
            sys.exit(f"{tool} is missing")
    print("kvm:", "yes" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "no (tcg, slow)", flush=True)
    args.work.mkdir(parents=True, exist_ok=True)
    files = fetch(args.cache)
    boot = {"kernel": files["vmlinuz-virt"], "initrd": initramfs(files, args.work)}
    sb = secure_boot(args, files, args.work) if secure else None
    for name in args.cases:
        try:
            if name == "secure-boot-medium":
                medium(args, sb, args.work)
            elif name in INSTALL_CASES:
                install_case(name, args, boot, files, args.work)
            else:
                case(name, args, boot, args.work, sb)
        except Exception as e:
            check(False, f"case {name}: {e}")
        for p in (args.work / name).glob("*.img"):
            p.unlink()
    if FAILURES:
        print(f"\n{len(FAILURES)} failed:")
        for f in FAILURES:
            print("  " + f)
        sys.exit(1)
    print("\nall cases passed")


if __name__ == "__main__":
    main()
