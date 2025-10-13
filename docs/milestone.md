# マイルストーン

## 決定した仕様

### 基本方針
- 単発実行のCLIツール（将来的にバッチ処理化）
- PostgreSQLを使用（production database: `datadoggo_v3`, test database: `test_datadoggo_v3`, schema: `rss`）
- スクレイピングAPIはモックで実装（本番APIは既存）

### 実行モデル
```bash
# 1. RSSフィードから新規記事をqueueに登録
cargo run -- fetch-rss

# 2. queue内のstatus_code=NULLまたは200以外な記事に対してAPI実行
cargo run -- fetch-content

# 3. HTTP経由で処理を呼び出す場合（任意）
cargo run -- serve --host 127.0.0.1 --port 8080
```

### データフロー
1. **RSS取得フェーズ（fetch-rss）**
   - rss_links.ymlから対象フィードを読み込み
   - 各RSSフィードをHTTP GETで取得（通常のrequest）
   - パース結果をqueueテーブルに保存
   - 既存レコード（linkが重複）は内容を更新（UPDATE）

2. **コンテンツ取得フェーズ（fetch-content）**
   - queueからstatus_codeがNULLまたは200以外のレコードを取得
   - スクレイピングAPIを呼び出し（Bot対策をすり抜けるため）
   - status_code=200の場合のみarticle_contentに保存（Brotli圧縮）
   - それ以外のstatus_codeはqueueに記録し直す（再試行対象）

### エラーハンドリング
- リトライは実装しない
- 失敗した記事はstatus_codeをそのまま記録
- 再実行は別ドメインとして扱う（今回は実装範囲外）

### 設定ファイル
- **rss_links.yml**: プロジェクトルートに配置
- **.env**: DB接続情報などを記述

### テーブル仕様
- **queue.id**: UUID（アプリケーションで生成）
- **queue.link**: UNIQUE制約（重複チェック用）
- **article_content.data**: Brotli圧縮されたバイナリ（PostgreSQLではBYTEA）

---

## 実装TODO

### Phase 1: プロジェクト基盤
- [x] Cargo.tomlの依存関係追加
  - sqlx（PostgreSQL、UUID、chrono機能）
  - tokio（非同期ランタイム）
  - reqwest（HTTP client）
  - feed-rs（RSSパーサー）
  - brotli（圧縮）
  - serde, serde_yaml（YAML読み込み）
  - dotenv（環境変数）
  - clap（CLI引数パース）
- [x] .envファイルのテンプレート作成
- [x] sqlx-cliのインストール確認

### Phase 2: データベースセットアップ
- [x] マイグレーションファイル作成
  - queueテーブル（linkにUNIQUE制約）
  - article_contentテーブル（queue_idに外部キー）
- [x] マイグレーション実行確認

### Phase 3: 共通モジュール実装
- [x] config.rs: 環境変数読み込み
- [x] models.rs: Queue, ArticleContent構造体定義
- [x] db.rs: データベース接続プール

### Phase 4: fetch-rssコマンド実装
- [x] rss_links.yml読み込み処理
- [x] RSSフィード取得（reqwest）
- [x] RSSパース（feed-rs）
- [x] queueへのINSERT/UPDATE処理
  - ON CONFLICT (link) DO UPDATE実装
- [x] テスト実装

### Phase 5: fetch-contentコマンド実装
- [x] スクレイピングAPIモック実装
- [x] queueからstatus_code=NULLのレコード取得
- [x] API呼び出し処理
- [x] Brotli圧縮処理
- [x] article_contentへの保存
- [x] queueのstatus_code更新
- [x] テスト実装

### Phase 6: CLIエントリーポイント
- [x] main.rs: clap設定
- [x] サブコマンド分岐処理
- [x] エラーハンドリング

### Phase 7: 動作確認
- [ ] fetch-rssの実動作確認
- [ ] fetch-contentの実動作確認
- [ ] エラーケースの確認

---

## 技術スタック

### 依存クレート（予定）
```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono"] }
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
feed-rs = "2.0"
brotli = "7.0"
serde = { version = "1.0", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1.0"
dotenv = "0.15"
clap = { version = "4", features = ["derive"] }
anyhow = "1.0"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1.0", features = ["v4", "serde"] }
```

### 開発ツール
- sqlx-cli: マイグレーション管理
- cargo: ビルド・テスト

---

## 将来の拡張予定
- バッチ処理化（cron or systemd timer）
- 失敗タスクの再実行機能
- レート制限実装
- 並行処理の最適化
- 記事取得API（読み出し用エンドポイント）
