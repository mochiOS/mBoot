# mBoot アーキテクチャ

## リポジトリと履歴の調査結果

調査対象の先頭コミットは `d398810` です。過去の構成は Buildroot の
`qemu_x86_64_defconfig`、次に `pc_x86_64_efi_defconfig` を利用し、その後
外部設定 `mboot_x86_64_defconfig` が追加されていました。しかし従来の外部設定は
カーネル設定とイメージ生成処理に `board/pc` を参照しており、
`board/mboot` がビルドの正本になっていませんでした。

現在は Buildroot 2025.02.16 と mBoot 所有の設定だけを使用します。
`.gitmodules` は存在しますが gitlink が履歴にないため、`make setup` は既存の
Buildroot checkout を保護しつつ、必要な場合だけ指定バージョンを取得します。

旧ランタイムには `:0.0 vt07`、固定3秒待機、`/dev/vdb`、固定4 vCPU/4 GiB、
`killall` による停止、共有 OVMF VARS などの固定的な処理がありました。
これらは現在の実装では使われません。

## ビルドの正本

`make defconfig` は `BR2_EXTERNAL` を指定し、常に
`configs/mboot_x86_64_defconfig` を選択します。この設定から以下の mBoot
所有ファイルを参照します。

- `board/mboot/linux.config`
- `board/mboot/rootfs-overlay`
- `board/mboot/post-build.sh` と `post-image.sh`
- BIOS/EFI 用 GRUB 設定
- `board/mboot/genimage.cfg`
- GPU firmware、Mesa、Xorg、SDL2、libinput、ALSA、QEMU
- S40 Xorg サービスと S90 mBoot ランチャー

Buildroot 2025.02 は target QEMU の音声 backend を無効化するため、
`external.mk` の configure wrapper で ALSA を明示的に有効化し、
`alsa-lib` を依存関係へ加えています。

Xorg 21.1 の modular modesetting/fbdev driver が暗黙に期待する provider は、
post-build 処理で ELF dependency として明示します。これにより module の
即時 binding 時に失敗せず、通常の GPU fallback を開始できます。

## BIOS/UEFI ディスク構成

生成される `disk.img` は次の GPT 構成です。

1. 1 MiB の BIOS Boot Partition
2. 64 MiB の EFI System Partition
3. 2 GiB の ext4 root partition

GPT disk UUID と各 partition UUID は mBoot 所有の固定値です。BIOS と UEFI の
GRUB は Linux device name ではなく、root partition UUID
`6d426f6f-7400-4b00-8a00-000000000001` を使用します。

`grub-bios-setup` は host block device を要求するため、post-image 処理は
GRUB PC-BIOS の sector field を検査可能な固定位置へ設定し、生成済み core を
連続した BIOS Boot Partition に直接配置します。loop device や root 権限は
不要で、protective MBR の partition table も保持されます。

## 起動時の処理

1. BusyBox init と eudev が物理ハードウェアを初期化します。root account は
   lock され、getty、SSH、bridge、外部公開サービスはありません。
2. S40 が VT2 を予約して Xorg を起動し、`xset` の実応答を最大30秒待ちます。
   stale PID/socket を処理し、Xorg 自身のログを保存します。
3. S90 がランチャーを起動して正確な PID を記録します。停止時に signal を
   送る対象は記録済みランチャーとその QEMU だけです。
4. ランチャーが GPU、DRM、display を記録し、Xorg を確認します。
5. メタデータに一致する mochiOS whole disk を1台だけ選び、検証して lock します。
6. OVMF code/template を検証し、mochiOS disk の GPT UUID ごとに永続 VARS を
   作ります。破損ファイルは `.invalid.TIMESTAMP` として保存してから再生成します。
7. host CPU とメモリから上限付き resource を算出します。使い捨て QEMU probe で
   KVM 初期化を確認できた場合だけ `kvm`/`host` を選び、それ以外は
   `tcg`/`max` に fallback します。
8. mochiOS を Q35、OVMF、raw virtio-blk、virtio GPU/input/rng/network、
   SDL/X11 全画面、任意の ALSA HDA で起動します。QEMU monitor は無効です。
9. QEMU の全引数と PID を記録します。正常終了時は mBoot を poweroff し、
   異常終了時は tty1 と永続ログへ分類済みエラーを残します。

## 対応ハードウェア範囲

Linux 6.12.98 では EFI、ACPI、PCIe、AHCI、NVMe、USB 2/3 HID/storage、PS/2、
VT、evdev、uinput、ext4、FAT、efivars、Intel/AMD KVM・IOMMU、thermal、
cpufreq、watchdog、power control を有効化しています。

DRM は i915、amdgpu、nouveau、virtio、vmwgfx、simpledrm、EFI/VESA framebuffer、
QXL、bochs を含みます。Mesa/Xorg には Intel、AMD、nouveau、VMware、fbdev、
modesetting、virgl、software rendering path を含めています。

NVIDIA proprietary driver は必須ではありません。対応できる GPU では nouveau、
それ以外では simpledrm/EFI framebuffer と fbdev を表示優先の fallback とします。

## 永続データと既知の制約

root filesystem は writable です。主な書き込み先は次のとおりです。

- `/var/log/mboot`: Xorg、launcher、QEMU、mochiOS serial log
- `/var/lib/mboot`: mochiOS disk ごとの OVMF VARS
- `/run/mboot`: PID、lock、一時エラー

QEMU は raw physical disk と `/dev/kvm` を機種に依存しない形で扱うため、現状は
root で動作します。monitor、host port forward、login console、network daemon は
公開しません。Secure Boot と host network の自動構成は対象外です。
