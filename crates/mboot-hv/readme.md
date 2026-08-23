# mBoot Hypervisor

このcrateは、LinuxをホストOSとして使わない新しいmBootの起動部分です。現在使われているLinuxベースのmBootとは別のバイナリとしてビルドされます。

UEFIから起動し、ファームウェアのメモリマップを引き継いだあと、mBoot専用のGDTとIDTへ切り替えます。Intel CPUではVMX、AMD CPUではSVMを有効にします。Domainには2 MiBのRAMを割り当て、IntelではEPT、AMDではNPTを使ってmBootのメモリから隔離します。

ESPの`\EFI\MBOOT\MNU.ELF`からmnuのDomain用ELFを読み込みます。mBootがDomain内のページテーブルと`DomainBootInfo`を用意し、mnuを64ビットモードで起動します。現在使えるHypercallは`ConsoleWrite`、`Yield`、`Shutdown`です。AMD CPUでは、`ConsoleWrite`の処理後に同じvCPUを再開し、`Shutdown`を受け取るところまでKVMで確認しています。Intel CPU用にも同じ経路がありますが、Intel実機での動作確認はまだです。

現在の`crates/mboot-hv`は、LinuxベースのmBootと並行して開発するための一時的な置き場所です。移行時にはLinuxベースの構成を取り除き、mnuと同じように、リポジトリ直下の`mboot`をハイパーバイザー本体にします。既存のmBootイメージはまだこのバイナリへ切り替えません。

ホスト上の単体テストは`make hv-test`、UEFIバイナリは`make hv-build`で生成します。mnuのDomain用ELFは`make hv-domain-build`で生成します。KVMでmnuの起動まで確認するときは`make hv-qemu-test`を実行します。IntelホストではVMX、AMDホストではSVMが選ばれます。
