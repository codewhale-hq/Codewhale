const ANSI16 = [
  "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
  "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];

export function hex(r, g, b) {
  return `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}

export function paletteColor(index) {
  if (index < 16) return ANSI16[index];
  if (index < 232) {
    const i = index - 16;
    const level = (v) => (v === 0 ? 0 : 55 + v * 40);
    return hex(level(Math.floor(i / 36)), level(Math.floor(i / 6) % 6), level(i % 6));
  }
  const v = 8 + (index - 232) * 10;
  return hex(v, v, v);
}
