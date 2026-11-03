// QR code encoder — byte mode, versions 1-10, error correction level L.
//
// Zero-dependency, so the Weixin bridge keeps its no-npm-deps property. The
// login URL is ASCII (~70 bytes), which fits version 4-5 at ECC L; versions up
// to 10 are supported for headroom.
//
// Rendered the same way the Rust side does it
// (`qrcode::render::unicode::Dense1x2` in crates/app-server/src/runtime_api.rs):
// two QR rows per text row using half-block glyphs.

// --- Galois field GF(256) tables for Reed-Solomon -------------------------

const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
(() => {
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
})();

function gfMul(a, b) {
  if (a === 0 || b === 0) return 0;
  return EXP[LOG[a] + LOG[b]];
}

/** Reed-Solomon generator polynomial of the given degree. */
function rsGenerator(degree) {
  let poly = [1];
  for (let i = 0; i < degree; i++) {
    const next = new Array(poly.length + 1).fill(0);
    for (let j = 0; j < poly.length; j++) {
      next[j] ^= gfMul(poly[j], 1);
      next[j + 1] ^= gfMul(poly[j], EXP[i]);
    }
    poly = next;
  }
  return poly;
}

/** Compute `ecLength` Reed-Solomon error correction bytes for `data`. */
function rsEncode(data, ecLength) {
  const gen = rsGenerator(ecLength);
  const result = new Array(ecLength).fill(0);
  for (const byte of data) {
    const factor = byte ^ result[0];
    result.shift();
    result.push(0);
    for (let i = 0; i < ecLength; i++) {
      result[i] ^= gfMul(gen[i + 1], factor);
    }
  }
  return result;
}

// --- Version tables --------------------------------------------------------

// [ecCodewordsPerBlock, [[blockCount, dataCodewordsPerBlock], ...]]
// Error correction level L only.
const VERSION_TABLE = {
  1: [7, [[1, 19]]],
  2: [10, [[1, 34]]],
  3: [15, [[1, 55]]],
  4: [20, [[1, 80]]],
  5: [26, [[1, 108]]],
  6: [18, [[2, 68]]],
  7: [20, [[2, 78]]],
  8: [24, [[2, 97]]],
  9: [30, [[2, 116]]],
  10: [18, [[2, 68], [2, 69]]],
};

const ALIGNMENT_POSITIONS = {
  1: [],
  2: [6, 18],
  3: [6, 22],
  4: [6, 26],
  5: [6, 30],
  6: [6, 34],
  7: [6, 22, 38],
  8: [6, 24, 42],
  9: [6, 26, 46],
  10: [6, 28, 50],
};

/** Total data codewords available at level L for a version. */
function dataCapacity(version) {
  const [, blocks] = VERSION_TABLE[version];
  return blocks.reduce((sum, [count, size]) => sum + count * size, 0);
}

// --- Bit buffer ------------------------------------------------------------

class BitBuffer {
  constructor() {
    this.bits = [];
  }
  put(value, length) {
    for (let i = length - 1; i >= 0; i--) {
      this.bits.push((value >>> i) & 1);
    }
  }
  get length() {
    return this.bits.length;
  }
}

// --- Encoding --------------------------------------------------------------

function chooseVersion(byteLength) {
  for (const version of Object.keys(VERSION_TABLE).map(Number).sort((a, b) => a - b)) {
    // 4 bits mode + 8 or 16 bits length + payload, then terminator headroom.
    const lengthBits = version < 10 ? 8 : 16;
    const needed = 4 + lengthBits + byteLength * 8;
    if (needed <= dataCapacity(version) * 8) return version;
  }
  throw new Error(`qr: payload too long for supported versions (${byteLength} bytes)`);
}

