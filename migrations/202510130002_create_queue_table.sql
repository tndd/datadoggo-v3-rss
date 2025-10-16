-- queueテーブルとトリガーを完全に作り直し
DROP TRIGGER IF EXISTS update_queue_updated_at ON rss.queue;
DROP TABLE IF EXISTS rss.queue CASCADE;

CREATE TABLE rss.queue (
    id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    link TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    pub_date TIMESTAMPTZ,
    description TEXT NOT NULL,
    status_code INTEGER,
    "group" TEXT
);

-- updated_at自動更新トリガー
CREATE OR REPLACE FUNCTION rss.update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

CREATE TRIGGER update_queue_updated_at BEFORE UPDATE ON rss.queue
    FOR EACH ROW EXECUTE FUNCTION rss.update_updated_at_column();
