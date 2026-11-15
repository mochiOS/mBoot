#!/usr/bin/env python3
"""Run actual EL1 accesses through mBoot's ARM EL2 Stage-2 mapper.

Instruction/protection diagnostic on a fixed QEMU fixture, not a full OS boot
or performance measurement. Existing OS/KVM launch settings are unchanged.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess

MBOOT = Path(__file__).resolve().parents[1]


def run(command, log, timeout, env=None, marker=None):
    try:
        result = subprocess.run(command, cwd=MBOOT, env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        log.write_bytes(error.stdout or b"")
        raise RuntimeError(f"timed out after {timeout}s; see {log}") from error
    log.write_bytes(result.stdout)
    if result.returncode or (marker and marker.encode() not in result.stdout):
        raise RuntimeError(f"exit {result.returncode} or missing success marker; see {log}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", default="nightly-2026-05-14")
    parser.add_argument("--qemu", default="qemu-system-aarch64")
    parser.add_argument("--cpu", choices=["cortex-a57", "max"], default="cortex-a57")
    parser.add_argument("--out", type=Path, default=MBOOT / "out/mdp/stage2")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    try:
        qemu = shutil.which(args.qemu)
        if not qemu:
            raise RuntimeError(f"required executable not found: {args.qemu}")
        binary = out / "stage2.elf"
        run(["rustc", f"+{args.toolchain}", "--edition", "2021", "--crate-type", "bin",
             "--target", "aarch64-unknown-none", "-C", "panic=abort", "-C", "opt-level=2",
             "-C", f"link-arg=-T{MBOOT / 'scripts/stage2-probe.ld'}",
             str(MBOOT / "scripts/stage2-probe.rs"), "-o", str(binary)], out / "build.log", 60)
        run([str(Path(qemu).resolve()), "-machine", "virt-8.2,virtualization=on,gic-version=2",
             "-cpu", args.cpu, "-accel", "tcg", "-m", "128M", "-smp", "1",
             "-display", "none", "-monitor", "none", "-nic", "none", "-serial", "stdio",
             "-no-reboot", "-kernel", str(binary), "-semihosting-config", "enable=on,target=native"],
            out / "run.log", 15, env=dict(os.environ, TMPDIR=str(out)), marker="STAGE2 PASS aarch64")
    except (OSError, RuntimeError) as error:
        parser.exit(1, f"Stage-2 check failed: {error}\n")
    print(f"aarch64 Stage-2 ({args.cpu}): PASS ({out / 'run.log'})")


if __name__ == "__main__":
    main()
