export function isActiveAt(endsAt: string, now: number = Date.now()): boolean {
  const end = Date.parse(endsAt);
  return Number.isFinite(end) && end > now;
}

export function daysLeft(endsAt: string, now: number = Date.now()): number {
  return Math.max(1, Math.ceil((Date.parse(endsAt) - now) / 86_400_000));
}
