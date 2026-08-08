# 同梱OVMF firmware

`board/mboot/rootfs-overlay/usr/share/mboot/`にあるOVMF codeとvariable templateは、
mochiOSを起動する内側QEMUだけで使用します。mBoot自身をUSBから起動する物理PCの
firmwareではありません。

code fileはread-onlyで使用します。variable templateは実行前に`/var/lib/mboot`
へcopyし、repository内の原本を変更せずfirmware stateを永続化します。
`scripts/check-image.sh`は完成root filesystemに両fileが存在することを検証します。

更新する場合はQEMUの4 MiB pflash layoutと互換性のあるOVMF buildを使用し、codeと
variable templateを同時に更新します。更新後は`make build`を再実行します。OVMFの
更新はmBoot root partition欠落の回避策にはなりません。
