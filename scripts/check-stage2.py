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
    parser.add_argument("--cpu", choices=["cortex-a53", "cortex-a57", "max"], default="cortex-a57")
    parser.add_argument("--out", type=Path, default=MBOOT / "out/mdp/stage2")
    parser.add_argument("--system-elf", type=Path,
                        help="also enter a real native System Domain ELF through its Ready HVC")
    parser.add_argument("--boot-module", type=Path,
                        help="continue the System Domain with a real selected boot module")
    parser.add_argument("--abi-rlib", type=Path,
                        help="matching AArch64 mnu-abi rlib for --system-elf without --production-library")
    parser.add_argument("--production-library", action="store_true",
                        help="link the complete native mBoot crate using Cargo (cached dependencies required)")
    parser.add_argument("--target-dir", type=Path,
                        help="reuse a Cargo target directory with --production-library")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    try:
        qemu = shutil.which(args.qemu)
        if not qemu:
            raise RuntimeError(f"required executable not found: {args.qemu}")
        binary = out / "stage2.elf"
        build = ["rustc", f"+{args.toolchain}", "--edition", "2021", "--crate-type", "bin",
             "--target", "aarch64-unknown-none", "-C", "panic=abort", "-C", "opt-level=2",
             "-C", f"link-arg=-T{MBOOT / 'scripts/stage2-probe.ld'}",
             str(MBOOT / "scripts/stage2-probe.rs"), "-o", str(binary)]
        environment = dict(os.environ)
        if args.boot_module:
            if not args.system_elf or not args.production_library or args.cpu != "cortex-a53":
                raise RuntimeError("--boot-module requires --system-elf, --production-library and cortex-a53")
            environment["MBOOT_SYSTEM_MODULE"] = str(args.boot_module.resolve(strict=True))
        if args.system_elf:
            if not args.abi_rlib and not args.production_library:
                raise RuntimeError("--system-elf requires --abi-rlib from the same toolchain")
            if not args.production_library:
                build += ["--cfg", "system_domain_probe", "--extern",
                          f"mnu_abi={args.abi_rlib.resolve()}"]
            environment["MBOOT_SYSTEM_ELF"] = str(args.system_elf.resolve())
        if args.production_library:
            target_dir = (args.target_dir or out / "cargo").resolve()
            build = ["cargo", f"+{args.toolchain}", "rustc", "--offline",
                     "-Z", "build-std=core,alloc,compiler_builtins", "--manifest-path",
                     str(MBOOT / "Cargo.toml"), "--features", "arm-diagnostics",
                     "--bin", "mboot-stage2-probe", "--target", "aarch64-unknown-none",
                     "--target-dir", str(target_dir), "--",
                     "-C", f"link-arg=-T{MBOOT / 'scripts/stage2-probe.ld'}"]
            if args.system_elf:
                build += ["--cfg", "system_domain_probe"]
            if args.boot_module:
                build += ["--cfg", "system_domain_boot"]
            environment.update(CARGO_INCREMENTAL="0", CARGO_PROFILE_DEV_DEBUG="0",
                               CARGO_PROFILE_DEV_OPT_LEVEL="2")
            environment.setdefault("CARGO_BUILD_JOBS", "2")
            environment.setdefault("TMPDIR", str(out))
            run(build, out / "build.log", 300, env=environment)
            shutil.copy2(target_dir / "aarch64-unknown-none/debug/mboot-stage2-probe", binary)
        else:
            run(build, out / "build.log", 60, env=environment)
        # ELF direct boot leaves x0 unspecified. Use the standard AArch64
        # Image protocol to exercise the real firmware DTB handoff instead.
        libdir = subprocess.check_output(
            ["rustc", f"+{args.toolchain}", "--print", "target-libdir"], text=True,
        ).strip()
        objcopy = Path(libdir).parent / "bin/llvm-objcopy"
        if not objcopy.is_file():
            raise RuntimeError(f"llvm-tools component required for {args.toolchain}")
        image = out / "stage2.Image"
        run([str(objcopy), "-O", "binary", str(binary), str(image)],
            out / "image.log", 30, env=environment)
        run([str(Path(qemu).resolve()), "-machine", "virt-8.2,virtualization=on,gic-version=2",
             "-cpu", args.cpu, "-accel", "tcg", "-m", "512M" if args.boot_module else "128M", "-smp", "1",
             "-display", "none", "-monitor", "none", "-nic", "none", "-serial", "stdio",
             "-no-reboot", "-kernel", str(image), "-semihosting-config", "enable=on,target=native"],
            out / "run.log", 15, env=dict(os.environ, TMPDIR=str(out)), marker="STAGE2 PASS aarch64")
    except (OSError, RuntimeError) as error:
        parser.exit(1, f"Stage-2 check failed: {error}\n")
    print(f"aarch64 Stage-2 ({args.cpu}): PASS ({out / 'run.log'})")


if __name__ == "__main__":
    main()
