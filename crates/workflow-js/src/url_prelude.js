"use strict";
(() => {
  const SPECIAL_PORTS = Object.freeze({
    "http:": "80",
    "https:": "443",
    "ws:": "80",
    "wss:": "443",
    "ftp:": "21",
    "file:": "",
  });

  const isSpecialScheme = (protocol) =>
    Object.prototype.hasOwnProperty.call(SPECIAL_PORTS, protocol);

  const defaultPort = (protocol) =>
    isSpecialScheme(protocol) ? SPECIAL_PORTS[protocol] : "";

  const stripOuterC0AndSpace = (value) =>
    value.replace(/^[\u0000-\u0020]+|[\u0000-\u0020]+$/g, "");

  const stripTabAndNewline = (value) => value.replace(/[\t\r\n]/g, "");

  const toUtf8Bytes = (str) => {
    const bytes = [];
    for (let i = 0; i < str.length; i++) {
      let cp = str.charCodeAt(i);
      if (cp >= 0xd800 && cp <= 0xdbff) {
        if (i + 1 < str.length) {
          const next = str.charCodeAt(i + 1);
          if (next >= 0xdc00 && next <= 0xdfff) {
            cp = 0x10000 + ((cp - 0xd800) << 10) + (next - 0xdc00);
            i++;
          } else {
            cp = 0xfffd;
          }
        } else {
          cp = 0xfffd;
        }
      } else if (cp >= 0xdc00 && cp <= 0xdfff) {
        cp = 0xfffd;
      }
      if (cp < 0x80) {
        bytes.push(cp);
      } else if (cp < 0x800) {
        bytes.push(0xc0 | (cp >> 6), 0x80 | (cp & 0x3f));
      } else if (cp < 0x10000) {
        bytes.push(
          0xe0 | (cp >> 12),
          0x80 | ((cp >> 6) & 0x3f),
          0x80 | (cp & 0x3f)
        );
      } else {
        bytes.push(
          0xf0 | (cp >> 18),
          0x80 | ((cp >> 12) & 0x3f),
          0x80 | ((cp >> 6) & 0x3f),
          0x80 | (cp & 0x3f)
        );
      }
    }
    return bytes;
  };

  const utf8BytesToString = (bytes) => {
    let out = "";
    let i = 0;
    while (i < bytes.length) {
      const b0 = bytes[i];
      if (b0 < 0x80) {
        out += String.fromCharCode(b0);
        i++;
      } else if (b0 >= 0xc2 && b0 <= 0xdf && i + 1 < bytes.length) {
        const b1 = bytes[i + 1];
        if ((b1 & 0xc0) === 0x80) {
          out += String.fromCharCode(((b0 & 0x1f) << 6) | (b1 & 0x3f));
          i += 2;
        } else {
          out += "\ufffd";
          i++;
        }
      } else if (b0 >= 0xe0 && b0 <= 0xef && i + 2 < bytes.length) {
        const b1 = bytes[i + 1];
        const b2 = bytes[i + 2];
        const lower = b0 === 0xe0 ? 0xa0 : 0x80;
        const upper = b0 === 0xed ? 0x9f : 0xbf;
        if (b1 >= lower && b1 <= upper && (b2 & 0xc0) === 0x80) {
          out += String.fromCharCode(
            ((b0 & 0x0f) << 12) | ((b1 & 0x3f) << 6) | (b2 & 0x3f)
          );
          i += 3;
        } else {
          out += "\ufffd";
          i++;
        }
      } else if (b0 >= 0xf0 && b0 <= 0xf4 && i + 3 < bytes.length) {
        const b1 = bytes[i + 1];
        const b2 = bytes[i + 2];
        const b3 = bytes[i + 3];
        const lower = b0 === 0xf0 ? 0x90 : 0x80;
        const upper = b0 === 0xf4 ? 0x8f : 0xbf;
        if (
          b1 >= lower &&
          b1 <= upper &&
          (b2 & 0xc0) === 0x80 &&
          (b3 & 0xc0) === 0x80
        ) {
          const cp =
            ((b0 & 0x07) << 18) |
            ((b1 & 0x3f) << 12) |
            ((b2 & 0x3f) << 6) |
            (b3 & 0x3f);
          const offset = cp - 0x10000;
          out += String.fromCharCode(
            0xd800 + (offset >> 10),
            0xdc00 + (offset & 0x3ff)
          );
          i += 4;
        } else {
          out += "\ufffd";
          i++;
        }
      } else {
        out += "\ufffd";
        i++;
      }
    }
    return out;
  };

  const percentEncodeByte = (byte) =>
    "%" + byte.toString(16).toUpperCase().padStart(2, "0");

  const percentEncode = (str, shouldEncodeAscii) => {
    const bytes = toUtf8Bytes(str);
    let out = "";
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      if (b < 0x80 && !shouldEncodeAscii(b)) {
        out += String.fromCharCode(b);
      } else {
        out += percentEncodeByte(b);
      }
    }
    return out;
  };

  const isC0OrDel = (b) => b <= 0x1f || b > 0x7e;

  const isFragmentEncode = (b) =>
    isC0OrDel(b) ||
    b === 0x20 ||
    b === 0x22 ||
    b === 0x3c ||
    b === 0x3e ||
    b === 0x60;

  const isPathEncode = (b) =>
    isFragmentEncode(b) ||
    b === 0x23 ||
    b === 0x3f ||
    b === 0x7b ||
    b === 0x7d;

  const isUserinfoEncode = (b) =>
    isPathEncode(b) ||
    b === 0x2f ||
    b === 0x3a ||
    b === 0x3b ||
    b === 0x3d ||
    b === 0x40 ||
    (b >= 0x5b && b <= 0x5e) ||
    b === 0x7c;

  const encodeFragment = (str) => percentEncode(str, isFragmentEncode);

  const encodeQuery = (str, special) =>
    percentEncode(
      str,
      (b) =>
        isC0OrDel(b) ||
        b === 0x20 ||
        b === 0x22 ||
        b === 0x23 ||
        b === 0x3c ||
        b === 0x3e ||
        (special && b === 0x27)
    );

  const encodePath = (str) => percentEncode(str, isPathEncode);

  const encodeUserinfo = (str) => percentEncode(str, isUserinfoEncode);

  const formEncode = (str) => {
    const bytes = toUtf8Bytes(str);
    let out = "";
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      if (b === 0x20) {
        out += "+";
      } else if (
        b === 0x2a ||
        b === 0x2d ||
        b === 0x2e ||
        (b >= 0x30 && b <= 0x39) ||
        (b >= 0x41 && b <= 0x5a) ||
        b === 0x5f ||
        (b >= 0x61 && b <= 0x7a)
      ) {
        out += String.fromCharCode(b);
      } else {
        out += percentEncodeByte(b);
      }
    }
    return out;
  };

  const hexValue = (code) => {
    if (code >= 0x30 && code <= 0x39) return code - 0x30;
    if (code >= 0x41 && code <= 0x46) return code - 0x37;
    if (code >= 0x61 && code <= 0x66) return code - 0x57;
    return -1;
  };

  const formDecode = (str) => {
    const spaced = str.replace(/\+/g, " ");
    if (spaced.indexOf("%") === -1) return spaced;
    const rawBytes = toUtf8Bytes(spaced);
    const out = [];
    for (let i = 0; i < rawBytes.length; i++) {
      if (rawBytes[i] === 0x25 && i + 2 < rawBytes.length) {
        const hi = hexValue(rawBytes[i + 1]);
        const lo = hexValue(rawBytes[i + 2]);
        if (hi !== -1 && lo !== -1) {
          out.push((hi << 4) | lo);
          i += 2;
          continue;
        }
      }
      out.push(rawBytes[i]);
    }
    return utf8BytesToString(out);
  };

  const parseQueryStringInto = (entries, input) => {
    const cleaned = input.startsWith("?") ? input.slice(1) : input;
    if (cleaned === "") return;
    const parts = cleaned.split("&");
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      if (part === "") continue;
      const eq = part.indexOf("=");
      if (eq === -1) {
        entries.push([formDecode(part), ""]);
      } else {
        entries.push([formDecode(part.slice(0, eq)), formDecode(part.slice(eq + 1))]);
      }
    }
  };

  const serializeEntries = (entries) => {
    if (entries.length === 0) return "";
    const parts = new Array(entries.length);
    for (let i = 0; i < entries.length; i++) {
      parts[i] = formEncode(entries[i][0]) + "=" + formEncode(entries[i][1]);
    }
    return parts.join("&");
  };

  const normalizePathSegments = (path) => {
    const raw = path.startsWith("/") ? path.slice(1) : path;
    const segments = raw.split("/");
    const out = [];
    for (let i = 0; i < segments.length; i++) {
      const seg = segments[i];
      const lower = seg.toLowerCase();
      const isLast = i === segments.length - 1;
      if (
        lower === ".." ||
        lower === ".%2e" ||
        lower === "%2e." ||
        lower === "%2e%2e"
      ) {
        out.pop();
        if (isLast) out.push("");
      } else if (lower === "." || lower === "%2e") {
        if (isLast) out.push("");
      } else {
        out.push(seg);
      }
    }
    return "/" + out.join("/");
  };

  const parseIpv4Octets = (input) => {
    const parts = input.split(".");
    if (parts.length !== 4) return null;
    const octets = [];
    for (let i = 0; i < 4; i++) {
      if (!/^\d{1,3}$/.test(parts[i])) return null;
      if (parts[i].length > 1 && parts[i].startsWith("0")) return null;
      const num = Number(parts[i]);
      if (num > 255) return null;
      octets.push(num);
    }
    return octets;
  };

  const parseIpv6 = (inner) => {
    if (inner === "") {
      throw new TypeError("Invalid URL");
    }
    const halves = inner.split("::");
    if (halves.length > 2) {
      throw new TypeError("Invalid URL");
    }
    const parseHalf = (half, allowIpv4) => {
      if (half === "") return [];
      const tokens = half.split(":");
      const pieces = [];
      for (let i = 0; i < tokens.length; i++) {
        const tok = tokens[i];
        if (tok === "") {
          throw new TypeError("Invalid URL");
        }
        if (allowIpv4 && i === tokens.length - 1 && tok.indexOf(".") !== -1) {
          const octets = parseIpv4Octets(tok);
          if (!octets) {
            throw new TypeError("Invalid URL");
          }
          pieces.push((octets[0] << 8) | octets[1], (octets[2] << 8) | octets[3]);
        } else {
          if (!/^[0-9A-Fa-f]{1,4}$/.test(tok)) {
            throw new TypeError("Invalid URL");
          }
          pieces.push(parseInt(tok, 16));
        }
      }
      return pieces;
    };
    const left = parseHalf(halves[0], halves.length === 1);
    const right = halves.length === 2 ? parseHalf(halves[1], true) : [];
    const pieces = [];
    if (halves.length === 2) {
      const missing = 8 - (left.length + right.length);
      if (missing < 1) {
        throw new TypeError("Invalid URL");
      }
      for (let i = 0; i < left.length; i++) pieces.push(left[i]);
      for (let i = 0; i < missing; i++) pieces.push(0);
      for (let i = 0; i < right.length; i++) pieces.push(right[i]);
    } else {
      if (left.length !== 8) {
        throw new TypeError("Invalid URL");
      }
      for (let i = 0; i < 8; i++) pieces.push(left[i]);
    }

    let bestStart = -1;
    let bestLen = 1;
    let curStart = -1;
    let curLen = 0;
    for (let i = 0; i < 8; i++) {
      if (pieces[i] === 0) {
        if (curStart === -1) curStart = i;
        curLen++;
        if (curLen > bestLen) {
          bestStart = curStart;
          bestLen = curLen;
        }
      } else {
        curStart = -1;
        curLen = 0;
      }
    }

    let out = "[";
    let skip = false;
    for (let i = 0; i < 8; i++) {
      if (skip) {
        if (i < bestStart + bestLen) continue;
        skip = false;
      }
      if (bestStart === i) {
        out += i === 0 ? "::" : ":";
        skip = true;
        continue;
      }
      out += pieces[i].toString(16);
      if (i !== 7) out += ":";
    }
    return out + "]";
  };

  const parsePort = (rawPort, protocol) => {
    if (rawPort === "") return "";
    if (!/^\d+$/.test(rawPort)) {
      throw new TypeError("Invalid URL");
    }
    const num = Number(rawPort);
    if (!Number.isSafeInteger(num) || num < 0 || num > 65535) {
      throw new TypeError("Invalid URL");
    }
    const canonical = String(num);
    return canonical === defaultPort(protocol) ? "" : canonical;
  };

  const FORBIDDEN_HOST_RE = /[\u0000-\u0020#/:<>?@[\\\]^|\u007F]/;

  const parseHostAndPort = (hostPort, protocol, allowEmptyHost) => {
    if (hostPort === "") {
      if (allowEmptyHost) return { hostname: "", port: "" };
      throw new TypeError("Invalid URL");
    }
    if (hostPort.startsWith("[")) {
      const close = hostPort.indexOf("]");
      if (close === -1) {
        throw new TypeError("Invalid URL");
      }
      const hostname = parseIpv6(hostPort.slice(1, close));
      const rest = hostPort.slice(close + 1);
      if (rest === "") {
        return { hostname, port: "" };
      }
      if (!rest.startsWith(":")) {
        throw new TypeError("Invalid URL");
      }
      return { hostname, port: parsePort(rest.slice(1), protocol) };
    }
    const firstColon = hostPort.indexOf(":");
    let rawHost = hostPort;
    let rawPort;
    if (firstColon !== -1) {
      rawHost = hostPort.slice(0, firstColon);
      rawPort = hostPort.slice(firstColon + 1);
      if (rawPort.indexOf(":") !== -1) {
        throw new TypeError("Invalid URL");
      }
    }
    if (rawHost === "") {
      if (allowEmptyHost && rawPort === undefined) {
        return { hostname: "", port: "" };
      }
      throw new TypeError("Invalid URL");
    }
    if (FORBIDDEN_HOST_RE.test(rawHost)) {
      throw new TypeError("Invalid URL");
    }
    const hostname = rawHost.toLowerCase();
    const port = rawPort === undefined ? "" : parsePort(rawPort, protocol);
    return { hostname, port };
  };

  const parseAuthority = (rawAuthority, protocol) => {
    const at = rawAuthority.lastIndexOf("@");
    let username = "";
    let password = "";
    let hostPort = rawAuthority;
    if (at !== -1) {
      const userinfo = rawAuthority.slice(0, at);
      hostPort = rawAuthority.slice(at + 1);
      const colon = userinfo.indexOf(":");
      if (colon !== -1) {
        username = encodeUserinfo(userinfo.slice(0, colon));
        password = encodeUserinfo(userinfo.slice(colon + 1));
      } else {
        username = encodeUserinfo(userinfo);
      }
    }
    const allowEmptyHost = !isSpecialScheme(protocol) || protocol === "file:";
    const { hostname, port } = parseHostAndPort(hostPort, protocol, allowEmptyHost);
    return { username, password, hostname, port };
  };

  const splitAtFirst = (value, needle) => {
    const idx = value.indexOf(needle);
    if (idx === -1) return [value, null];
    return [value.slice(0, idx), value.slice(idx + 1)];
  };

  const SCHEME_RE = /^([A-Za-z][A-Za-z0-9+.-]*):/;

  const resolveRelativePath = (basePathname, relativePath) => {
    const lastSlash = basePathname.lastIndexOf("/");
    const dir = lastSlash === -1 ? "/" : basePathname.slice(0, lastSlash + 1);
    return normalizePathSegments(dir + relativePath);
  };

  const parseUrl = (inputValue, baseValue) => {
    const raw = stripTabAndNewline(stripOuterC0AndSpace(String(inputValue)));
    let base = null;
    if (baseValue !== undefined) {
      try {
        base = parseUrl(baseValue, undefined);
      } catch (_) {
        throw new TypeError("Invalid base URL");
      }
    }

    const schemeMatch = SCHEME_RE.exec(raw);
    if (schemeMatch) {
      const protocol = schemeMatch[1].toLowerCase() + ":";
      const afterScheme = raw.slice(schemeMatch[0].length);
      const [beforeHash, rawFrag] = splitAtFirst(afterScheme, "#");
      const [beforeQuery, rawQuery] = splitAtFirst(beforeHash, "?");
      const special = isSpecialScheme(protocol);
      const fragment = rawFrag === null ? null : encodeFragment(rawFrag);
      const query = rawQuery === null ? null : encodeQuery(rawQuery, special);

      if (special) {
        const slashed = beforeQuery.replace(/\\/g, "/");
        if (base && base.protocol === protocol && !slashed.startsWith("//")) {
          if (slashed === "") {
            return {
              protocol: base.protocol,
              username: base.username,
              password: base.password,
              hostname: base.hostname,
              port: base.port,
              pathname: base.pathname,
              query: rawQuery === null ? base.query : query,
              fragment,
              hasAuthority: base.hasAuthority,
            };
          }
          const pathname = slashed.startsWith("/")
            ? encodePath(normalizePathSegments(slashed))
            : encodePath(resolveRelativePath(base.pathname, slashed));
          return {
            protocol: base.protocol,
            username: base.username,
            password: base.password,
            hostname: base.hostname,
            port: base.port,
            pathname,
            query,
            fragment,
            hasAuthority: base.hasAuthority,
          };
        }

        if (protocol === "file:") {
          let rest = slashed;
          let hostname = "";
          if (rest.startsWith("//")) {
            rest = rest.slice(2);
            const slash = rest.indexOf("/");
            const rawHost = slash === -1 ? rest : rest.slice(0, slash);
            const rawPath = slash === -1 ? "" : rest.slice(slash);
            if (rawHost !== "" && rawHost.toLowerCase() !== "localhost") {
              const parsed = parseHostAndPort(rawHost, protocol, true);
              if (parsed.port !== "") {
                throw new TypeError("Invalid URL");
              }
              hostname = parsed.hostname;
            }
            const pathname = encodePath(
              normalizePathSegments(rawPath === "" ? "/" : rawPath)
            );
            return {
              protocol,
              username: "",
              password: "",
              hostname,
              port: "",
              pathname,
              query,
              fragment,
              hasAuthority: true,
            };
          }
          const pathname = encodePath(
            normalizePathSegments(rest.startsWith("/") ? rest : "/" + rest)
          );
          return {
            protocol,
            username: "",
            password: "",
            hostname: "",
            port: "",
            pathname,
            query,
            fragment,
            hasAuthority: true,
          };
        }

        const stripped = slashed.replace(/^\/+/, "");
        const slash = stripped.indexOf("/");
        const rawAuthority = slash === -1 ? stripped : stripped.slice(0, slash);
        const rawPath = slash === -1 ? "/" : stripped.slice(slash);
        const auth = parseAuthority(rawAuthority, protocol);
        const pathname = encodePath(normalizePathSegments(rawPath));
        return {
          protocol,
          username: auth.username,
          password: auth.password,
          hostname: auth.hostname,
          port: auth.port,
          pathname,
          query,
          fragment,
          hasAuthority: true,
        };
      }

      if (beforeQuery.startsWith("//")) {
        const afterSlashes = beforeQuery.slice(2);
        const slash = afterSlashes.indexOf("/");
        const rawAuthority = slash === -1 ? afterSlashes : afterSlashes.slice(0, slash);
        const auth = parseAuthority(rawAuthority, protocol);
        const pathname =
          slash === -1
            ? ""
            : encodePath(normalizePathSegments(afterSlashes.slice(slash)));
        return {
          protocol,
          username: auth.username,
          password: auth.password,
          hostname: auth.hostname,
          port: auth.port,
          pathname,
          query,
          fragment,
          hasAuthority: true,
        };
      }

      return {
        protocol,
        username: "",
        password: "",
        hostname: "",
        port: "",
        pathname: encodePath(beforeQuery),
        query,
        fragment,
        hasAuthority: false,
      };
    }

    if (!base) {
      throw new TypeError("Invalid URL");
    }

    const [beforeHash, rawFrag] = splitAtFirst(raw, "#");
    const fragment = rawFrag === null ? null : encodeFragment(rawFrag);
    if (beforeHash === "") {
      return {
        protocol: base.protocol,
        username: base.username,
        password: base.password,
        hostname: base.hostname,
        port: base.port,
        pathname: base.pathname,
        query: base.query,
        fragment,
        hasAuthority: base.hasAuthority,
      };
    }

    if (!base.hasAuthority) {
      throw new TypeError("Invalid URL");
    }

    const special = isSpecialScheme(base.protocol);
    const [rawBeforeQuery, rawQuery] = splitAtFirst(beforeHash, "?");
    const beforeQuery = special ? rawBeforeQuery.replace(/\\/g, "/") : rawBeforeQuery;
    const query = rawQuery === null ? null : encodeQuery(rawQuery, special);

    if (beforeQuery.startsWith("//")) {
      const rebuilt =
        base.protocol +
        beforeQuery +
        (rawQuery === null ? "" : "?" + rawQuery) +
        (rawFrag === null ? "" : "#" + rawFrag);
      return parseUrl(rebuilt, undefined);
    }

    if (beforeQuery === "") {
      return {
        protocol: base.protocol,
        username: base.username,
        password: base.password,
        hostname: base.hostname,
        port: base.port,
        pathname: base.pathname,
        query,
        fragment,
        hasAuthority: true,
      };
    }

    const pathname = beforeQuery.startsWith("/")
      ? encodePath(normalizePathSegments(beforeQuery))
      : encodePath(resolveRelativePath(base.pathname, beforeQuery));

    return {
      protocol: base.protocol,
      username: base.username,
      password: base.password,
      hostname: base.hostname,
      port: base.port,
      pathname,
      query,
      fragment,
      hasAuthority: true,
    };
  };

  const formatHost = (state) =>
    state.port === "" ? state.hostname : state.hostname + ":" + state.port;

  const formatOrigin = (state) => {
    if (
      state.protocol === "http:" ||
      state.protocol === "https:" ||
      state.protocol === "ws:" ||
      state.protocol === "wss:" ||
      state.protocol === "ftp:"
    ) {
      return state.protocol + "//" + formatHost(state);
    }
    return "null";
  };

  const formatHref = (state) => {
    let out = state.protocol;
    if (state.hasAuthority) {
      out += "//";
      if (state.username !== "" || state.password !== "") {
        out += state.username;
        if (state.password !== "") {
          out += ":" + state.password;
        }
        out += "@";
      }
      out += formatHost(state);
    }
    out += state.pathname;
    if (state.query !== null) {
      out += "?" + state.query;
    }
    if (state.fragment !== null) {
      out += "#" + state.fragment;
    }
    return out;
  };

  const paramsState = new WeakMap();
  const urlState = new WeakMap();

  const requireParamsState = (self) => {
    const state = paramsState.get(self);
    if (!state) {
      throw new TypeError("Illegal invocation");
    }
    return state;
  };

  const requireUrlState = (self) => {
    const state = urlState.get(self);
    if (!state) {
      throw new TypeError("Illegal invocation");
    }
    return state;
  };

  const initSearchParams = (entries, init) => {
    if (init === undefined || init === null) return;
    if (typeof init === "string") {
      parseQueryStringInto(entries, init);
      return;
    }
    const existing = paramsState.get(init);
    if (existing) {
      for (let i = 0; i < existing.entries.length; i++) {
        entries.push([existing.entries[i][0], existing.entries[i][1]]);
      }
      return;
    }
    if (typeof init === "object" && typeof init[Symbol.iterator] === "function") {
      for (const pair of init) {
        if (
          pair === null ||
          typeof pair !== "object" ||
          typeof pair[Symbol.iterator] !== "function"
        ) {
          throw new TypeError(
            "URLSearchParams sequence element must be an iterable pair"
          );
        }
        const items = Array.from(pair);
        if (items.length !== 2) {
          throw new TypeError(
            "URLSearchParams sequence element must have exactly 2 items"
          );
        }
        entries.push([String(items[0]), String(items[1])]);
      }
      return;
    }
    if (typeof init === "object") {
      const keys = Object.keys(init);
      for (let i = 0; i < keys.length; i++) {
        entries.push([String(keys[i]), String(init[keys[i]])]);
      }
      return;
    }
    parseQueryStringInto(entries, String(init));
  };

  class URLSearchParams {
    constructor(init) {
      const entries = [];
      initSearchParams(entries, init);
      paramsState.set(this, { entries, onUpdate: null });
    }

    get size() {
      return requireParamsState(this).entries.length;
    }

    append(name, value) {
      if (arguments.length < 2) {
        throw new TypeError("URLSearchParams.append requires 2 arguments");
      }
      const state = requireParamsState(this);
      state.entries.push([String(name), String(value)]);
      if (state.onUpdate) state.onUpdate();
    }

    delete(name, value) {
      if (arguments.length < 1) {
        throw new TypeError("URLSearchParams.delete requires at least 1 argument");
      }
      const state = requireParamsState(this);
      const key = String(name);
      const matchValue = arguments.length >= 2 && value !== undefined;
      const target = matchValue ? String(value) : "";
      const next = [];
      for (let i = 0; i < state.entries.length; i++) {
        const pair = state.entries[i];
        if (pair[0] === key && (!matchValue || pair[1] === target)) {
          continue;
        }
        next.push(pair);
      }
      state.entries = next;
      if (state.onUpdate) state.onUpdate();
    }

    get(name) {
      if (arguments.length < 1) {
        throw new TypeError("URLSearchParams.get requires 1 argument");
      }
      const state = requireParamsState(this);
      const key = String(name);
      for (let i = 0; i < state.entries.length; i++) {
        if (state.entries[i][0] === key) {
          return state.entries[i][1];
        }
      }
      return null;
    }

    getAll(name) {
      if (arguments.length < 1) {
        throw new TypeError("URLSearchParams.getAll requires 1 argument");
      }
      const state = requireParamsState(this);
      const key = String(name);
      const values = [];
      for (let i = 0; i < state.entries.length; i++) {
        if (state.entries[i][0] === key) {
          values.push(state.entries[i][1]);
        }
      }
      return values;
    }

    has(name, value) {
      if (arguments.length < 1) {
        throw new TypeError("URLSearchParams.has requires at least 1 argument");
      }
      const state = requireParamsState(this);
      const key = String(name);
      const matchValue = arguments.length >= 2 && value !== undefined;
      const target = matchValue ? String(value) : "";
      for (let i = 0; i < state.entries.length; i++) {
        const pair = state.entries[i];
        if (pair[0] === key && (!matchValue || pair[1] === target)) {
          return true;
        }
      }
      return false;
    }

    set(name, value) {
      if (arguments.length < 2) {
        throw new TypeError("URLSearchParams.set requires 2 arguments");
      }
      const state = requireParamsState(this);
      const key = String(name);
      const val = String(value);
      let replaced = false;
      const next = [];
      for (let i = 0; i < state.entries.length; i++) {
        const pair = state.entries[i];
        if (pair[0] === key) {
          if (!replaced) {
            next.push([key, val]);
            replaced = true;
          }
        } else {
          next.push(pair);
        }
      }
      if (!replaced) {
        next.push([key, val]);
      }
      state.entries = next;
      if (state.onUpdate) state.onUpdate();
    }

    sort() {
      const state = requireParamsState(this);
      state.entries.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
      if (state.onUpdate) state.onUpdate();
    }

    entries() {
      const state = requireParamsState(this);
      return (function* () {
        for (let i = 0; i < state.entries.length; i++) {
          yield [state.entries[i][0], state.entries[i][1]];
        }
      })();
    }

    keys() {
      const state = requireParamsState(this);
      return (function* () {
        for (let i = 0; i < state.entries.length; i++) {
          yield state.entries[i][0];
        }
      })();
    }

    values() {
      const state = requireParamsState(this);
      return (function* () {
        for (let i = 0; i < state.entries.length; i++) {
          yield state.entries[i][1];
        }
      })();
    }

    forEach(callback, thisArg) {
      const state = requireParamsState(this);
      if (typeof callback !== "function") {
        throw new TypeError("URLSearchParams.forEach callback must be a function");
      }
      for (let i = 0; i < state.entries.length; i++) {
        callback.call(thisArg, state.entries[i][1], state.entries[i][0], this);
      }
    }

    toString() {
      return serializeEntries(requireParamsState(this).entries);
    }

    [Symbol.iterator]() {
      return this.entries();
    }
  }

  Object.defineProperty(URLSearchParams.prototype, Symbol.toStringTag, {
    value: "URLSearchParams",
    configurable: true,
  });

  const syncParamsFromQuery = (state) => {
    const pState = paramsState.get(state.searchParams);
    const next = [];
    if (state.query !== null && state.query !== "") {
      parseQueryStringInto(next, state.query);
    }
    pState.entries = next;
  };

  class URL {
    constructor(input, base) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'URL': 1 argument required");
      }
      const state = parseUrl(input, base);
      const searchParams = new URLSearchParams(state.query || "");
      const pState = paramsState.get(searchParams);
      pState.onUpdate = () => {
        const serialized = serializeEntries(pState.entries);
        state.query = serialized === "" ? null : serialized;
      };
      state.searchParams = searchParams;
      urlState.set(this, state);
    }

    static canParse(input, base) {
      if (arguments.length < 1) return false;
      try {
        parseUrl(input, base);
        return true;
      } catch (_) {
        return false;
      }
    }

    static parse(input, base) {
      if (arguments.length < 1) return null;
      try {
        return new URL(input, base);
      } catch (_) {
        return null;
      }
    }

    get href() {
      return formatHref(requireUrlState(this));
    }

    set href(value) {
      const state = requireUrlState(this);
      const parsed = parseUrl(value, undefined);
      state.protocol = parsed.protocol;
      state.username = parsed.username;
      state.password = parsed.password;
      state.hostname = parsed.hostname;
      state.port = parsed.port;
      state.pathname = parsed.pathname;
      state.query = parsed.query;
      state.fragment = parsed.fragment;
      state.hasAuthority = parsed.hasAuthority;
      syncParamsFromQuery(state);
    }

    get origin() {
      return formatOrigin(requireUrlState(this));
    }

    get protocol() {
      return requireUrlState(this).protocol;
    }

    set protocol(value) {
      const state = requireUrlState(this);
      const raw = stripTabAndNewline(String(value));
      const match = /^([A-Za-z][A-Za-z0-9+.-]*)(?::|$)/.exec(raw);
      if (!match) return;
      const nextProtocol = match[1].toLowerCase() + ":";
      if (isSpecialScheme(state.protocol) !== isSpecialScheme(nextProtocol)) {
        return;
      }
      if (
        (state.protocol === "file:" || nextProtocol === "file:") &&
        state.protocol !== nextProtocol
      ) {
        return;
      }
      state.protocol = nextProtocol;
      if (state.port !== "" && state.port === defaultPort(nextProtocol)) {
        state.port = "";
      }
    }

    get username() {
      return requireUrlState(this).username;
    }

    set username(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority || state.hostname === "" || state.protocol === "file:") {
        return;
      }
      state.username = encodeUserinfo(String(value));
    }

    get password() {
      return requireUrlState(this).password;
    }

    set password(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority || state.hostname === "" || state.protocol === "file:") {
        return;
      }
      state.password = encodeUserinfo(String(value));
    }

    get host() {
      return formatHost(requireUrlState(this));
    }

    set host(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority) return;
      const raw = stripTabAndNewline(String(value));
      try {
        const allowEmpty = !isSpecialScheme(state.protocol) || state.protocol === "file:";
        const parsed = parseHostAndPort(raw, state.protocol, allowEmpty);
        if (state.protocol === "file:" && parsed.port !== "") return;
        state.hostname = parsed.hostname;
        const portGiven = raw.startsWith("[")
          ? raw.slice(raw.indexOf("]") + 1).startsWith(":")
          : raw.indexOf(":") !== -1;
        if (portGiven) state.port = parsed.port;
      } catch (_) {}
    }

    get hostname() {
      return requireUrlState(this).hostname;
    }

    set hostname(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority) return;
      const raw = stripTabAndNewline(String(value));
      if (!raw.startsWith("[") && raw.indexOf(":") !== -1) return;
      try {
        const allowEmpty = !isSpecialScheme(state.protocol) || state.protocol === "file:";
        const parsed = parseHostAndPort(raw, state.protocol, allowEmpty);
        if (parsed.port !== "") return;
        state.hostname = parsed.hostname;
      } catch (_) {}
    }

    get port() {
      return requireUrlState(this).port;
    }

    set port(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority || state.hostname === "" || state.protocol === "file:") {
        return;
      }
      const raw = stripTabAndNewline(String(value));
      if (raw === "") {
        state.port = "";
        return;
      }
      try {
        state.port = parsePort(raw, state.protocol);
      } catch (_) {}
    }

    get pathname() {
      return requireUrlState(this).pathname;
    }

    set pathname(value) {
      const state = requireUrlState(this);
      if (!state.hasAuthority) return;
      let raw = stripTabAndNewline(String(value));
      if (isSpecialScheme(state.protocol)) {
        raw = raw.replace(/\\/g, "/");
        if (!raw.startsWith("/")) raw = "/" + raw;
        state.pathname = encodePath(normalizePathSegments(raw));
      } else {
        if (raw === "") {
          state.pathname = "";
        } else {
          if (!raw.startsWith("/")) raw = "/" + raw;
          state.pathname = encodePath(normalizePathSegments(raw));
        }
      }
    }

    get search() {
      const query = requireUrlState(this).query;
      return query ? "?" + query : "";
    }

    set search(value) {
      const state = requireUrlState(this);
      const raw = stripTabAndNewline(String(value));
      if (raw === "") {
        state.query = null;
      } else {
        const stripped = raw.startsWith("?") ? raw.slice(1) : raw;
        state.query = encodeQuery(stripped, isSpecialScheme(state.protocol));
      }
      syncParamsFromQuery(state);
    }

    get searchParams() {
      return requireUrlState(this).searchParams;
    }

    get hash() {
      const fragment = requireUrlState(this).fragment;
      return fragment ? "#" + fragment : "";
    }

    set hash(value) {
      const state = requireUrlState(this);
      const raw = stripTabAndNewline(String(value));
      if (raw === "") {
        state.fragment = null;
      } else {
        const stripped = raw.startsWith("#") ? raw.slice(1) : raw;
        state.fragment = encodeFragment(stripped);
      }
    }

    toString() {
      return this.href;
    }

    toJSON() {
      return this.href;
    }
  }

  Object.defineProperty(URL.prototype, Symbol.toStringTag, {
    value: "URL",
    configurable: true,
  });

  globalThis.URLSearchParams = URLSearchParams;
  globalThis.URL = URL;
})();