function buildCodewords(bytes, version) {
  const capacity = dataCapacity(version);
  const buf = new BitBuffer();
  buf.put(0b0100, 4); // byte mode
  buf.put(bytes.length, version < 10 ? 8 : 16);
  for (const byte of bytes) buf.put(byte, 8);

  // Terminator, then pad to a byte boundary.
  const maxBits = capacity * 8;
  buf.put(0, Math.min(4, maxBits - buf.length));
  while (buf.length % 8 !== 0) buf.bits.push(0);

  const data = [];
  for (let i = 0; i < buf.length; i += 8) {
    let byte = 0;
    for (let j = 0; j < 8; j++) byte = (byte << 1) | buf.bits[i + j];
    data.push(byte);
  }
  // Alternating pad bytes.
  const pads = [0xec, 0x11];
  for (let i = 0; data.length < capacity; i++) data.push(pads[i % 2]);

  // Split into blocks, add EC, then interleave.
  const [ecPerBlock, blockSpec] = VERSION_TABLE[version];
  const dataBlocks = [];
  const ecBlocks = [];
  let offset = 0;
  for (const [count, size] of blockSpec) {
    for (let b = 0; b < count; b++) {
      const block = data.slice(offset, offset + size);
      offset += size;
      dataBlocks.push(block);
      ecBlocks.push(rsEncode(block, ecPerBlock));
    }
  }

  const out = [];
  const maxData = Math.max(...dataBlocks.map((b) => b.length));
  for (let i = 0; i < maxData; i++) {
    for (const block of dataBlocks) if (i < block.length) out.push(block[i]);
  }
  for (let i = 0; i < ecPerBlock; i++) {
    for (const block of ecBlocks) out.push(block[i]);
  }
  return out;
}

// --- Matrix construction ---------------------------------------------------

function makeMatrix(version) {
  const size = version * 4 + 17;
  const modules = Array.from({ length: size }, () => new Array(size).fill(null));
  const reserved = Array.from({ length: size }, () => new Array(size).fill(false));

  const setFinder = (row, col) => {
    for (let r = -1; r <= 7; r++) {
      for (let c = -1; c <= 7; c++) {
        const rr = row + r;
        const cc = col + c;
        if (rr < 0 || rr >= size || cc < 0 || cc >= size) continue;
        const inRing = r >= 0 && r <= 6 && c >= 0 && c <= 6;
        const dark =
          inRing && (r === 0 || r === 6 || c === 0 || c === 6 || (r >= 2 && r <= 4 && c >= 2 && c <= 4));
        modules[rr][cc] = dark ? 1 : 0;
        reserved[rr][cc] = true;
      }
    }
  };

  setFinder(0, 0);
  setFinder(0, size - 7);
  setFinder(size - 7, 0);

  // Alignment patterns.
  const positions = ALIGNMENT_POSITIONS[version];
  for (const row of positions) {
    for (const col of positions) {
      // Skip the three finder corners.
      const nearFinder =
        (row <= 8 && col <= 8) ||
        (row <= 8 && col >= size - 9) ||
        (row >= size - 9 && col <= 8);
      if (nearFinder) continue;
      for (let r = -2; r <= 2; r++) {
        for (let c = -2; c <= 2; c++) {
          const dark = Math.max(Math.abs(r), Math.abs(c)) !== 1;
          modules[row + r][col + c] = dark ? 1 : 0;
          reserved[row + r][col + c] = true;
        }
      }
    }
  }

  // Timing patterns.
  for (let i = 8; i < size - 8; i++) {
    if (!reserved[6][i]) {
      modules[6][i] = i % 2 === 0 ? 1 : 0;
      reserved[6][i] = true;
    }
    if (!reserved[i][6]) {
      modules[i][6] = i % 2 === 0 ? 1 : 0;
      reserved[i][6] = true;
    }
  }

  // Dark module + reserve format areas.
  modules[size - 8][8] = 1;
  reserved[size - 8][8] = true;
  for (let i = 0; i < 9; i++) {
    if (!reserved[8][i]) reserved[8][i] = true;
    if (!reserved[i][8]) reserved[i][8] = true;
  }
  for (let i = 0; i < 8; i++) {
    reserved[8][size - 1 - i] = true;
    reserved[size - 1 - i][8] = true;
  }

  // Reserve version info for version >= 7.
  if (version >= 7) {
    for (let i = 0; i < 6; i++) {
      for (let j = 0; j < 3; j++) {
        reserved[size - 11 + j][i] = true;
        reserved[i][size - 11 + j] = true;
      }
    }
  }

  return { size, modules, reserved };
}

