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
  the release's least system partition (the release manifest fixture on the
  stick) is refused with the space it lacks;
- a control run that writes one disk byte and one variable is caught.

Needs: qemu-system-x86_64, OVMF, sgdisk, mkfs.ext4, mkfs.fat, mcopy,
unsquashfs, sha256sum. Downloads (pinned, hash-checked) go to --cache.
Runs without KVM if there is none (slower)."""

import argparse
import hashlib
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
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


def ntfs_boot_sector(sectors: int) -> bytes:
    """An NTFS boot sector: the signature only (no MFT behind it)."""
    b = bytearray(512)
    b[0:3] = b"\xeb\x52\x90"
    b[3:11] = b"NTFS    "
    b[11:13] = (512).to_bytes(2, "little")
    b[13] = 8
    b[0x28:0x30] = sectors.to_bytes(8, "little")
    b[0x30:0x38] = (4).to_bytes(8, "little")
    b[0x40] = 0xF6  # -10: 1024-byte MFT records
    b[510:512] = b"\x55\xaa"
    return bytes(b)


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


def make_linux(p: Path, work: Path):
    sparse(p, 24 * GIB)
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


def make_windows(p: Path):
    sparse(p, 24 * GIB)
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
    for n in (3, 4):
        off, size = part_start(p, n)
        put(p, off, ntfs_boot_sector(size // 512))


def make_stick(p: Path, binary: Path):
    sparse(p, 256 * MIB)
    run("sgdisk", "-n1:1MiB:0", "-t1:0700", "-c1:GLINSTALL", p)
    off, size = part_start(p, 1)
    run("mkfs.fat", "-F", "32", "-n", "GLINSTALL", "--offset", off // 512, p, size // 1024)
    run("mcopy", "-i", f"{p}@@{off}", binary, "::/glassine-install")
    run("mcopy", "-i", f"{p}@@{off}", RELEASE, "::/release.json")


# Each disk: (file, how QEMU attaches it, its size, what is on it).
CASES = {
    "blank": [("nvme", "blank16")],
    "linux": [("virtio", "linux")],
    "windows": [("sata", "windows")],
    "small": [("virtio", "small")],
    "two-disks": [("nvme", "linux"), ("sata", "blank16")],
    "control": [("virtio", "blank16")],
}
KIND = {"nvme": "nvme", "virtio": "virtio", "sata": "sata"}


def build_disk(what: str, p: Path, work: Path):
    if what == "blank16":
        make_blank(p, 16 * GIB)
    elif what == "small":
        make_blank(p, 4 * GIB)
    elif what == "linux":
        make_linux(p, work)
    elif what == "windows":
        make_windows(p)


def sha(paths):
    """sha256 of each file, in parallel."""
    procs = {p: subprocess.Popen(["sha256sum", str(p)], stdout=subprocess.PIPE, text=True) for p in paths}
    return {p: pr.communicate()[0].split()[0] for p, pr in procs.items()}


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
        """The lines before the one starting with `marker`."""
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
            if marker in line:
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


def qemu_cmd(args, kernel, initrd, vars_fd, disks, stick, mode):
    cmd = [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-m", "1024",
        "-smp", "2",
        "-accel", "kvm", "-accel", "tcg",
        "-display", "none", "-monitor", "none", "-serial", "stdio",
        "-no-reboot",
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={args.ovmf_code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_fd}",
        "-kernel", str(kernel), "-initrd", str(initrd),
        "-append", f"console=ttyS0 quiet loglevel=1 panic=-1 glassine.disks={len(disks) + 1} glassine.mode={mode}",
        "-device", "qemu-xhci,id=xhci",
        "-drive", f"if=none,id=stick,format=raw,file={stick}",
        "-device", "usb-storage,bus=xhci.0,drive=stick,removable=on",
        "-device", "ahci,id=ahci",
    ]
    for i, (bus, p) in enumerate(disks):
        cmd += ["-drive", f"if=none,id=d{i},format=raw,file={p}"]
        if bus == "nvme":
            cmd += ["-device", f"nvme,drive=d{i},serial=glassine-ci-{i}"]
        elif bus == "sata":
            cmd += ["-device", f"ide-hd,drive=d{i},bus=ahci.{i}"]
        else:
            cmd += ["-device", f"virtio-blk-pci,drive=d{i}"]
    return cmd


def case(name, args, boot, work: Path):
    print(f"case {name}:", flush=True)
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
    shutil.copyfile(args.ovmf_vars, vars_fd)
    images = [p for _, p in disks] + [stick]
    before_boot = sha(images)

    mode = "control" if name == "control" else "check"
    g = Guest(qemu_cmd(args, boot["kernel"], boot["initrd"], vars_fd, disks, stick, mode), cw / "serial.log")
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
    check(fw.get("secure_boot") == "off", f"secure boot: {fw.get('secure_boot')}")
    check(facts.get("cpu", {}).get("arch") == "x86-64", "x86-64")
    by_kind = {}
    for d in st["disks"]:
        if d["kind"] != "usb":
            by_kind.setdefault(d["kind"], []).append(d)
    for bus, _ in disks:
        check(KIND[bus] in by_kind, f"a {KIND[bus]} disk is listed")

    chk = rec.get("check", (None, None))[1] or {}
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
            # The boot partition's fixed 1 GiB and the release's least system partition.
            least = json.loads(RELEASE.read_text())["artifacts"]["base"]["min_partition_bytes"]
            needs, free = GIB + least, d["free"][0] if d["free"] else 0
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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", type=Path, required=True, help="the static glassine-install")
    ap.add_argument("--work", type=Path, required=True, help="where the disks and logs go")
    ap.add_argument("--cache", type=Path, required=True, help="where the downloads are kept")
    ap.add_argument("--ovmf-code", type=Path)
    ap.add_argument("--ovmf-vars", type=Path)
    ap.add_argument("--boot-timeout", type=int, default=600)
    ap.add_argument("--run-timeout", type=int, default=600)
    ap.add_argument("cases", nargs="*", default=list(CASES))
    args = ap.parse_args()
    if not args.ovmf_code:
        for c, v in OVMF_CANDIDATES:
            if Path(c).exists() and Path(v).exists():
                args.ovmf_code, args.ovmf_vars = Path(c), Path(v)
                break
        else:
            sys.exit("no OVMF found: pass --ovmf-code and --ovmf-vars")
    for tool in ["qemu-system-x86_64", "sgdisk", "mkfs.ext4", "mkfs.fat", "mcopy", "unsquashfs", "sha256sum"]:
        if not shutil.which(tool):
            sys.exit(f"{tool} is missing")
    print("kvm:", "yes" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "no (tcg, slow)", flush=True)
    args.work.mkdir(parents=True, exist_ok=True)
    files = fetch(args.cache)
    boot = {"kernel": files["vmlinuz-virt"], "initrd": initramfs(files, args.work)}
    for name in args.cases:
        try:
            case(name, args, boot, args.work)
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
