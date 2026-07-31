# mBoot

mBoot は、物理 x86_64 PC 上で mochiOS を全画面 QEMU 仮想マシンとして
起動する Buildroot ベースの専用 Linux アプライアンスです。mBoot 自体は
BIOS/UEFI の両方から起動でき、物理 GPU、入力、ストレージ、音声、
ネットワークを Linux が担当します。mochiOS 側には安定した virtio
プラットフォームを提供します。

## 必要な mochiOS 成果物

mBoot に必要なのは `kernel.elf` や `initfs` の単体ファイルではありません。
**mochiOS のブートローダー、カーネル、initfs、パーティションテーブルを
すべて含んだ、起動可能な raw GPT ディスクイメージ**が必要です。

現在の mochiOS リポジトリでは、通常これは次の成果物です。

```text
mochiOS/out/artifacts/disk.img
```

このファイルはビルド時に mBoot の root filesystem へ
`/var/lib/mboot/mochiOS.img` として格納されます。実機では mBoot と mochiOS を
別々のディスクへ書き込む必要はありません。

```sh
# mBoot リポジトリ直下へ、既定名で配置する
cp ../mochiOS/out/artifacts/disk.img ./mochiOS.img
make defconfig
make build

# または元の場所を明示する
make build MOCHIOS=../mochiOS/out/artifacts/disk.img
```

必要条件は以下のとおりです。

- QEMU の raw block device として渡せる通常ファイル
- GPT パーティションテーブルを持つこと
- 64 MiB 以上で、ビルド時に読み取れること
- OVMF/UEFI で単独起動できる完全な mochiOS ディスクであること

`make build` はファイルの存在、最小サイズ、GPT header を検査します。完成後の
mBoot は内包したファイルを `MOCHIOS` という virtio disk serial 付きで mochiOS
へ渡します。GPTパーティション名やfilesystem labelの変更は不要です。

## ビルドと起動

```sh
make defconfig                                  # 初回だけ設定を生成
make build MOCHIOS=../mochiOS/out/artifacts/disk.img
make check
make check-image MOCHIOS=../mochiOS/out/artifacts/disk.img
make run MOCHIOS=../mochiOS/out/artifacts/disk.img
```

`make build` は次の2つを生成します。内容は同一です。

```text
output/images/disk.img   通常のraw GPTディスクイメージ
output/images/mboot.iso  USB書き込みツール向けの配布名
```

`mboot.iso` は、CD/DVD用の読み取り専用ISO9660ではありません。mochiOSのディスク
内容やOVMF状態を実機で永続化するため、BIOS/UEFI両対応の書き込み可能なraw GPT
イメージを`.iso`という名前でも出力しています。

GUI を使わずシリアルログだけを確認する場合は、次のように実行します。

```sh
make run QEMU_DISPLAY=none
```

`make run` は完成した1台のmBootディスクだけを外側のQEMUへ接続します。内側の
mochiOSが自動起動するため、単一ディスク構成をそのまま仮想環境で確認できます。
利用可能ならKVM、利用できなければTCGを選択します。

## USBまたはSSDから実機起動

`output/images/mboot.iso`または`disk.img`を、ファイルとしてコピーするのではなく、
USBメモリやSSDの**デバイス全体**へディスクイメージとして書き込みます。4 GiB
以上の専用媒体を推奨します。書き込み先の既存データは消去されます。

書き込んだ1台だけを実機へ接続し、BIOSまたはUEFIから起動してください。別の
mochiOS用ディスクや`MOCHIOS`ラベルは不要です。Secure Bootには対応していない
ため、ファームウェア設定で無効にしてください。

## 実行時設定とログ

root 所有の `/etc/mboot.conf` で、vCPU、メモリ上限、Q35/PC、virtio GPU、
SDL 全画面、user networking、ALSA 音声、disk cache を設定できます。
mochiOSは現在マルチコア未対応のため、既定値は`MBOOT_VCPUS=1`です。メモリを
空欄にするとLinux用の予約分を残して自動算出します。

ログは次に保存され、再起動後も残ります。

```text
/var/log/mboot/launcher.log
/var/log/mboot/xorg.log
/var/log/mboot/qemu.log
/var/log/mboot/mochios.log
```

mochiOS が正常終了すると mBoot も電源を切ります。初期化失敗や QEMU の
異常終了時はログインシェルを開かず、tty1 に短いエラーコードとログ位置を
表示します。

詳しい構成は [docs/architecture.md](docs/architecture.md)、検証項目は
[docs/test-plan.md](docs/test-plan.md)、同梱 OVMF の由来は
[docs/ovmf.md](docs/ovmf.md) を参照してください。
