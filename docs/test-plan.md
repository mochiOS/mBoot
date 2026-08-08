# mBoot検証計画

## 自動検証

`make check`はsource設定、生成設定、shell syntax、必要なbuiltin kernel設定、
有限のroot待機、host identityを変えても生成結果が同一であることを検証します。

成功するすべての`make build`は`scripts/check-image.sh`を実行します。次のいずれかが
成立しない場合、buildを失敗させます。

- GPT main/backup metadataが正しい
- 期待するdisk GUIDとroot PARTUUIDが一意に存在する
- root partitionの範囲とx86-64 root typeが正しい
- 完成disk内のroot partitionが検証対象ext4とbyte単位で一致する
- ext4が読み取り可能で、UUID/typeが正しい
- BIOS GRUBとEFI-stub kernelのcommand lineが一致する
- BIOS/UEFI boot fileが存在する
- 同梱mochiOS imageが入力imageと一致する
- 必要なservice、QEMU、Xorg、OVMF、Intel DMC firmwareが存在する
- rootがlockされ、gettyと意図しない一般userが存在しない
- hostnameが`mboot`で、build hostのidentity/pathが混入していない
- root用storage/filesystem driverがkernel builtinである
- `mbootd`が開発hostのdynamic loaderへ依存しない

`make protocol-test`はmBoot control protocolとdaemon transportを検証します。
`make check-reproducible MOCHIOS=<disk.img>`はimage生成を2回実行し、完成した
`disk.img`のSHA-256が同一であることを要求します。

## QEMU回帰検証

再buildせず完成diskを検証する場合は次を実行します。

```sh
make run-built QEMU_DISPLAY=none QEMU_ACCELERATOR=kvm
```

KVMを使用できないhostでは`QEMU_ACCELERATOR=tcg`を別のfallback検証として
使用します。Linuxがext4 rootをmountし、Xorgとmbootdを起動して、同梱した
mochiOS imageを起動することを確認します。QEMUでの成功は、物理USB controllerと
storage deviceの動作を証明するものではありません。

USB root経路は完成ディスクをxHCIおよびEHCI配下のUSB Mass Storageとして接続して
検証します。

```sh
make check-qemu-usb QEMU_ACCELERATOR=kvm
```

この検証はfirmwareによるUSB image選択、xHCI/EHCI enumeration、SCSI diskの
`/dev/sda`登録、GPT root PARTUUID、ext4 mount、内側mochiOSのuserspace到達を
ログから検証します。
物理controller、USB stick固有のquirk、実機firmwareの挙動は引き続き実機検証が必要です。

## 実機USB検証

`output/images/mboot.iso`をpartitionではなく、検証専用USB device全体へ書き込み
ます。UEFIと、対応機ではlegacy BIOSの両方から起動し、serial logまたは画面全体の
写真を保存します。

userspace到達後に次を確認します。

```sh
cat /proc/cmdline
findmnt -no SOURCE,FSTYPE /
cat /etc/hostname
cat /etc/passwd
lsblk -o NAME,TYPE,FSTYPE,UUID,PARTUUID
```

serial consoleを保存できない実機では、停止後にUSBのroot partitionを別のLinuxへ
mountし、`/var/log/mboot/boot.log`、`kernel.log`、`launcher.log`を回収します。
`kernel.log`はXorgとmBoot launcherの開始後に保存したkernel ring bufferで、i915
firmware、storage、ACPIに関するwarningの分類に使用します。

`/`は`/proc/cmdline`に記載されたPARTUUIDのpartitionからmountされ、filesystemは
ext4、hostnameは`mboot`でなければなりません。開発host由来の一般userが存在しては
なりません。

診断のnegative testでは、root PARTUUIDだけを意図的に無効化した検証用imageを
使用します。有限時間で待機を終了し、検出partitionとVFS mount errorを表示して
panicすることを確認します。この変更済みimageは配布しません。

## Warningの分類

Intel i915を有効にしているため、選択したlinux-firmwareから
`i915/tgl_dmc_ver2_12.bin`を収録し、image検査でも必須とします。i915、amdgpu、
nouveauはroot mount前にfirmwareを要求しないようmoduleとし、S10udevのcoldplugで
rootfs上のfirmwareを利用してからS40xorgを起動します。DMC firmwareの欠落warningは
mBootのpackaging failureです。Transparent Hugepageは機能を有効にし、不要な常時
割り当てを避けるため既定を`madvise`とします。

ACPI table/methodを示すBIOS errorは物理firmwareに由来する場合があります。logを
隠したりfilterしたりせず、root filesystem、display、input、mochiOS起動がすべて
成功した場合にのみ非致命的なfirmware問題として分離します。その際もhardware
model、firmware revision、完全なlogを保存します。storage reset、I/O error、
root PARTUUID欠落、VFS mount error、firmware file欠落はmBoot failureです。
