# mDriverのライセンス

ここでは、mDriverを配布するときに守る区分を記録します。法的な助言ではありません。配布方法や組み込むfirmwareを変えたときは、もう一度確認してください。

## Linux kernel

Linux kernelはGPL-2.0-onlyです。`board/mdriver/patches/linux/`にあるmBoot対応コードもkernelへ組み込むため、同じGPL-2.0-onlyにしています。

`vmlinux`を配布するときは、そのバイナリを作るために使ったLinuxソース、mBoot対応パッチ、最終`.config`を提供します。mDriverでは`make legal-info`がBuildrootのライセンス資料と対応するソースを`output/legal-info/`へまとめます。書面によるソース提供の約束に頼らず、バイナリと同じ場所からソースを取得できる配布方法を選びます。

## PID 1

`init/init.c`はApache-2.0です。Linuxのsyscallだけを使う独立したuserspaceプログラムとしてビルドします。Linux kernelの公式文書ではsyscall interfaceを明確な境界として扱い、UAPIには`Linux-syscall-note`例外があります。

`init/init.c`とApache-2.0のライセンス文も`output/legal-info/`へ収録します。将来、GPL-only kernel symbolを使うkernel moduleを追加する場合、そのmoduleをApache-2.0のままにはしません。kernel内で動くmBoot guest driverはGPL-2.0-onlyにします。

## mBootとmochiOS

mBoot、mochiOS、mDriver kernelは別々の実行ファイルです。mBootはLinux kernelをリンクせず、別Domainへ読み込んで仮想CPUを開始します。通信もHypercall、共有ページ、Event Channelを通します。この構成では、同じディスクイメージへ収録しても別プログラムの集合として扱えると判断しています。

mBootとmochiOSのライセンスをGPLへ変更する予定はありません。ただし、Linux由来コードをmBoot本体へコピーした場合や、両者を同じアドレス空間でリンクする構成へ変えた場合、この判断は使えません。

## firmware

現在のmDriver成果物には外部firmwareを収録しません。Wi-FiやGPU用firmwareを追加するときは、各firmwareの再配布条件を個別に記録します。Linux kernelがGPLだからといって、firmwareを無条件に再配布できるわけではありません。

確認に使った一次資料:

- [Linux kernel licensing rules](https://www.kernel.org/doc/html/next/process/license-rules.html)
- [GNU GPL FAQ: Mere Aggregation](https://www.gnu.org/licenses/gpl-faq.en.html#MereAggregation)
