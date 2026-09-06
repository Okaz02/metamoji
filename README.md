# MetaMoJi — 軽量デスクトップ手書きノート

Android 版 **MetaMoJi Share Classroom** が端末・用途に対して重すぎたため、
**デスクトップ向けに軽量に作り直す**ことを目的としたリポジトリ群の親オーガナイゼーション。

商用製品のバイナリ (`apk/`) を逆解析して仕様を起こし、その仕様に基づいて
Tauri + React + Rust の軽量な代替実装を組み立てる。元アプリの重さの主因
(約29,000クラスの巨大 APK、プロプライエタリな手書き認識エンジン mazec、
課金・ライセンス、動画/音声の ffmpeg 依存) は意図的に継承しない。

## 背景

| | 元アプリ (Android) | 本プロジェクト |
|---|---|---|
| 実行環境 | スマートフォン/タブレット | PC デスクトップ (Tauri v2) |
| サイズ | 約29,000クラスの巨大 APK | Rust + TypeScript の最小構成 |
| 手書き認識 | mazec (プロプライエタリ) | 筆圧つきストロークの取得・保存のみ |
| サーバー | MetaMoJi 社のクラウド | MetaMoJi 社サーバーへの互換サインイン + 自前参照バックエンド |
| 課金・ライセンス | 同社の商流 | 実装しない |

## リポジトリ構成

| ディレクトリ | 内容 |
|---|---|
| [`metamoji/`](metamoji/) | **デスクトップアプリ本体**。Tauri v2 + React 19 + TypeScript + Vite、Rust バックエンド (SQLite、`.atdoc` インポータ、教室ソケット) |
| [`metamoji-api/`](metamoji-api/) | **クラウドAPIの TypeScript SDK** (`@metamoji/sdk`)。`docs/typespec` の全131オペレーションを網羅した Resend 風クライアント (submodule) |
| [`server/`](server/) | **参照バックエンド**。クラウド同期 (スコープ B) と教室協働 (スコープ C) の自前実装 (SQLite + Bun、クラスタなし) |
| [`docs/`](docs/) | **解析ドキュメント**。apktool で展開した APK から起こしたアーキテクチャ・プロトコル・フォーマット仕様、および TypeSpec の API 仕様 |
| [`apk/`](apk/) | 解析対象 APK (`com.metamoji.share_classroom` 3.15.1.0) の展開物 (submodule) |
| `.github/` | CI・リリースワークフロー |

`metamoji-api/` と `apk/` は git submodule。`git submodule update --init` で取得する。

## クイックスタート

```bash
git submodule update --init --recursive
bun install --cwd metamoji
bun run --cwd metamoji tauri dev      # アプリ本体
bun run --cwd server start            # 参照バックエンド (http://localhost:8787)
```

セットアップの詳細は各 README を参照:

- アプリ本体: [`metamoji/README.md`](metamoji/README.md)
- API SDK: [`metamoji-api/README.md`](metamoji-api/README.md)
- 参照バックエンド: [`server/README.md`](server/README.md)
- 解析・仕様: [`docs/README.md`](docs/README.md)

## 実装スコープ

- **A. スタンドアロン・ノートアプリ** — 手書き(筆圧)・消しゴム・図形・テキスト・付箋・画像・
  レーザーポインタ・複数ページ/レイヤー・PDF/`.atdoc` 入出力・ライブラリ(フォルダ/タグ/検索)。
  詳細は [`metamoji/FEATURES.md`](metamoji/FEATURES.md)。
- **サインイン・教室** — MetaMoJi 社サーバーへの互換サインイン (学校ID/簡易/QR)、
  クラスボックス・ルーム・中継ソケット。実サーバー検証は学校アカウント未取得のため未実施。
- **B. クラウド同期 / C. 教室協働** — 自前参照バックエンド (`server/`) 向け。
  競合はサーバー優先 + 複製保存、Direction はサーバー側重複排除付き。
  現時点では UI から到達しない (コードとテストは残っている)。

## 方針・ルール

- **軽量を優先**。重いものを移植せず、効果のある部分だけを取り出す。
- ワイヤ形式・保存形式は元アプリとの**互換を保つ** (`.atdoc` 読み込み、MetaMoJi API)。
- 元アプリのサーバーへは、許可された互換サインインを通じてのみ接続する。
  `docs/typespec` との整合は `metamoji-api` のカバレッジテストが担保する。