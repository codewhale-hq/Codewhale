CREATE TABLE IF NOT EXISTS daily_totals (
  day TEXT NOT NULL,
  metric TEXT NOT NULL,
  dim TEXT NOT NULL,
  key TEXT NOT NULL,
  value REAL NOT NULL,
  PRIMARY KEY (day, metric, dim, key)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS daily_totals_metric_dim ON daily_totals (metric, dim, day);
