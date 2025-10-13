# datadoggo-v3-rss

RSSフィードから記事を収集してPostgreSQLに保存するCLIツール。

## 機能

- RSSフィードからメタデータ（タイトル、リンク、説明など）を取得
- スクレイピングAPIを使用して記事本文を取得（Bot対策をすり抜ける）
- 取得した記事をBrotli圧縮してPostgreSQLに保存
- 重複チェック機能（linkベースで自動的にUPSERT）

## セットアップ

### 1. 依存関係のインストール

```bash
cargo build
```

### 2. データベース設定

PostgreSQLで以下を実行：

```sql
DROP DATABASE IF EXISTS "datadoggo-v3";
DROP DATABASE IF EXISTS datadoggo_v3;
CREATE DATABASE datadoggo_v3;
```

`.env`ファイルを編集してDB接続情報を設定：

```env
DATABASE_URL="postgresql://user:password@localhost:5432/datadoggo_v3"
SCRAPING_API_URL="http://localhost:8000"
```

### 3. マイグレーション実行

```bash
# schema作成
psql -d datadoggo_v3 -f migrations/202510130001_create_schema.sql

# queueテーブル作成
psql -d datadoggo_v3 -f migrations/202510130002_create_queue_table.sql

# article_contentテーブル作成
psql -d datadoggo_v3 -f migrations/202510130003_create_article_content_table.sql
```

### 4. RSSフィード設定

`rss_links.yml`を編集して監視したいRSSフィードを追加。

## 使い方

### RSSフィードから記事を取得してqueueに登録

```bash
cargo run -- fetch-rss
```

- `rss_links.yml`から対象フィードを読み込み
- 各RSSフィードを取得してパース
- queueテーブルに保存（既存レコードは更新）

### queue内の記事本文を取得

```bash
cargo run -- fetch-content

# 処理件数を指定
cargo run -- fetch-content --limit 50
```

- `status_code=NULL`または`status_code<>200`のエントリを取得
- スクレイピングAPI（現在はモック）を呼び出し
- `status_code=200`の場合のみ記事本文をBrotli圧縮してarticle_contentに保存
- 上記以外のステータスはqueueに記録し直す（再試行可）

### APIサーバを起動

```bash
cargo run -- serve --host 127.0.0.1 --port 8080
```

- `GET /health` : ヘルスチェック（`{"status":"ok"}`を返す）
- `POST /api/fetch-rss` : RSS巡回を実行し、処理結果をJSONで返す
- `POST /api/fetch-content` : queue内の未取得/失敗レコードを再試行する
  - リクエストボディ例: `{"limit": 100}`（省略時は100件）

## テーブル構成

### rss.queue

| カラム       | 型          | 説明                   |
| ------------ | ----------- | ---------------------- |
| id           | UUID        | 主キー（アプリケーション側で生成） |
| created_at   | TIMESTAMPTZ | 作成日時               |
| updated_at   | TIMESTAMPTZ | 更新日時（自動更新）   |
| link         | TEXT        | 記事URL（UNIQUE制約）  |
| title        | TEXT        | 記事タイトル           |
| pub_date     | TIMESTAMPTZ | 公開日時（NULL許容）   |
| description  | TEXT        | 記事説明               |
| status_code  | INTEGER     | HTTPステータスコード（NULL許容、未取得時はNULL） |
| group        | TEXT        | グループ名（NULL許容） |

### rss.article_content

| カラム     | 型          | 説明                          |
| ---------- | ----------- | ----------------------------- |
| queue_id   | UUID        | 主キー（queue.idへの外部キー）|
| created_at | TIMESTAMPTZ | 作成日時                      |
| updated_at | TIMESTAMPTZ | 更新日時（自動更新）          |
| data       | BYTEA       | Brotli圧縮された記事本文      |

## 開発

### テスト実行

```bash
cargo test
```

データベースへ接続する統合テストでは `TEST_DATABASE_URL` を使用します。PostgreSQLのテスト用データベースを作成し、以下のように環境変数を設定してください。

```bash
export TEST_DATABASE_URL="postgresql://user:password@localhost:5432/test_datadoggo_v3"

# テスト用データベースの初期化例
psql -d postgres -c "DROP DATABASE IF EXISTS test_datadoggo_v3"
psql -d postgres -c "CREATE DATABASE test_datadoggo_v3"
```

テストデータベースは毎回初期化される想定のため、本番データベースと共有しないよう注意してください。環境変数が未設定の場合、DB接続を必要とするテストは自動的にスキップされます。

### コンパイルチェック

```bash
cargo check
```

## 今後の拡張予定

- バッチ処理化（cron or systemd timer）
- 失敗タスクの再実行機能
- レート制限実装
- 並行処理の最適化
- 記事取得API（読み出し用エンドポイント）
