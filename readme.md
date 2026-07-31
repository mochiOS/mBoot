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

このファイルは次のどちらかの方法で mBoot に渡します。

```sh
# mBoot リポジトリ直下へ、既定名で配置する
cp ../mochiOS/out/artifacts/disk.img ./mochiOS.img
make run

# または元の場所を明示する
make run MOCHIOS=../mochiOS/out/artifacts/disk.img
```

必要条件は以下のとおりです。

- QEMU の raw block device として渡せる通常ファイルまたは物理ディスク
- GPT パーティションテーブルを持つこと
- 64 MiB 以上で、mBoot から読み書きできること
- OVMF/UEFI で単独起動できる完全な mochiOS ディスクであること

`make run` はこのイメージを `MOCHIOS` という virtio disk serial 付きで
接続します。そのため、開発用 QEMU テストではパーティション名を変更する
必要はありません。物理ディスクを接続する場合の識別方法は
「mochiOS ディスクの識別」を参照してください。

## ビルドと起動

```sh
make defconfig   # configs/mboot_x86_64_defconfig を適用
make build       # output/images/disk.img を生成
make check       # 設定とシェルスクリプトの回帰検査
make check-image # 完成したイメージ、GRUB、カーネル設定を検査
make run         # mBoot と ./mochiOS.img をホスト QEMU で起動
```

GUI を使わずシリアルログだけを確認する場合は、次のように実行します。

```sh
make run QEMU_DISPLAY=none
```

`make run` は利用可能なら KVM、利用できなければ TCG を選択します。
生成された `output/images/disk.img` を専用の USB メモリまたは SSD に
書き込むと、物理マシンで起動できます。Secure Boot には対応していないため、
ファームウェア設定で無効にしてください。

## mochiOS ディスクの識別

mBoot は候補を Linux の `/dev/vdb` のような不安定な名前では識別しません。
次のいずれかのメタデータを持つ、ちょうど1台の raw GPT ディスクを選択します。

- GPT パーティション名が `MOCHIOS`
- GPT パーティション type GUID が
  `4d4f4348-494f-5300-a11e-000000000001`
- パーティションの filesystem label が `MOCHIOS`
- ディスク全体の udev short serial が `MOCHIOS`
- カーネル引数で永続 selector を明示

カーネル引数の例:

```text
mboot.disk=/dev/disk/by-id/ata-example
mboot.disk=PARTUUID=01234567-89ab-cdef-0123-456789abcdef
```

物理 mochiOS ディスクでは、既存パーティションの GPT 名または filesystem
label を `MOCHIOS` にする方法が簡単です。マーカーがパーティションにあっても、
QEMU へ渡されるのはその親ディスク全体です。

mBoot は次の候補を拒否します。

- mBoot 自身の root disk
- 複数の一致候補
- マウント中、swap 使用中、または holder があるディスク
- 読み書きできないディスク
- 64 MiB 未満または GPT でないディスク

ランチャーは QEMU の全実行期間にわたって排他ロックを保持します。

## 実行時設定とログ

root 所有の `/etc/mboot.conf` で、vCPU、メモリ上限、Q35/PC、virtio GPU、
SDL 全画面、user networking、ALSA 音声、disk cache を設定できます。
CPU とメモリを空欄にすると、Linux 用の予約分を残して自動算出します。

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
