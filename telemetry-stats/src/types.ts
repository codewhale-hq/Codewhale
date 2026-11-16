export interface AnalyticsSqlBinding {
  query(request: { query: string; params?: unknown }): Promise<{ data?: unknown }>;
}

export interface D1Statement {
  bind(...values: unknown[]): D1Statement;
  all<T = Record<string, unknown>>(): Promise<{ results?: T[] }>;
  first<T = Record<string, unknown>>(): Promise<T | null>;
}

export interface D1Like {
  prepare(sql: string): D1Statement;
  batch(statements: D1Statement[]): Promise<unknown[]>;
}

export interface Env {
  ANALYTICS_SQL?: AnalyticsSqlBinding;
  CF_ACCOUNT_ID?: string;
  CF_API_TOKEN?: string;
  DB?: D1Like;
  ASSETS?: { fetch(request: Request): Promise<Response> };
  ACCESS_TEAM_DOMAIN?: string;
  ACCESS_AUD?: string;
}

export interface StatRow {
  day: string;
  metric: string;
  dim: string;
  key: string;
  value: number;
}

export type SqlRunner = {
  source: "binding" | "rest";
  run(sql: string): Promise<unknown[]>;
};
