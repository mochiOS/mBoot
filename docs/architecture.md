# mBootイメージ構成

## 起動設定の正本

disk GUID、partition GUID、root filesystem UUID/type、root deviceの待機時間、
hostname、kernel consoleは`board/mboot/boot-layout.conf`だけで定義します。
`scripts/generate-boot-config.sh`は値を検証し、次の設定を
`output/generated/`へ生成します。

- Buildroot defconfig
- Linux kernel config
- BIOS GRUB menu
- GRUB組み込み探索設定
- genimage disk layout

生成fileは直接編集しません。元設定が変わると`make configure`がdefconfigを
再適用します。これによりGPT root PARTUUID、BIOS GRUB、EFI-stub kernelの
command lineが常に一致します。

`USER`、`LOGNAME`、`HOME`、build hostのhostnameは生成入力に使用しません。
`BR2_REPRODUCIBLE=y`によりBuildrootのtimestampとbuild pathも正規化します。

## Disk layout

配布成果物は、次の順序を持つraw GPT diskです。

1. GRUB core imageを格納する1 MiBのBIOS Boot Partition
2. Linux EFI-stub kernelを格納する64 MiBのEFI System Partition
3. mBootのext4 root partition

3番目のpartitionにはx86-64 Linux root partition type GUIDを使用します。
`mboot.iso`と`disk.img`はbyte単位で同一の書き込み可能disk imageです。`.iso`は
USB書き込みtoolとの互換性のための拡張子であり、ISO9660ではありません。

BIOSでは最初のpartitionからGRUBを起動し、filesystem UUIDでext4を探索して
`/boot/bzImage`を読み込みます。UEFIでは同じ`bzImage`を
`EFI/BOOT/BOOTX64.EFI`として直接起動します。kernelは生成済みcommand lineを
強制するため、どちらの経路でも同じroot PARTUUIDを選択します。

## Root device障害時の動作

kernelは設定されたroot PARTUUIDを有限時間だけ待ち、無期限の`rootwait`は
使用しません。serial consoleとlocal consoleをinfo log levelで有効にしています。
root deviceが見つからない場合、Linuxは期待したroot識別子、mount error、検出済み
partitionを出力してからVFS panicへ進みます。無言で永久停止することはありません。

root mount前に必要なGPT parser、SCSI disk、xHCI/EHCI/OHCI/UHCI、USB mass
storage/UAS、ext4はすべてkernel builtinです。userspaceへ到達するためにinitramfsや
loadable moduleを必要としません。

## Root filesystemのidentity

Buildrootは`/etc/hostname`へ`mboot`を書き込みます。interactive gettyは無効、
root passwordはlockされ、一般userは作成しません。package所有のsystem accountは
維持します。`mbootd`はstatic PIEとして構築し、build PCのdynamic loaderやlibc
versionへ依存しません。GDB helperなどbuild pathを含む不要な開発fileもrootfsへ
収録しません。
