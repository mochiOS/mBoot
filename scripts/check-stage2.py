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
import uuid

MBOOT = Path(__file__).resolve().parents[1]


def run(command, log, timeout, env=None, marker=None):
    try:
        # Keep serial/build output inspectable during bring-up, including a
        # hung guest. A timeout must never erase the output already written.
        with log.open("wb") as output:
            result = subprocess.run(command, cwd=MBOOT, env=env, stdout=output,
                                    stderr=subprocess.STDOUT, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f"timed out after {timeout}s; see {log}") from error
    if result.returncode or (marker and marker.encode() not in log.read_bytes()):
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
    parser.add_argument("--hardware-image", type=Path, help="enter the actual ARM64 mDriver Linux Image")
    parser.add_argument("--hardware-initrd", type=Path, help="actual mDriver initramfs for --hardware-image")
    parser.add_argument("--hardware-boot-disk", action="store_true", help="explicit RAM-disk payload profile (initrd must contain the real disk)")
    parser.add_argument("--firmware-ramfb", action="store_true", help="explicit QEMU reference firmware framebuffer (no GPU/DMA assignment)")
    parser.add_argument("--qemu-firmware-dir", type=Path, help="directory containing the real vgabios-ramfb.bin")
    parser.add_argument("--timeout", type=int, default=15, help="bounded QEMU run in seconds (1..1800; long native OS diagnostics are explicit)")
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
        if args.timeout < 1 or args.timeout > 1800:
            raise RuntimeError("--timeout must be between 1 and 1800 seconds")
        if args.firmware_ramfb and (not args.hardware_boot_disk or not args.qemu_firmware_dir
                                     or not (args.qemu_firmware_dir / "vgabios-ramfb.bin").is_file()):
            raise RuntimeError("--firmware-ramfb requires --hardware-boot-disk and a real --qemu-firmware-dir")
        if args.hardware_boot_disk and not (args.hardware_image and args.system_elf and args.boot_module):
            raise RuntimeError("--hardware-boot-disk requires both actual Domain images and boot-module")
        if args.hardware_image:
            if not args.production_library or args.cpu != "cortex-a53" or not args.hardware_initrd:
                raise RuntimeError("--hardware-image requires production-library, cortex-a53 and hardware-initrd")
            if args.system_elf and not args.boot_module:
                raise RuntimeError("paired Domains require a real boot-module")
            environment["MBOOT_HARDWARE_IMAGE"] = str(args.hardware_image.resolve(strict=True))
            environment["MBOOT_HARDWARE_INITRD"] = str(args.hardware_initrd.resolve(strict=True))
        elif args.hardware_initrd:
            raise RuntimeError("--hardware-initrd requires --hardware-image")
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
            if args.hardware_image:
                build += ["--cfg", "hardware_domain_probe"]
                if args.system_elf:
                    build += ["--cfg", "paired_domains"]
                if args.hardware_boot_disk:
                    build += ["--cfg", "hardware_boot_disk"]
                if args.firmware_ramfb:
                    build += ["--cfg", "firmware_ramfb"]
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
        command = [str(Path(qemu).resolve()), "-machine", "virt-8.2,virtualization=on,gic-version=2",
             "-cpu", args.cpu, "-accel", "tcg", "-m", "2G" if args.hardware_boot_disk else "512M" if args.boot_module or args.hardware_image else "128M", "-smp", "1",
             "-display", "none", "-monitor", "none", "-nic", "none", "-serial", "stdio",
             "-no-reboot", "-kernel", str(image), "-semihosting-config", "enable=on,target=native"]
        if args.firmware_ramfb:
            monitor = out / f"monitor-{uuid.uuid4().hex[:12]}.qmp"
            (out / "monitor-path.txt").write_text(str(monitor) + "\n")
            command += ["-L", str(args.qemu_firmware_dir.resolve()), "-device", "ramfb",
                        "-qmp", f"unix:{monitor},server=on,wait=off"]
        run(command, out / "run.log", args.timeout, env=dict(os.environ, TMPDIR=str(out)), marker="STAGE2 PASS aarch64")
    except KeyboardInterrupt:
        parser.exit(130, f"ARM diagnostic interrupted; existing logs remain in {out}\n")
    except (OSError, RuntimeError) as error:
        parser.exit(1, f"Stage-2 check failed: {error}\n")
    print(f"aarch64 Stage-2 ({args.cpu}): PASS ({out / 'run.log'})")


if __name__ == "__main__":
    main()
