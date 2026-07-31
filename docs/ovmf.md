# OVMF の由来と更新手順

`board/mboot/rootfs-overlay/usr/share/mboot` にある2つの firmware file は、
2026-07-31 の調査時点で Ubuntu `ovmf-generic` package
`2025.11-3ubuntu7` の内容と byte 単位で一致しました。

| ファイル | サイズ | SHA-256 |
| --- | ---: | --- |
| `OVMF_CODE_4M.fd` | 3,653,632 bytes | `a5708766c49ee39db0f4e7e53d73376e2dbc0d45bf12501c0977c48412bf8902` |
| `OVMF_VARS_4M.fd` | 540,672 bytes | `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |

OVMF は TianoCore EDK II project から生成されます。EDK II source の中心的な
license は BSD-2-Clause-Patent ですが、同梱物すべての最終的な license 判断には
downstream package の copyright metadata も確認してください。

code image は QEMU へ read-only で渡します。VARS image は template としてのみ
使用し、原本を直接変更しません。ランチャーは mochiOS disk の GPT UUID ごとに
private copy を作成します。

firmware を更新する場合は、次の作業が必要です。

1. 配布元と license metadata を確認する
2. この文書の package version、サイズ、SHA-256 を更新する
3. `make check` と `make check-image` を実行する
4. OVMF で mBoot を起動する
5. 内側の mochiOS も OVMF から起動することを確認する
