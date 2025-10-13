# 変更履歴

## 2025-10-13
- `rss_links.yml`の追加パラメータは現行未対応である旨をドキュメントに明記。
- queue/Articleのテーブル仕様を`pub_date`表記へ統一し、`Article.data`の型誤記を修正。
- UUID生成をアプリケーションで行う方針へ切り替え、`rss.queue`のマイグレーションおよび挿入処理を更新。
- スクレイピングAPIがHTTPエラーを返した場合でも`status_code`を保存するよう`fetch-content`の処理を改修。
- HTTP 5xx応答をモック化し、`status_code`保持を検証するテストを追加。
- `fetch_rss`テストの参照方法を明示的な`use crate::...`に揃え、将来のモジュール変更でも追従しやすくした。

## 2025-10-12
- `fetch-content` フローを外部スクレイピングAPI連携仕様に合わせて再実装。
- API呼び出し結果の保存処理をトランザクション化し、Brotli圧縮データとstatus_codeの整合性を保証。
- `wiremock` を用いたテストを追加し、HTTP応答別の挙動を検証。
- `fetch-rss` のRSSリンク解析を刷新し、リンク抽出ロジックとYAML仕様の乖離を是正。
- RSSリンク設定を正規化し、YAML構造と実装の不整合を解消。
- テストモジュールをガイドライン準拠で再編し、DB連携テストは`TEST_DATABASE_URL`設定時のみ実行されるよう制御。
