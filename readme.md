# mBoot

mBootは、LinuxのKVMとQEMUを利用してmochiOSを起動するための専用ブート環境です。

## 構成

mBootは主に以下の要素で構成されます。

* Buildrootによる最小Linux環境
* KVM
* QEMU
* OVMF
* virtioによる仮想デバイス
* mochiOSの起動と監視を行う管理サービス

## mochiOSに提供するデバイス

* virtio-blk
* virtio-net
* virtio-gpu
* virtio-input
* virtio-rng
* virtio-serial
* ACPI

## 起動の流れ

1. 実機のUEFIからmBootを起動
2. mBoot Linuxが起動
3. 管理サービスがQEMUを起動
4. QEMUがKVMを利用してmochiOSを実行
5. mochiOSの画面を全画面で表示

## 管理サービス

mBootの管理サービスは、以下を担当します。

* mochiOSの起動
* QEMUプロセスの監視
* 終了状態の確認
* クラッシュログの保存
* 再起動処理
* シャットダウン処理
* 起動失敗回数の管理

## mochiOSとの連携

mochiOSとmBootの間では、virtio-serialを利用して制御メッセージを送受信します。

メッセージ一覧は以下の通りです。

* SHUTDOWN
* REBOOT
* BOOT_SUCCESS
* HOST_VERSION
* GUEST_REBOOT
* RECOVERY_MODE