function placeData(matrix, codewords) {
  const { size, modules, reserved } = matrix;
  let bitIndex = 0;
  const totalBits = codewords.length * 8;
  const nextBit = () => {
    if (bitIndex >= totalBits) return 0;
    const byte = codewords[bitIndex >> 3];
    const bit = (byte >>> (7 - (bitIndex & 7))) & 1;
    bitIndex++;
    return bit;
  };

  // Two-module-wide columns, right to left, alternating up/down. The vertical
  // timing column (6) is skipped by shifting the whole pair left by one, which
  // is why the guard is `<= 6` rather than `=== 6`: once the pair straddles the
  // timing column every subsequent column is offset.
  let upward = true;
  for (let col = size - 1; col > 0; col -= 2) {
    if (col <= 6) col -= 1;
    for (let i = 0; i < size; i++) {
      const row = upward ? size - 1 - i : i;
      for (const c of [col, col - 1]) {
        if (reserved[row][c]) continue;
        modules[row][c] = nextBit();
      }
    }
    upward = !upward;
  }
}

function applyMask(modules, reserved, maskId) {
  const size = modules.length;
  return modules.map((row, r) =>
    row.map((value, c) => {
      if (reserved[r][c]) return value;
      let invert;
      switch (maskId) {
        case 0: invert = (r + c) % 2 === 0; break;
        case 1: invert = r % 2 === 0; break;
        case 2: invert = c % 3 === 0; break;
        case 3: invert = (r + c) % 3 === 0; break;
        case 4: invert = (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0; break;
        case 5: invert = ((r * c) % 2) + ((r * c) % 3) === 0; break;
        case 6: invert = (((r * c) % 2) + ((r * c) % 3)) % 2 === 0; break;
        default: invert = (((r + c) % 2) + ((r * c) % 3)) % 2 === 0; break;
      }
      return invert ? value ^ 1 : value;
    })
  );
}

function penalty(modules) {
  const size = modules.length;
  let score = 0;
  // Rule 1: runs of 5+ same-colour modules.
  for (let r = 0; r < size; r++) {
    for (const dir of ["row", "col"]) {
      let run = 1;
      for (let i = 1; i < size; i++) {
        const prev = dir === "row" ? modules[r][i - 1] : modules[i - 1][r];
        const cur = dir === "row" ? modules[r][i] : modules[i][r];
        if (cur === prev) {
          run++;
        } else {
          if (run >= 5) score += 3 + (run - 5);
          run = 1;
        }
      }
      if (run >= 5) score += 3 + (run - 5);
    }
  }
  // Rule 2: 2x2 blocks.
  for (let r = 0; r < size - 1; r++) {
    for (let c = 0; c < size - 1; c++) {
      const v = modules[r][c];
      if (v === modules[r][c + 1] && v === modules[r + 1][c] && v === modules[r + 1][c + 1]) {
        score += 3;
      }
    }
  }
  // Rule 3: the two ISO/IEC 18004 finder-like patterns, each 11 modules long:
  //   pattern1: 10111010000
  //   pattern2: 00001011101
  // Matching these exactly matters — a looser check selects a different mask
  // than reference encoders, producing a valid-looking but different matrix.
  const PATTERN1 = [1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0];
  const PATTERN2 = [0, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1];
  const scanForFinderLike = (get) => {
    let hits = 0;
    for (let start = 0; start + 11 <= size; start++) {
      let p1 = true;
      let p2 = true;
      for (let i = 0; i < 11; i++) {
        const v = get(start + i);
        if (v !== PATTERN1[i]) p1 = false;
        if (v !== PATTERN2[i]) p2 = false;
        if (!p1 && !p2) break;
      }
      if (p1 || p2) hits++;
    }
    return hits;
  };
  for (let r = 0; r < size; r++) {
    score += 40 * scanForFinderLike((i) => modules[r][i]);
  }
  for (let c = 0; c < size; c++) {
    score += 40 * scanForFinderLike((i) => modules[i][c]);
  }
  // Rule 4: dark-module balance.
  let dark = 0;
  for (const row of modules) for (const v of row) dark += v;
  const percent = (dark * 100) / (size * size);
  score += Math.floor(Math.abs(percent - 50) / 5) * 10;
  return score;
}

function formatBits(maskId) {
  // ECC level L (0b01) + mask, BCH(15,5) with the standard generator.
  let data = (0b01 << 3) | maskId;
  let rem = data << 10;
  for (let i = 14; i >= 10; i--) {
    if ((rem >>> i) & 1) rem ^= 0b10100110111 << (i - 10);
  }
  return ((data << 10) | rem) ^ 0b101010000010010;
}

function placeFormat(matrix, maskId) {
  const { size, modules } = matrix;
  const bits = formatBits(maskId);
  for (let i = 0; i < 15; i++) {
    const bit = (bits >>> i) & 1;
    // First copy: bits 0-5 down the left of the top-left finder (column 8),
    // bit 6 at (8,7), bits 7-8 at (8,5)-(8,6)... then along row 8 to the right.
    if (i < 6) {
      modules[i][8] = bit;
    } else if (i < 8) {
      modules[i + 1][8] = bit;
    } else if (i === 8) {
      modules[8][7] = bit;
    } else {
      modules[8][14 - i] = bit;
    }
    // Second copy: bits 0-7 along the bottom of the top-right finder,
    // bits 8-14 down the right of the bottom-left finder.
    if (i < 8) {
      modules[8][size - 1 - i] = bit;
    } else {
      modules[size - 15 + i][8] = bit;
    }
  }
  modules[size - 8][8] = 1;
}

function versionBits(version) {
  let rem = version << 12;
  for (let i = 17; i >= 12; i--) {
    if ((rem >>> i) & 1) rem ^= 0b1111100100101 << (i - 12);
  }
  return (version << 12) | rem;
}

function placeVersion(matrix, version) {
  if (version < 7) return;
  const { size, modules } = matrix;
  const bits = versionBits(version);
  for (let i = 0; i < 18; i++) {
    const bit = (bits >>> i) & 1;
    const row = Math.floor(i / 3);
    const col = i % 3;
    modules[size - 11 + col][row] = bit;
    modules[row][size - 11 + col] = bit;
  }
}

// --- Public API ------------------------------------------------------------

/**
 * Encode `text` as a QR module matrix (1 = dark).
 * @param {string} text
 * @returns {number[][]}
 */
export function encodeQr(text) {
  const bytes = [...Buffer.from(text, "utf8")];
  const version = chooseVersion(bytes.length);
  const codewords = buildCodewords(bytes, version);
  const matrix = makeMatrix(version);
  placeData(matrix, codewords);

  let best = null;
  for (let maskId = 0; maskId < 8; maskId++) {
    const masked = applyMask(matrix.modules, matrix.reserved, maskId);
    const score = penalty(masked);
    if (!best || score < best.score) best = { score, masked, maskId };
  }
  placeFormat({ size: matrix.size, modules: best.masked }, best.maskId);
  placeVersion({ size: matrix.size, modules: best.masked }, version);
  return best.masked;
}

/**
 * Render `text` as a terminal QR code using half-block glyphs, matching the
 * Rust side's `Dense1x2` renderer.
 *
 * @param {string} text
 * @param {{invert?: boolean, quietZone?: number}} [options]
 * @returns {string}
 */
export function renderQrToText(text, options = {}) {
  const { invert = false, quietZone = 2 } = options;
  const matrix = encodeQr(text);
  const size = matrix.length;
  const padded = size + quietZone * 2;

  // Pad so the matrix has an even number of rows for half-block pairing.
  const totalRows = padded % 2 === 0 ? padded : padded + 1;

  const dark = (r, c) => {
    const rr = r - quietZone;
    const cc = c - quietZone;
    if (rr < 0 || rr >= size || cc < 0 || cc >= size) return false;
    return matrix[rr][cc] === 1;
  };

  const lines = [];
  for (let r = 0; r < totalRows; r += 2) {
    let line = "";
    for (let c = 0; c < padded; c++) {
      const top = dark(r, c);
      const bottom = dark(r + 1, c);
      // Half-block glyphs: each text cell covers two module rows.
      if (top && bottom) line += "\u2588";      // █
      else if (top) line += "\u2580";           // ▀
      else if (bottom) line += "\u2584";        // ▄
      else line += " ";
    }
    // Dark modules must be dark ink; when the terminal draws light-on-dark this
    // is already correct, but allow the caller to flip for dark-on-light.
    lines.push(invert ? line.replace(/[\u2580\u2584\u2588 ]/g, (ch) =>
      ch === " " ? "\u2588" : " ") : line);
  }
  return lines.join("\n");
}
