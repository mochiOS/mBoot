# mDriver

mDriverは、mochiOSの物理デバイスを動かすHardware Domainです。Linuxカーネルを使いますが、Linuxデスクトップとしては動かしません。ログイン画面、シェル、パッケージ管理機能は入れず、mBootから割り当てられたデバイスだけを扱います。

Linuxを作り、長い時間をかけて育ててきた皆さんに感謝します。mDriverは、その仕事がなければ作れません。

## ビルド

mDriverはBuildroot 2025.02.16で作ります。Buildroot、Linux 6.12.98、Rustのツールチェーンと依存ファイルは、mBootのセットアップからまとめて取得できます。

```sh
./setup.sh
make -C mdriver build
```

`make image`などでmBootを初めてビルドするときも、セットアップは自動で走ります。`make clean`の後は、次のビルド時にもう一度確認します。取得済みのファイルはハッシュを確認して再利用するため、毎回ダウンロードし直すことはありません。

Buildrootの版、Linuxの版、kernel設定、mBoot用パッチはリポジトリで固定しています。ダウンロードしたアーカイブはSHA-256が一致したものだけを使います。必要なソースを先にすべて取得するため、一度セットアップできれば、次のコマンドでネットワークを使わないビルドも確認できます。

```sh
make -C mdriver build-offline
```

成果物は`mdriver/output/artifacts/`へ作ります。

| ファイル | 内容 |
|---|---|
| `vmlinux` | mBootがPVH形式で読み込むLinux kernelです |
| `initramfs.cpio` | PID 1だけを収録した最小initramfsです |
| `linux.config` | 実際に使ったkernel設定です |

mBootイメージへ収録するときは、次のように渡します。

```sh
make image \
  CONFIG=config/qemu-mdriver.toml \
  MDRIVER_KERNEL=mdriver/output/artifacts/vmlinux \
  MDRIVER_INITRAMFS=mdriver/output/artifacts/initramfs.cpio
```

今のmDriverは、mBoot上でPVH起動してPID 1へ到達するところまでを担当します。物理PCIデバイスをLinuxの通常のPCI層へ接続するguest driverと、mochiOS向けvirtio backendは次に実装します。

## ライセンス

Linux kernelと`board/mdriver/patches/linux/`の変更はGPL-2.0-onlyです。`init/init.c`は独立したuserspaceプログラムで、Apache-2.0です。Linuxのsyscall境界には明示的な例外があり、userspaceプログラムへkernelのGPLが自動的に及ぶ扱いにはなっていません。

Linux kernelバイナリを配布するときは、対応する完全なソースも提供します。Buildrootの次のターゲットで、ライセンス文書と対応するソースをまとめます。

```sh
make -C mdriver legal-info
```

配布時は`output/legal-info/`の内容と最終`linux.config`を、`vmlinux`と同じ場所から取得できるようにします。詳しい区分は[licensing.md](licensing.md)に記録しています。
