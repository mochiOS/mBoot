# mBoot

mBootは、mnuのABIを使ってmochiOSとmDriverを動かすx86_64向けType-1ハイパーバイザーです。UEFIから直接起動します。LinuxをホストOSとして起動する旧方式は、このリポジトリから削除しました。

Intel CPUではVMXとEPT、AMD CPUではSVMとNPTを使います。mBootが管理するのはCPU、RAM、IOMMU、PCIデバイス、割り込み、Domain間の共有ページです。ファイルシステム、ネットワーク、GUI、一般的なデバイスドライバは持ちません。

## ディレクトリ

| 場所 | 内容 |
|---|---|
| `mboot/` | ハイパーバイザー本体です |
| `config/` | Domain、メモリ、Capability、PCI割り当てを記述します |
| `scripts/` | Launch Manifestと起動イメージを作り、QEMUで検査します |
| `firmware/` | QEMUテストで使うOVMFです。実機イメージには入りません |

`mboot-protocol`と`mbootd`はありません。どちらもLinuxホストからmochiOSのQEMUを操作するためのコードだったため、Type-1 mBootには不要です。

## ビルド

mBoot単体リポジトリの隣にmnuを置いた場合は、次のコマンドで`output/mochiOS.img`を作れます。

```sh
make image
```

mnuが別の場所にある場合は明示します。

```sh
make image MNU_DIR=/path/to/mnu CONFIG=config/intel-hardware.toml
```

`mochiOS.img`は、USBメモリへそのまま書き込めるraw GPTディスクイメージです。EFI System PartitionにはmBoot、Launch Manifest、設定で選んだDomainイメージが入ります。

mochiOSワークスペースからは、ルートで`make mboot`を実行します。通常の出力先は`out/mochiOS.img`です。

## mDriver

mDriverはmBootの外でビルドします。mBootは完成済みのkernelとinitramfsを受け取ります。

mDriverを含む設定でイメージを作る場合は、2つの成果物を渡します。

```sh
make image \
  CONFIG=config/qemu-mdriver.toml \
  MDRIVER_KERNEL=/path/to/vmlinux \
  MDRIVER_INITRAMFS=/path/to/initramfs.cpio
```

指定したファイルがない場合、mBootは代わりのLinuxを自動生成せず、その場でエラーにします。古いBuildrootの出力を黙って使うこともありません。

詳しい説明は、mochiOSリポジトリの[mBootについて](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/about.md)、[mDriver](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/mdriver.md)、[物理ストレージ](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/storage.md)、[OVMF](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/ovmf.md)にまとめています。

## 確認

ホスト上の単体テストは次のコマンドで実行します。

```sh
make test
```

QEMUで起動まで確認する場合は、CPU仮想化を使える環境で実行します。

```sh
make image-test MNU_DIR=/path/to/mnu
make qemu-test MNU_DIR=/path/to/mnu
make device-io-test MNU_DIR=/path/to/mnu
```

`config/intel-hardware.toml`はIntel実機向け、`config/qemu.toml`は通常のDomain起動試験向けです。`config/qemu-device-io.toml`はPCI、DMA、IOMMU、MSI-Xの試験に使います。

## 起動時の表示

mBootは画面とシリアルへ進行状況を出します。Intelでは`INTEL VMX`、AMDでは`AMD SVM`と表示します。mochiOS System Domainまで起動すると`MOCHIOS OK`、Hardware Domainの準備が終わると`Hardware OK`になります。

赤い画面に`ERROR`が出た場合は、番号と直前のシリアルログを確認してください。`VMCS`の後ろに出る4桁の値は、Intel VMCSへ書き込めなかったフィールド番号です。
