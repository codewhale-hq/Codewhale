export const MIN_SECRET_CHARS = 32;
export const MAX_SECRET_CHARS = 512;
export const MIN_SECRET_DISTINCT_CHARS = 16;

export const MAINTAINER_TOKEN_UNAVAILABLE =
  `Maintainer sign-in is unavailable: MAINTAINER_TOKEN must be set to a secret of at least ${MIN_SECRET_CHARS} characters ` +
  `(at most ${MAX_SECRET_CHARS}, with at least ${MIN_SECRET_DISTINCT_CHARS} distinct characters).`;

export const CRON_SECRET_UNAVAILABLE =
  `manual trigger disabled: CRON_SECRET must be a secret of at least ${MIN_SECRET_CHARS} characters ` +
  `(at most ${MAX_SECRET_CHARS}, with at least ${MIN_SECRET_DISTINCT_CHARS} distinct characters)`;

export function secretStrongEnough(value: string | undefined): value is string {
  if (typeof value !== "string" || value.length > MAX_SECRET_CHARS) return false;
  const chars = [...value];
  return chars.length >= MIN_SECRET_CHARS && new Set(chars).size >= MIN_SECRET_DISTINCT_CHARS;
}

export const maintainerTokenUsable = secretStrongEnough;
