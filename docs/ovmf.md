# OVMF

`firmware/`のOVMFはQEMUテストでだけ使います。mBootの実機向けイメージには収録しません。実機ではPCのUEFIファームウェアが`EFI/BOOT/BOOTX64.EFI`を読み込みます。

OVMFを更新するときは、QEMUの4 MiB pflash配置と互換性があるcodeとvariable templateを同時に差し替えます。差し替えたあとは`make image-test`と`make qemu-test`を実行します。
