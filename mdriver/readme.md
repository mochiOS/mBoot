# mDriver

mDriverは、mochiOSの物理デバイスを動かすHardware Domainです。Linuxカーネルを使いますが、Linuxデスクトップとしては動かしません。ログイン画面、シェル、パッケージ管理機能は入れず、mBootから割り当てられたデバイスだけを扱います。

Linuxを作り、長い時間をかけて育ててきた皆さんに感謝します。mDriverは、その仕事がなければ作れません。

## ビルド

```sh
./setup.sh
make -C mdriver build
```

詳しいビルド方法、構成、ライセンスは[mochiOSのmDriverドキュメント](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/mdriver.md)にまとめています。物理ストレージを割り当てる前には、[ストレージの安全条件](https://github.com/mochiOS/mochiOS/blob/master/docs/mboot/storage.md)も確認してください。
