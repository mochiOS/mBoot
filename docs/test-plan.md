# 検証計画

基本手順は `make check`、`make defconfig`、`make build`、対象となる実行時試験の
順です。物理試験では mBoot の4種類のログ、使用した image、commit ID、機種を
記録してください。設定が存在するだけでは物理試験の合格とはしません。

## 自動・仮想環境での試験

- [x] `make check`: 所有 path、必須 Buildroot/kernel symbol、shell syntax、
  固定 device name や広範囲な process kill がないことを確認
- [x] `make check-image`: GPT、BIOS GRUB、ESP、ext4 root、実行ファイル、
  firmware、最終 kernel config を確認
- [x] `make run MOCHIOS=/path/to/mochiOS/out/artifacts/disk.img`:
  Xorg を起動し、virtio GPU/input/network 付きで mochiOS を自動起動
- [x] `/dev/kvm` を利用できない host で外側の QEMU が TCG を自動選択
- [ ] mochiOS disk を外し、shell ではなく `DISK_NOT_FOUND` を表示
- [ ] marker 付き disk を2台接続し、`DISK_MULTIPLE` を表示
- [ ] 候補 disk を mount または holder 使用中にして `DISK_BUSY` を表示
- [ ] Xorg を停止し、1回だけ再起動を試した後に `XORG_FAILED` を表示
- [ ] 不正な QEMU を代入し、`QEMU_START_FAILED` を表示
- [ ] OVMF VARS copy を truncate し、退避後に再生成されることを確認
- [x] 同じ `disk.img` を OVMF/UEFI と SeaBIOS の両方で起動
- [ ] 4 GiB と 8 GiB の host で Linux 用メモリ予約量を確認

チェック済みの仮想試験は 2026-07-31 に実施しました。UEFI と SeaBIOS の双方で
Linux 6.12.98 へ到達し、Xorg modesetting が 1280x800 の virtio display を初期化、
metadata detector が serial `MOCHIOS` の whole disk を選択しました。disk ごとの
OVMF state が作成され、内側の mochiOS は `Kernel initialization complete` まで
到達しました。

## 物理マシン試験（未検証）

- [ ] Intel integrated GPU、native mode、libinput keyboard/mouse/touchpad
- [ ] AMD GPU、native mode、音声 device がない場合の graceful fallback
- [ ] NVIDIA GPU の nouveau、および利用不能時の simpledrm fallback
- [ ] USB keyboard と mouse
- [ ] PS/2 keyboard と mouse
- [ ] AHCI disk と NVMe disk
- [ ] Intel KVM と AMD KVM
- [ ] 別々の実機で Legacy BIOS と UEFI 起動
- [ ] 複数 monitor で default screen の全画面表示と mode change
- [ ] mochiOS の ACPI shutdown 後に物理 host が poweroff
- [ ] 強制的な QEMU failure 後も tty error と永続 log を読めること

物理項目は、機種、実施日、採取した証拠を記録するまで合格として報告しません。
