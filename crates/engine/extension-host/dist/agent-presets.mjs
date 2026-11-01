var __defProp = Object.defineProperty;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __esm = (fn, res, err) => function __init() {
  if (err) throw err[0];
  try {
    return fn && (res = (0, fn[__getOwnPropNames(fn)[0]])(fn = 0)), res;
  } catch (e) {
    throw err = [e], e;
  }
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};

// node_modules/@deepseek-ai/cosmokit/lib/index.js
function isNullable(value) {
  return value === null || value === void 0;
}
function isNonNullable(value) {
  return !isNullable(value);
}
function mapValues(object, transform) {
  return Object.fromEntries(Object.entries(object).map(([key, value]) => [key, transform(value, key)]));
}
function defineProperty(object, key, value) {
  return Object.defineProperty(object, key, {
    writable: true,
    value,
    enumerable: false
  });
}
function isVolatile(value) {
  return typeof value === "object" && value !== null && write in value;
}
function is(type2, value) {
  if (arguments.length === 1) return (value2) => is(type2, value2);
  return type2 in globalThis && value instanceof globalThis[type2] || Object.prototype.toString.call(value).slice(8, -1) === type2;
}
function isArrayBufferLike(value) {
  return is("ArrayBuffer", value) || is("SharedArrayBuffer", value);
}
function isArrayBufferSource(value) {
  return isArrayBufferLike(value) || ArrayBuffer.isView(value);
}
function deepEqual(a, b, strict) {
  const ancestors = /* @__PURE__ */ new Set();
  function compare(a2, b2) {
    if (a2 === b2) return true;
    if (isVolatile(a2) || isVolatile(b2)) return isVolatile(a2) && isVolatile(b2);
    if (!strict && isNullable(a2) && isNullable(b2)) return true;
    if (typeof a2 !== typeof b2 || typeof a2 !== "object" || !a2 || !b2) return false;
    if (ancestors.has(a2)) return false;
    function check(test, then) {
      return test(a2) ? test(b2) ? then(a2, b2) : false : test(b2) ? false : void 0;
    }
    ancestors.add(a2);
    try {
      return check(Array.isArray, (a3, b3) => {
        if (a3.length !== b3.length) return false;
        for (let index = 0; index < a3.length; index++) if (!compare(a3[index], b3[index])) return false;
        return true;
      }) ?? check(is("Date"), (a3, b3) => a3.valueOf() === b3.valueOf()) ?? check(is("URL"), (a3, b3) => a3.href === b3.href) ?? check(is("RegExp"), (a3, b3) => a3.source === b3.source && a3.flags === b3.flags) ?? check(isArrayBufferLike, (a3, b3) => {
        if (a3.byteLength !== b3.byteLength) return false;
        const viewA = new Uint8Array(a3);
        const viewB = new Uint8Array(b3);
        for (let i2 = 0; i2 < viewA.length; i2++) if (viewA[i2] !== viewB[i2]) return false;
        return true;
      }) ?? ((!strict || [a2, b2].every((value) => Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null)) && Object.keys({
        ...a2,
        ...b2
      }).every((key) => compare(a2[key], b2[key])));
    } finally {
      ancestors.delete(a2);
    }
  }
  return compare(a, b);
}
function tokenize(source, delimiters, delimiter) {
  const output = [];
  let state = 0;
  for (let i2 = 0; i2 < source.length; i2++) {
    const code = source.charCodeAt(i2);
    if (code >= 65 && code <= 90) {
      if (state === 1) {
        const next = source.charCodeAt(i2 + 1);
        if (next >= 97 && next <= 122) output.push(delimiter);
        output.push(code + 32);
      } else {
        if (state !== 0) output.push(delimiter);
        output.push(code + 32);
      }
      state = 1;
    } else if (code >= 97 && code <= 122) {
      output.push(code);
      state = 2;
    } else if (delimiters.includes(code)) {
      if (state !== 0) output.push(delimiter);
      state = 0;
    } else output.push(code);
  }
  return String.fromCharCode(...output);
}
function paramCase(source) {
  return tokenize(source, [45, 95], 45);
}
var write, Binary, base64ToArrayBuffer, arrayBufferToBase64, hexToArrayBuffer, arrayBufferToHex, hyphenate, Time;
var init_lib = __esm({
  "node_modules/@deepseek-ai/cosmokit/lib/index.js"() {
    write = /* @__PURE__ */ Symbol.for("cosmokit.volatile.write");
    (function(Binary2) {
      Binary2.is = isArrayBufferLike;
      Binary2.isSource = isArrayBufferSource;
      function fromSource(source) {
        if (ArrayBuffer.isView(source)) return source.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength);
        else return source;
      }
      Binary2.fromSource = fromSource;
      function toBase64(source) {
        source = fromSource(source);
        if (typeof Buffer !== "undefined") return Buffer.from(source).toString("base64");
        let binary2 = "";
        const bytes = new Uint8Array(source);
        for (let i2 = 0; i2 < bytes.byteLength; i2++) binary2 += String.fromCharCode(bytes[i2]);
        return btoa(binary2);
      }
      Binary2.toBase64 = toBase64;
      function fromBase64(source) {
        if (typeof Buffer !== "undefined") return fromSource(Buffer.from(source, "base64"));
        return Uint8Array.from(atob(source), (c) => c.charCodeAt(0));
      }
      Binary2.fromBase64 = fromBase64;
      function toHex(source) {
        source = fromSource(source);
        if (typeof Buffer !== "undefined") return Buffer.from(source).toString("hex");
        return Array.from(new Uint8Array(source), (byte) => byte.toString(16).padStart(2, "0")).join("");
      }
      Binary2.toHex = toHex;
      function fromHex(source) {
        if (typeof Buffer !== "undefined") return fromSource(Buffer.from(source, "hex"));
        const hex = source.length % 2 === 0 ? source : source.slice(0, source.length - 1);
        const buffer = [];
        for (let i2 = 0; i2 < hex.length; i2 += 2) buffer.push(parseInt(`${hex[i2]}${hex[i2 + 1]}`, 16));
        return Uint8Array.from(buffer).buffer;
      }
      Binary2.fromHex = fromHex;
    })(Binary || (Binary = {}));
    base64ToArrayBuffer = Binary.fromBase64;
    arrayBufferToBase64 = Binary.toBase64;
    hexToArrayBuffer = Binary.fromHex;
    arrayBufferToHex = Binary.toHex;
    hyphenate = paramCase;
    (function(Time2) {
      Time2.millisecond = 1;
      Time2.second = 1e3;
      Time2.minute = Time2.second * 60;
      Time2.hour = Time2.minute * 60;
      Time2.day = Time2.hour * 24;
      Time2.week = Time2.day * 7;
      let timezoneOffset = (/* @__PURE__ */ new Date()).getTimezoneOffset();
      function setTimezoneOffset(offset2) {
        timezoneOffset = offset2;
      }
      Time2.setTimezoneOffset = setTimezoneOffset;
      function getTimezoneOffset() {
        return timezoneOffset;
      }
      Time2.getTimezoneOffset = getTimezoneOffset;
      function getDateNumber(date = /* @__PURE__ */ new Date(), offset2) {
        if (typeof date === "number") date = new Date(date);
        if (offset2 === void 0) offset2 = timezoneOffset;
        return Math.floor((date.valueOf() / Time2.minute - offset2) / 1440);
      }
      Time2.getDateNumber = getDateNumber;
      function fromDateNumber(value, offset2) {
        const date = new Date(value * Time2.day);
        if (offset2 === void 0) offset2 = timezoneOffset;
        return new Date(+date + offset2 * Time2.minute);
      }
      Time2.fromDateNumber = fromDateNumber;
      const numeric = /\d+(?:\.\d+)?/.source;
      const timeRegExp = new RegExp(`^${[
        "w(?:eek(?:s)?)?",
        "d(?:ay(?:s)?)?",
        "h(?:our(?:s)?)?",
        "m(?:in(?:ute)?(?:s)?)?",
        "s(?:ec(?:ond)?(?:s)?)?"
      ].map((unit) => `(${numeric}${unit})?`).join("")}$`);
      function parseTime(source) {
        const capture = timeRegExp.exec(source);
        if (!capture) return 0;
        return (parseFloat(capture[1]) * Time2.week || 0) + (parseFloat(capture[2]) * Time2.day || 0) + (parseFloat(capture[3]) * Time2.hour || 0) + (parseFloat(capture[4]) * Time2.minute || 0) + (parseFloat(capture[5]) * Time2.second || 0);
      }
      Time2.parseTime = parseTime;
      function parseDate(date) {
        const parsed = parseTime(date);
        if (parsed) date = Date.now() + parsed;
        else if (/^\d{1,2}(:\d{1,2}){1,2}$/.test(date)) date = `${(/* @__PURE__ */ new Date()).toLocaleDateString()}-${date}`;
        else if (/^\d{1,2}-\d{1,2}-\d{1,2}(:\d{1,2}){1,2}$/.test(date)) date = `${(/* @__PURE__ */ new Date()).getFullYear()}-${date}`;
        return date ? new Date(date) : /* @__PURE__ */ new Date();
      }
      Time2.parseDate = parseDate;
      function format(ms) {
        const abs = Math.abs(ms);
        if (abs >= Time2.day - Time2.hour / 2) return Math.round(ms / Time2.day) + "d";
        else if (abs >= Time2.hour - Time2.minute / 2) return Math.round(ms / Time2.hour) + "h";
        else if (abs >= Time2.minute - Time2.second / 2) return Math.round(ms / Time2.minute) + "m";
        else if (abs >= Time2.second) return Math.round(ms / Time2.second) + "s";
        return ms + "ms";
      }
      Time2.format = format;
      function toDigits(source, length = 2) {
        return source.toString().padStart(length, "0");
      }
      Time2.toDigits = toDigits;
      function template(template2, time = /* @__PURE__ */ new Date()) {
        return template2.replace("yyyy", time.getFullYear().toString()).replace("yy", time.getFullYear().toString().slice(2)).replace("MM", toDigits(time.getMonth() + 1)).replace("dd", toDigits(time.getDate())).replace("hh", toDigits(time.getHours())).replace("mm", toDigits(time.getMinutes())).replace("ss", toDigits(time.getSeconds())).replace("SSS", toDigits(time.getMilliseconds(), 3));
      }
      Time2.template = template;
    })(Time || (Time = {}));
  }
});

// node_modules/@deepseek-ai/cordis/lib/index.js
function isConstructor(func) {
  if (!func.prototype) return false;
  if (func instanceof GeneratorFunction) return false;
  if (AsyncGeneratorFunction !== Function && func instanceof AsyncGeneratorFunction) return false;
  return true;
}
function joinPrototype(proto1, proto2) {
  if (proto1 === Object.prototype) return proto2;
  const result = Object.create(joinPrototype(Object.getPrototypeOf(proto1), proto2));
  for (const key of Reflect.ownKeys(proto1)) Object.defineProperty(result, key, Object.getOwnPropertyDescriptor(proto1, key));
  return result;
}
function isObject(value) {
  return value && (typeof value === "object" || typeof value === "function");
}
function getPropertyDescriptor(target, prop) {
  let proto = target;
  while (proto) {
    const desc = Reflect.getOwnPropertyDescriptor(proto, prop);
    if (desc) return desc;
    proto = Object.getPrototypeOf(proto);
  }
}
function getTraceable(ctx, value) {
  if (!isObject(value)) return value;
  if (Object.hasOwn(value, symbols.shadow)) return Object.getPrototypeOf(value);
  const tracker = value[symbols.tracker];
  if (!tracker) return value;
  return createTraceable(ctx, value, tracker);
}
function withProps(target, props) {
  if (!props) return target;
  return new Proxy(target, {
    get: (target2, prop, receiver) => {
      if (prop in props && prop !== "constructor") return Reflect.get(props, prop, receiver);
      return Reflect.get(target2, prop, receiver);
    },
    set: (target2, prop, value, receiver) => {
      if (prop in props && prop !== "constructor") return Reflect.set(props, prop, value, receiver);
      return Reflect.set(target2, prop, value, receiver);
    }
  });
}
function withProp(target, prop, value) {
  return withProps(target, Object.defineProperty(/* @__PURE__ */ Object.create(null), prop, {
    value,
    writable: false
  }));
}
function createShadow(ctx, target, property, receiver) {
  if (!property) return receiver;
  const origin = Reflect.getOwnPropertyDescriptor(target, property)?.value;
  if (!origin) return receiver;
  return withProp(receiver, property, ctx.extend({ [symbols.shadow]: origin }));
}
function createShadowMethod(ctx, value, outer, shadow) {
  return new Proxy(value, { apply: (target, thisArg, args) => {
    if (thisArg === outer) thisArg = shadow;
    return getTraceable(ctx, Reflect.apply(target, thisArg, args));
  } });
}
function createTraceable(ctx, value, tracker) {
  if (ctx[symbols.shadow] && !tracker.noShadow) ctx = Object.getPrototypeOf(ctx);
  const proxy = new Proxy(value, {
    get: (target, prop, receiver) => {
      if (prop === symbols.original) return target;
      if (prop === tracker.property) return ctx;
      if (typeof prop === "symbol") return Reflect.get(target, prop, receiver);
      if (tracker.associate && ctx.reflect.props[`${tracker.associate}.${prop}`]) return Reflect.get(ctx, `${tracker.associate}.${prop}`, withProp(ctx, symbols.receiver, receiver));
      let shadow, innerValue;
      const desc = getPropertyDescriptor(target, prop);
      if (desc && "value" in desc) innerValue = desc.value;
      else {
        shadow = createShadow(ctx, target, tracker.property, receiver);
        innerValue = Reflect.get(target, prop, shadow);
      }
      const innerTracker = innerValue?.[symbols.tracker];
      if (innerTracker) return createTraceable(ctx, innerValue, innerTracker);
      else if (!tracker.noShadow && typeof innerValue === "function") {
        shadow ??= createShadow(ctx, target, tracker.property, receiver);
        return createShadowMethod(ctx, innerValue, receiver, shadow);
      } else return innerValue;
    },
    set: (target, prop, value2, receiver) => {
      if (prop === symbols.original) return false;
      if (prop === tracker.property) return false;
      if (typeof prop === "symbol") return Reflect.set(target, prop, value2, receiver);
      if (tracker.associate && ctx.reflect.props[`${tracker.associate}.${prop}`]) return Reflect.set(ctx, `${tracker.associate}.${prop}`, value2, withProp(ctx, symbols.receiver, receiver));
      const shadow = createShadow(ctx, target, tracker.property, receiver);
      return Reflect.set(target, prop, value2, shadow);
    },
    apply: (target, thisArg, args) => {
      return applyTraceable(proxy, target, thisArg, args);
    }
  });
  return proxy;
}
function applyTraceable(proxy, value, thisArg, args) {
  if (!value[symbols.invoke]) return Reflect.apply(value, thisArg, args);
  return value[symbols.invoke].apply(proxy, args);
}
function createCallable(name, proto, tracker) {
  const self = function(...args) {
    return applyTraceable(createTraceable(self["ctx"], self, tracker), self, this, args);
  };
  defineProperty(self, "name", name);
  return Object.setPrototypeOf(self, proto);
}
function handleError(info, reason, getOuterStack) {
  const innerLines = info.error.stack.split("\n");
  if (typeof reason?.stack !== "string") {
    const outerError = new Error(reason);
    const lines2 = outerError.stack.split("\n");
    lines2.splice(1, Infinity, ...getOuterStack());
    outerError.stack = lines2.join("\n");
    throw outerError;
  }
  const lines = reason.stack.split("\n");
  let index = lines.indexOf(innerLines[2]);
  if (index === -1) throw reason;
  index -= info.offset;
  while (index > 0) {
    if (!lines[index - 1].endsWith(" (<anonymous>)")) break;
    index -= 1;
  }
  lines.splice(index, Infinity, ...getOuterStack());
  reason.stack = lines.join("\n");
  throw reason;
}
function composeError(callback, getOuterStack = buildOuterStack()) {
  const info = {
    offset: 1,
    error: /* @__PURE__ */ new Error()
  };
  try {
    const result = callback(info);
    if (isObject(result) && "then" in result) return result.then(void 0, (reason) => handleError(info, reason, getOuterStack));
    else return result;
  } catch (reason) {
    handleError(info, reason, getOuterStack);
  }
}
function buildOuterStack(offset2 = 0) {
  const outerError = /* @__PURE__ */ new Error();
  return () => outerError.stack.split("\n").slice(3 + offset2);
}
function isBailed(value) {
  return value !== null && value !== false && value !== void 0;
}
function isAggregateError(error) {
  return error instanceof Error && Array.isArray(error["errors"]);
}
function enhanceError(error) {
  const lines = error.stack.split("\n");
  lines.splice(0, 2, `Error: ${error.message}`);
  error.stack = lines.join("\n");
  return error;
}
function isSpecialProperty(prop) {
  return typeof prop === "symbol" || RESERVED_WORDS.includes(prop) || parseInt(prop).toString() === prop || prop.startsWith("_");
}
function resolveConfig(runtime, config) {
  if (!runtime.Config) return config;
  const result = runtime.Config["~standard"].validate(config);
  if ("then" in result) throw new TypeError("Async config validation is not supported");
  if (result.issues) throw new ValidationError(result.issues);
  else return result.value;
}
function runDisposable(dispose) {
  const result = dispose();
  return effectInertia.get(dispose)?.() ?? result;
}
function emitPluginDisposed(context, fiber) {
  const args = ["internal/plugin", fiber];
  let callbacks;
  try {
    callbacks = context.events.dispatch("emit", args);
  } catch (error) {
    context.logger.error(error);
    return;
  }
  for (const callback of callbacks) try {
    const returned = callback(...args);
    Promise.resolve(returned).catch((error) => context.logger.error(error));
  } catch (error) {
    context.logger.error(error);
  }
}
function isApplicable(object) {
  return object && typeof object === "object" && typeof object.apply === "function";
}
function Inject(name, config) {
  return function(value, decorator) {
    if (decorator.kind === "class") {
      if (!Object.hasOwn(value, "inject")) {
        defineProperty(value, "inject", Object.create(Object.getPrototypeOf(value).inject ?? null));
        defineProperty(value.inject, symbols.checkProto, true);
      }
      value.inject[name] = config;
    } else if (decorator.kind === "method") {
      const inject = (value[symbols.metadata] ??= {}).inject ??= /* @__PURE__ */ Object.create(null);
      inject[name] = config;
      decorator.addInitializer(function() {
        const property = this[symbols.tracker]?.property;
        (this[symbols.initHooks] ??= []).push(() => {
          this.ctx.inject(inject, (ctx) => {
            return value.call(property ? withProps(this, { [property]: ctx }) : this);
          });
        });
      });
    } else throw new Error("@Inject() can only be used on class or class methods");
  };
}
var DisposableList, symbols, GeneratorFunction, AsyncGeneratorFunction, EventsService, defaultFormatters, Logger, c16, c256, LoggerService, RESERVED_WORDS, ReflectService, kValidationError, ValidationError, effectInertia, CordisError, INACTIVE, Fiber, RegistryService, Context, Service;
var init_lib2 = __esm({
  "node_modules/@deepseek-ai/cordis/lib/index.js"() {
    init_lib();
    DisposableList = class {
      sn = 0;
      map = /* @__PURE__ */ new Map();
      weak = /* @__PURE__ */ new WeakMap();
      get length() {
        return this.map.size;
      }
      push(value) {
        const sn = ++this.sn;
        this.map.set(sn, value);
        this.weak.set(value, sn);
        return () => this.map.delete(sn);
      }
      delete(value) {
        const sn = this.weak.get(value);
        if (!sn) return false;
        return this.map.delete(sn);
      }
      clear() {
        const values = [...this.map.values()];
        this.map.clear();
        return values.reverse();
      }
      [Symbol.iterator]() {
        return this.map.values();
      }
      [/* @__PURE__ */ Symbol.for("nodejs.util.inspect.custom")]() {
        return [...this];
      }
    };
    symbols = {
      shadow: /* @__PURE__ */ Symbol.for("cordis.shadow"),
      receiver: /* @__PURE__ */ Symbol.for("cordis.receiver"),
      original: /* @__PURE__ */ Symbol.for("cordis.original"),
      metadata: /* @__PURE__ */ Symbol.for("cordis.metadata"),
      initHooks: /* @__PURE__ */ Symbol.for("cordis.initHooks"),
      checkProto: /* @__PURE__ */ Symbol.for("cordis.checkProto"),
      effect: /* @__PURE__ */ Symbol.for("cordis.effect"),
      filter: /* @__PURE__ */ Symbol.for("cordis.filter"),
      isolate: /* @__PURE__ */ Symbol.for("cordis.isolate"),
      intercept: /* @__PURE__ */ Symbol.for("cordis.intercept"),
      init: /* @__PURE__ */ Symbol.for("cordis.init"),
      check: /* @__PURE__ */ Symbol.for("cordis.check"),
      config: /* @__PURE__ */ Symbol.for("cordis.config"),
      invoke: /* @__PURE__ */ Symbol.for("cordis.invoke"),
      extend: /* @__PURE__ */ Symbol.for("cordis.extend"),
      tracker: /* @__PURE__ */ Symbol.for("cordis.tracker"),
      resolveConfig: /* @__PURE__ */ Symbol.for("cordis.resolveConfig")
    };
    GeneratorFunction = function* () {
    }.constructor;
    AsyncGeneratorFunction = async function* () {
    }.constructor;
    EventsService = class {
      ctx;
      _hooks = {};
      constructor(ctx) {
        this.ctx = ctx;
        defineProperty(this, symbols.tracker, {
          property: "ctx",
          noShadow: true
        });
        this.on("internal/listener", function(name, listener, options) {
          if (name === "internal/update" && !options.global) return (this.fiber._hooks["internal/update"] ??= new DisposableList())[options.prepend ? "unshift" : "push"](listener);
        });
        this.on("internal/update", function(config, noSave, next) {
          const cbs = [...this._hooks["internal/update"] || []];
          const _next = () => {
            return (cbs.shift() ?? next).call(this, config, noSave, _next);
          };
          return _next();
        }, {
          global: true,
          prepend: true
        });
      }
      /**
      * Resolve listeners for one dispatch and apply context filtering.
      *
      * @param type — the dispatch mode, reported on `internal/dispatch`.
      * @param args — the raw dispatch arguments; consumed up to the event name.
      * @returns the matching listener callbacks, bound to the dispatch `this`.
      */
      dispatch(type2, args) {
        const thisArg = typeof args[0] === "object" || typeof args[0] === "function" ? args.shift() : null;
        const name = args.shift();
        if (!name.startsWith("internal/")) this.emit("internal/dispatch", type2, name, args, thisArg);
        const filter = thisArg?.[Context.filter];
        return (this._hooks[name] || []).filter((hook) => hook.global || !filter || filter.call(thisArg, hook.ctx)).map((hook) => hook.callback.bind(thisArg));
      }
      /**
      * Run listeners concurrently and wait for all of them.
      *
      * @param args — optional `this`, the event name, then listener arguments.
      * @returns a promise resolving once every listener has settled.
      */
      async parallel(...args) {
        const errors = (await Promise.allSettled(this.dispatch("emit", args).map(async (cb) => cb(...args)))).filter((result) => result.status === "rejected");
        if (errors.length) throw new AggregateError(errors.map((error) => error.reason));
      }
      /**
      * Run listeners synchronously without waiting for returned promises.
      *
      * @param args — optional `this`, the event name, then listener arguments.
      */
      emit(...args) {
        this.dispatch("emit", args).map((cb) => cb(...args));
      }
      /**
      * Run listeners in order, awaiting each, until one returns a bail value.
      *
      * @param args — optional `this`, the event name, then listener arguments.
      * @returns the first bail value (see {@link isBailed}), if any.
      */
      async serial(...args) {
        for (const cb of this.dispatch("serial", args)) {
          const result = await cb(...args);
          if (isBailed(result)) return result;
        }
      }
      /**
      * Run listeners synchronously until one returns a bail value.
      *
      * @param args — optional `this`, the event name, then listener arguments.
      * @returns the first bail value (see {@link isBailed}), if any.
      */
      bail(...args) {
        for (const cb of this.dispatch("bail", args)) {
          const result = cb(...args);
          if (isBailed(result)) return result;
        }
      }
      /**
      * Compose listeners around the final `next` callback.
      *
      * The last dispatch argument is treated as the innermost `next`. Listeners
      * run outermost-first; a listener that does not call `next()` vetoes the
      * rest of the chain, including the built-in behavior.
      *
      * @param args — optional `this`, the event name, listener arguments, then `next`.
      * @returns the outermost listener's return value.
      */
      waterfall(...args) {
        const cbs = this.dispatch("waterfall", args);
        const inner = args.pop();
        const next = () => {
          return (cbs.shift() ?? inner)(...args);
        };
        args.push(next);
        return next();
      }
      /**
      * Store a listener record as an effect on the current fiber.
      *
      * @param label — effect label shown in fiber diagnostics.
      * @param hooks — the listener list for one event.
      * @param callback — the listener to store.
      * @param options — placement and filtering options.
      * @returns a disposer that unregisters the listener.
      */
      register(label, hooks, callback, options) {
        const method = options.prepend ? "unshift" : "push";
        return this.ctx.fiber.effect(() => {
          hooks[method]({
            ctx: this.ctx,
            callback,
            ...options
          });
          return () => this.unregister(hooks, callback);
        }, label);
      }
      /**
      * Remove a stored listener record.
      *
      * @param hooks — the listener list for one event.
      * @param callback — the listener to remove.
      * @returns `true` if the listener was found and removed.
      */
      unregister(hooks, callback) {
        const index = hooks.findIndex((hook) => hook.callback === callback);
        if (index >= 0) {
          hooks.splice(index, 1);
          return true;
        }
      }
      /**
      * Register an event listener owned by the current fiber.
      *
      * The listener is removed automatically when the fiber unloads. Throws
      * `CordisError('INACTIVE_EFFECT')` if the fiber is already disposed.
      *
      * @param name — the event name to listen for.
      * @param listener — called with the dispatch arguments.
      * @param options — listener options; a boolean is shorthand for `prepend`.
      * @returns a disposer removing the listener; `true` if it was still registered.
      */
      on(name, listener, options) {
        if (typeof options !== "object") options = { prepend: options };
        this.ctx.fiber.assertActive();
        listener = this.ctx.reflect.bind(listener);
        const result = this.bail(this.ctx, "internal/listener", name, listener, options);
        if (result) return result;
        const hooks = this._hooks[name] ||= [];
        const label = `ctx.on(${typeof name === "string" ? JSON.stringify(name) : name.toString()})`;
        return this.register(label, hooks, listener, options);
      }
      /**
      * Register an event listener that disposes itself after the first call.
      *
      * @param name — the event name to listen for.
      * @param listener — called at most once with the dispatch arguments.
      * @param options — listener options; a boolean is shorthand for `prepend`.
      * @returns a disposer removing the listener; `true` if it was still registered.
      */
      once(name, listener, options) {
        const dispose = this.on(name, function(...args) {
          dispose();
          return listener.apply(this, args);
        }, options);
        return dispose;
      }
    };
    defaultFormatters = {
      s: (value) => String(value),
      d: (value) => Math.trunc(Number(value)),
      i: (value) => Math.trunc(Number(value)),
      f: (value) => Number(value),
      o: (value) => JSON.stringify(value),
      O: (value) => JSON.stringify(value),
      c: () => "",
      C: (value, exporter, message) => {
        return Logger.color(exporter, Logger.code(message.name, exporter.colors), value);
      }
    };
    Logger = class {
      service;
      static color(exporter, code, value, decoration = "") {
        if (!exporter.colors) return "" + value;
        return `\x1B[3${code < 8 ? code : "8;5;" + code}${exporter.colors >= 2 ? decoration : ""}m${value}\x1B[0m`;
      }
      static code(name, level) {
        let hash2 = 0;
        for (let i2 = 0; i2 < name.length; i2++) {
          hash2 = (hash2 << 3) - hash2 + name.charCodeAt(i2) + 13;
          hash2 |= 0;
        }
        const colors = !level ? [] : level >= 2 ? c256 : c16;
        return colors[Math.abs(hash2) % colors.length];
      }
      static format(exporter, message) {
        const args = message.args.slice();
        if (args[0] instanceof Error) {
          args[0] = args[0].stack || args[0].message;
          args.unshift("%s");
        } else if (typeof args[0] !== "string") args.unshift("%o");
        let format = args.shift();
        format = format.replace(/%([a-zA-Z%])/g, (match, char) => {
          if (match === "%%") return "%";
          const formatter = exporter.formatters?.[char] ?? defaultFormatters[char];
          if (typeof formatter === "function") return formatter(args.shift(), exporter, message);
          return match;
        });
        const oFormatter = exporter.formatters?.o ?? defaultFormatters.o;
        for (let arg of args) {
          if (typeof arg === "object" && arg) arg = oFormatter(arg, exporter, message);
          format += " " + arg;
        }
        const { maxLength = 10240 } = exporter;
        return format.split(/\r?\n/g).map((line) => {
          return line.slice(0, maxLength) + (line.length > maxLength ? "..." : "");
        }).join("\n");
      }
      constructor(options, service) {
        this.service = service;
        Object.assign(this, options);
        this.error = this._method("error", 0);
        this.info = this._method("info", 1);
        this.warn = this._method("warn", 2);
        this.debug = this._method("debug", 3);
      }
      _method(type2, level) {
        return (...args) => {
          if (args.length === 1 && args[0] instanceof Error) {
            if (args[0].cause) this[type2](args[0].cause);
            else if (isAggregateError(args[0])) {
              args[0].errors.forEach((error) => this[type2](error));
              return;
            }
          }
          const sn = ++this.service._snMessage;
          const ts = Date.now();
          for (const exporter of this.service.exporters.values()) {
            if ((exporter.levels?.[this.name] ?? exporter.levels?.default ?? this.level ?? 1) < level) continue;
            const message = {
              sn,
              ts,
              type: type2,
              level,
              name: this.name,
              ...this.meta,
              args
            };
            exporter.export(message);
          }
        };
      }
    };
    c16 = [
      6,
      2,
      3,
      4,
      5,
      1
    ];
    c256 = [
      20,
      21,
      26,
      27,
      32,
      33,
      38,
      39,
      40,
      41,
      42,
      43,
      44,
      45,
      56,
      57,
      62,
      63,
      68,
      69,
      74,
      75,
      76,
      77,
      78,
      79,
      80,
      81,
      92,
      93,
      98,
      99,
      112,
      113,
      129,
      134,
      135,
      148,
      149,
      160,
      161,
      162,
      163,
      164,
      165,
      166,
      167,
      168,
      169,
      170,
      171,
      172,
      173,
      178,
      179,
      184,
      185,
      196,
      197,
      198,
      199,
      200,
      201,
      202,
      203,
      204,
      205,
      206,
      207,
      208,
      209,
      214,
      215,
      220,
      221
    ];
    LoggerService = class LoggerService2 {
      bufferSize = 1e3;
      buffer = [];
      ctx;
      _snMessage = 0;
      _snExporter = 0;
      exporters = /* @__PURE__ */ new Map();
      constructor(ctx) {
        const tracker = {
          property: "ctx",
          noShadow: true
        };
        const self = createCallable("logger", joinPrototype(Object.getPrototypeOf(this), Function.prototype), tracker);
        Object.assign(self, this);
        self.ctx = ctx;
        defineProperty(self, symbols.tracker, tracker);
        self.exporter({
          colors: 3,
          export: (message) => {
            self.buffer.push(message);
            if (self.buffer.length > self.bufferSize) self.buffer = self.buffer.slice(-self.bufferSize);
          }
        });
        return self;
      }
      /**
      * Register an exporter and dispose it with the current fiber.
      *
      * @param exporter — the sink that receives structured log messages.
      * @returns a disposer that removes the exporter.
      */
      exporter(exporter) {
        return this.ctx.effect(() => {
          const id = ++this._snExporter;
          this.exporters.set(id, exporter);
          return () => this.exporters.delete(id);
        }, "ctx.logger.exporter()");
      }
      _resolveConfig() {
        let intercept = this.ctx[symbols.intercept];
        const configs = [];
        while ("logger" in intercept) {
          if (Object.hasOwn(intercept, "logger")) configs.unshift(intercept["logger"]);
          intercept = Object.getPrototypeOf(intercept);
        }
        return Object.assign({}, ...configs);
      }
      [symbols.invoke](name) {
        const config = this._resolveConfig();
        const fiber = (this.ctx[symbols.shadow] ?? this.ctx).fiber;
        name ??= config.name;
        name ??= hyphenate(fiber.name);
        return new Logger({
          name,
          level: config.level,
          meta: { fiber: new WeakRef(fiber) }
        }, this);
      }
      static {
        for (const type2 of [
          "error",
          "info",
          "warn",
          "debug"
        ]) LoggerService2.prototype[type2] = function(...args) {
          return this()[type2](...args);
        };
      }
    };
    RESERVED_WORDS = ["prototype", "then"];
    ReflectService = class {
      ctx;
      /** Proxy traps implementing service resolution for every context object. */
      static handler = {
        get: (target, prop, ctx) => {
          if (isSpecialProperty(prop)) return Reflect.get(target, prop, ctx);
          if (Reflect.has(target, prop)) return getTraceable(ctx, Reflect.get(target, prop, ctx));
          const error = /* @__PURE__ */ new Error(`cannot get property "${prop}" without inject`);
          try {
            const def = target.reflect.props[prop];
            if (def?.type === "accessor") return def.get.call(ctx, ctx[symbols.receiver], error);
            if (!ctx.fiber.runtime) return ctx.reflect.get(prop, false);
            return ctx.events.waterfall("internal/get", ctx, prop, error, () => {
              const key = target[symbols.isolate][prop];
              let fiber = (ctx[symbols.shadow] ?? ctx).fiber;
              while (true) {
                const impl = fiber.store?.[prop];
                if (impl) return getTraceable(ctx, impl.value);
                if (prop in fiber.inject) {
                  error.message = `cannot get required service "${prop}" in inactive context`;
                  throw error;
                }
                if (!fiber.runtime) throw error;
                if (fiber.parent[symbols.isolate][prop] !== key) throw error;
                fiber = fiber.parent.fiber;
              }
            });
          } catch (e) {
            throw e === error ? enhanceError(e) : e;
          }
        },
        set: (target, prop, value, ctx) => {
          if (isSpecialProperty(prop)) return Reflect.set(target, prop, value, ctx);
          const error = /* @__PURE__ */ new Error(`cannot set property "${prop}" without provide`);
          const def = target.reflect.props[prop];
          if (!def) {
            if (!ctx.fiber.runtime) return Reflect.set(target, prop, value, ctx);
            throw enhanceError(error);
          }
          try {
            if (def.type === "accessor") {
              if (!def.set) return false;
              return def.set.call(ctx, value, ctx[symbols.receiver], error);
            }
            return ctx.events.waterfall("internal/set", ctx, prop, value, error, () => {
              return ctx.reflect.set(prop, value, error);
            });
          } catch (e) {
            throw e === error ? enhanceError(e) : e;
          }
        },
        has: (target, prop) => {
          if (isSpecialProperty(prop)) return Reflect.has(target, prop);
          if (Reflect.has(target, prop)) return true;
          return !!target.reflect.props[prop];
        }
      };
      /** Service implementations, keyed by isolation label. */
      store = /* @__PURE__ */ Object.create(null);
      /** Declared context properties (services and accessors), by name. */
      props = /* @__PURE__ */ Object.create(null);
      constructor(ctx) {
        this.ctx = ctx;
        defineProperty(this, symbols.tracker, {
          property: "ctx",
          noShadow: true
        });
        this.mixin("reflect", [
          "get",
          "set",
          "provide",
          "accessor",
          "mixin"
        ]);
        this.mixin("fiber", ["runtime", "effect"]);
        this.mixin("registry", ["inject", "plugin"]);
        this.mixin("events", [
          "on",
          "once",
          "parallel",
          "emit",
          "serial",
          "bail",
          "waterfall"
        ]);
      }
      /**
      * Read a service from the store without the inject requirement.
      *
      * @param name — the service name.
      * @param strict — when `true`, only return implementations whose providing
      * fiber is currently active.
      * @returns the service value, or `undefined` when not (yet) provided.
      */
      get(name, strict = true) {
        return getTraceable(this.ctx, this._getImpl(name, strict)?.value);
      }
      _getImpl(name, strict = true) {
        const key = this.ctx[symbols.isolate][name];
        const impl = key && this.store[key];
        if (!impl) return;
        if (strict && impl.fiber.state !== 2) return;
        return impl;
      }
      /**
      * Overwrite a provided service's value.
      *
      * @param name — the service name.
      * @param value — the new service value.
      * @param error — carrier for the caller stack in diagnostics.
      * @returns `true` on success.
      * @throws when `name` was never provided, or was provided by another fiber.
      */
      set(name, value, error) {
        const key = this.ctx[symbols.isolate][name];
        const impl = this.store[key];
        if (!impl) throw new Error(`cannot set property "${name}" without provide`);
        if (impl.fiber !== this.ctx.fiber) throw new Error(`cannot set property "${name}" in multiple fibers`);
        impl.value = value;
        return true;
      }
      /**
      * Register a service implementation owned by the current fiber.
      *
      * See the `ctx.provide()` overload above for the full contract.
      *
      * @param name — the service name.
      * @param value — the service value.
      * @param check — optional availability predicate for dependents.
      * @returns a disposer that unregisters the service.
      */
      provide(name, value, check) {
        return this.ctx.fiber.effect(() => {
          if (!this.props[name]) this.props[name] ??= { type: "service" };
          else if (this.props[name].type !== "service") throw new Error(`property "${name}" is already declared as ${this.props[name].type}`);
          this.props[name] = { type: "service" };
          this.ctx.root[symbols.isolate][name] ??= Symbol(name);
          const key = this.ctx[symbols.isolate][name];
          const impl = {
            name,
            value,
            fiber: this.ctx.fiber,
            check
          };
          if (this.store[key]) throw new Error(`service "${name}" has been registered at <${this.store[key].fiber.name}>`);
          this.store[key] = impl;
          this.ctx.fiber.store[name] = impl;
          if (this.ctx.fiber.state === 2) this.notify([name]);
          return async () => {
            delete this.store[key];
            const fibers = this.notify([name]);
            await Promise.allSettled(fibers.map((fiber) => fiber.await()));
            delete this.ctx.fiber.store[name];
          };
        }, `ctx.provide(${JSON.stringify(name)})`);
      }
      /**
      * Re-evaluate every fiber that requires one of the given services.
      *
      * @param names — the service names that changed.
      * @param filter — restricts notification to matching isolation scopes.
      * @returns the fibers whose dependency state was refreshed.
      */
      notify(names, filter = (ctx, name) => ctx[symbols.isolate][name] === this.ctx[symbols.isolate][name]) {
        const fibers = [];
        for (const runtime of this.ctx.registry.values()) for (const fiber of runtime.fibers) {
          let hasUpdate = false;
          for (const name of names) {
            if (!(name in fiber.inject)) continue;
            if (!filter(fiber.ctx, name)) continue;
            hasUpdate = true;
            fiber._checkImpl(name);
          }
          if (!hasUpdate) continue;
          fiber._refresh();
          fibers.push(fiber);
        }
        for (const name of names) {
          const self = Object.create(this.ctx);
          self[symbols.filter] = (target) => filter(target, name);
          this.ctx.events.emit(self, "internal/service", name, this._getImpl(name, false)?.value);
        }
        return fibers;
      }
      /**
      * Define a computed context property backed by get/set hooks.
      *
      * @param name — the context property name.
      * @param options — the `get` hook and optional `set` hook.
      * @returns a disposer that removes the accessor.
      */
      accessor(name, options) {
        return this.ctx.fiber.effect(() => {
          if (name in this.props) throw new Error(`property "${name}" is already declared as ${this.props[name].type}`);
          this.props[name] = {
            type: "accessor",
            ...options
          };
          return () => delete this.props[name];
        }, `ctx.accessor(${JSON.stringify(name)})`);
      }
      /**
      * Expose selected members of a service directly on `ctx`.
      *
      * See the `ctx.mixin()` overload above for the full contract.
      *
      * @param source — a context property name or a source object.
      * @param mixins — keys to forward, or a source-key → ctx-key map.
      * @returns a disposer that removes all created accessors.
      */
      mixin(source, mixins) {
        const self = this;
        return this.ctx.fiber.effect(function* () {
          const entries = Array.isArray(mixins) ? mixins.map((key) => [key, key]) : Object.entries(mixins);
          const getTarget = (ctx, error) => {
            return ctx[source];
          };
          for (const [key, value] of entries) yield self.accessor(value, {
            get(receiver, error) {
              const service = getTarget(this, error);
              if (isNullable(service)) return service;
              const mixin = receiver ? withProps(receiver, service) : service;
              const value2 = Reflect.get(service, key, mixin);
              if (typeof value2 !== "function") return value2;
              return value2.bind(mixin ?? service);
            },
            set(value2, receiver, error) {
              const service = getTarget(this, error);
              const mixin = receiver ? withProps(receiver, service) : service;
              return Reflect.set(service, key, value2, mixin);
            }
          });
        }, `ctx.mixin(${JSON.stringify(source)})`);
      }
      /**
      * Attach this context's tracing wrapper to a value.
      *
      * @param value — the value to wrap.
      * @returns the traceable wrapper (or the value itself when not applicable).
      */
      trace(value) {
        return getTraceable(this.ctx, value);
      }
      /**
      * Wrap a callback so calls trace `this` and arguments to this context.
      *
      * @param callback — the function to wrap.
      * @returns a proxy delegating to `callback` with traced values.
      */
      bind(callback) {
        return new Proxy(callback, {
          apply: (target, thisArg, args) => {
            return Reflect.apply(target, this.trace(thisArg), args.map((arg) => this.trace(arg)));
          },
          construct: (target, args, newTarget) => {
            return Reflect.construct(target, args.map((arg) => this.trace(arg)), newTarget);
          }
        });
      }
    };
    kValidationError = /* @__PURE__ */ Symbol.for("ValidationError");
    ValidationError = class extends TypeError {
      name = "ValidationError";
      /**
      * Build the aggregated message from schema issues.
      *
      * @param issues — the standard-schema issues, one message line each.
      */
      constructor(issues) {
        super(`invalid config:
` + issues.map((issue) => {
          if (issue.path) return `  - ${issue.message} (at ${issue.path.join(".")})`;
          else return `  - ${issue.message}`;
        }).join("\n"));
      }
    };
    Object.defineProperty(ValidationError.prototype, kValidationError, { value: true });
    effectInertia = /* @__PURE__ */ new WeakMap();
    CordisError = class CordisError2 extends Error {
      code;
      /**
      * @param code — the stable error code; also the default message.
      * @param message — optional human-readable override.
      */
      constructor(code, message) {
        super(message ?? CordisError2.Code[code]);
        this.code = code;
      }
    };
    (function(CordisError3) {
      CordisError3.Code = { INACTIVE_EFFECT: "cannot create effect on inactive context" };
    })(CordisError || (CordisError = {}));
    INACTIVE = "__INACTIVE__";
    Fiber = class {
      parent;
      inject;
      runtime;
      /** Unique id within the registry; 0 for the root fiber, `null` once disposed. */
      uid;
      /** The context this fiber's plugin runs in (extends the parent context). */
      ctx;
      /** The validated plugin config (updated by `update()`). */
      config;
      /** The raw plugin config, re-resolved before each activation. */
      _config;
      /** Current lifecycle state; transitions emit `internal/status`. */
      state = 0;
      /** Dispose this fiber: unload the plugin, then settle once cleanup finished. */
      dispose;
      /** Snapshot of required service implementations while loaded; `undefined` otherwise. */
      store;
      /** The in-flight load/unload transition, if one is currently running. */
      inertia;
      _hooks = /* @__PURE__ */ Object.create(null);
      _disposables = new DisposableList();
      context;
      _error;
      _runner;
      _store = /* @__PURE__ */ Object.create(null);
      /**
      * Create a fiber. Plugin authors normally obtain fibers from `ctx.plugin()`
      * rather than constructing them directly.
      *
      * @param parent — the context the plugin was loaded from.
      * @param config — raw config, validated against the runtime's schema.
      * @param inject — resolved dependency map (service name → intercept config).
      * @param runtime — the shared plugin runtime, or `null` for the root fiber.
      * @param getOuterStack — captures the caller stack for effect diagnostics.
      */
      constructor(parent, config, inject, runtime, getOuterStack) {
        this.parent = parent;
        this.inject = inject;
        this.runtime = runtime;
        this._config = config;
        const collect = (dispose) => {
          this._disposables.push(dispose);
        };
        if (runtime) {
          this.uid = parent.registry.counter;
          this.ctx = this.context = parent.extend({ fiber: this });
          const injectEntries = Object.entries(this.inject);
          if (injectEntries.length) {
            this.ctx[Context.intercept] = Object.create(parent[Context.intercept]);
            for (const [name, config2] of injectEntries) {
              if (isNullable(config2)) continue;
              this.ctx[Context.intercept][name] = config2;
            }
          }
          this._runner = {
            epoch: INACTIVE,
            getOuterStack,
            execute: function() {
              if (isConstructor(runtime.callback)) {
                const instance = new runtime.callback(this.ctx, this.config);
                for (const hook of instance?.[symbols.initHooks] ?? []) hook();
                return instance?.[symbols.init]?.();
              } else return runtime.callback(this.ctx, this.config);
            },
            collect
          };
          this.dispose = parent.fiber.effect(() => {
            const remove = runtime.fibers.push(this);
            return async () => {
              this.uid = null;
              emitPluginDisposed(this.context, this);
              if (this.ctx.registry.has(runtime.callback)) {
                remove();
                if (!runtime.fibers.length) this.ctx.registry.delete(runtime.callback);
              }
              this._setEpoch(INACTIVE);
              if (!this.inertia) this._updateState(() => {
                this.inertia = this._unload();
                return 5;
              });
              while (this.inertia) await this.inertia;
            };
          }, "ctx.plugin()");
          try {
            this.context.emit("internal/plugin", this);
          } catch (error) {
            Promise.resolve(this.dispose()).catch((reason) => this.ctx.logger.error(reason));
            throw error;
          }
          if (this.uid !== null && parent.fiber.state !== 5) {
            for (const name of Object.keys(this.inject)) this._checkImpl(name);
            this._refresh();
          }
        } else {
          this.uid = 0;
          this.ctx = this.context = parent;
          this.state = 2;
          this.store = /* @__PURE__ */ Object.create(null);
          this._runner = {
            epoch: "",
            getOuterStack,
            execute: () => {
            },
            collect
          };
          this.dispose = () => this.restart();
        }
      }
      /** The plugin's display name, inherited from the nearest named ancestor, else `'root'`. */
      get name() {
        let fiber = this;
        do {
          if (fiber.runtime?.name) return fiber.runtime.name;
          fiber = fiber.parent.fiber;
        } while (fiber !== fiber.parent.fiber);
        return "root";
      }
      /**
      * Throw if the fiber has already been disposed.
      *
      * @returns nothing when the fiber is still active.
      * @throws {CordisError} `INACTIVE_EFFECT` when the fiber's uid has been cleared.
      */
      assertActive() {
        if (this.uid !== null) return;
        throw new CordisError("INACTIVE_EFFECT");
      }
      _execute(runner) {
        const oldEpoch = runner.epoch;
        return composeError((info) => {
          const safeCollect = (dispose) => {
            if (typeof dispose === "function") runner.collect(dispose);
            else if (!isNullable(dispose)) throw new TypeError("Invalid effect");
          };
          const effect = runner.execute.call(this);
          if (typeof effect === "function") return runner.collect(effect);
          else if (isNullable(effect)) {
          } else if (!isObject(effect)) throw new TypeError("Invalid effect");
          else if ("then" in effect) return effect.then(safeCollect);
          else if (Symbol.iterator in effect) {
            info.error = /* @__PURE__ */ new Error();
            const iter = effect[Symbol.iterator]();
            while (true) {
              const result = iter.next();
              safeCollect(result.value);
              if (result.done) return;
            }
          } else if (Symbol.asyncIterator in effect) {
            const iter = effect[Symbol.asyncIterator]();
            return (async () => {
              await Promise.resolve();
              info.error = /* @__PURE__ */ new Error();
              while (true) {
                if (runner.epoch !== oldEpoch) return;
                const result = await iter.next();
                safeCollect(result.value);
                if (result.done) return;
              }
            })();
          } else throw new TypeError("Invalid effect");
        }, runner.getOuterStack);
      }
      effect(execute, label = "anonymous") {
        this.assertActive();
        if (this.state === 5) throw new CordisError("INACTIVE_EFFECT");
        const disposables = [];
        let disposing = false;
        let disposalTask;
        const dispose = () => {
          if (disposing) return disposalTask;
          disposing = true;
          let task2;
          for (const disposable of disposables.splice(0).reverse()) if (task2) task2 = task2.then(() => runDisposable(disposable));
          else {
            const result = runDisposable(disposable);
            if (isObject(result) && "then" in result) task2 = result;
          }
          return disposalTask = task2;
        };
        const meta = {
          label,
          children: []
        };
        const runner = {
          execute,
          epoch: true,
          collect: (dispose2) => {
            disposables.push(dispose2);
            this._disposables.delete(dispose2);
            if (dispose2[symbols.effect]) meta.children.push(dispose2[symbols.effect]);
          },
          getOuterStack: buildOuterStack()
        };
        let task;
        let executing = true;
        let resolveSetup;
        let rejectSetup;
        let setupBarrier;
        let setupFailed = false;
        let inFlight;
        let removeWrapper = () => false;
        const waitForSetup = () => {
          setupBarrier ??= new Promise((resolve6, reject) => {
            resolveSetup = resolve6;
            rejectSetup = reject;
          });
          return setupBarrier;
        };
        const disposeAfter = (setup) => {
          return Promise.resolve(setup).then(() => dispose(), async (reason) => {
            await dispose();
            throw reason;
          });
        };
        const finalizeDisposal = (callback) => {
          let result;
          try {
            result = callback();
          } catch (error) {
            removeWrapper();
            throw error;
          }
          if (isObject(result) && "then" in result) {
            const pending = Promise.resolve(result).finally(() => {
              removeWrapper();
              if (inFlight === pending) inFlight = void 0;
            });
            return inFlight = pending;
          }
          removeWrapper();
          return result;
        };
        const wrapper = defineProperty(() => {
          if (!runner.epoch) return setupFailed ? inFlight : void 0;
          runner.epoch = false;
          return finalizeDisposal(() => {
            if (executing) return disposeAfter(waitForSetup());
            return task ? disposeAfter(task) : dispose();
          });
        }, symbols.effect, meta);
        effectInertia.set(wrapper, () => inFlight);
        removeWrapper = this._disposables.push(wrapper);
        try {
          task = this._execute(runner);
        } catch (reason) {
          executing = false;
          setupFailed = true;
          runner.epoch = false;
          let cleanup;
          try {
            cleanup = finalizeDisposal(dispose);
          } finally {
            rejectSetup?.(reason);
          }
          if (isObject(cleanup) && "then" in cleanup) cleanup.catch((error) => this.ctx.logger.error(error));
          throw reason;
        }
        executing = false;
        if (setupBarrier) Promise.resolve(task).then(resolveSetup, rejectSetup);
        task?.catch(() => {
          if (!runner.epoch) return dispose();
          return finalizeDisposal(dispose);
        }).catch((error) => this.ctx.logger.error(error));
        const disposeAsync = () => {
          if (!runner.epoch) return;
          runner.epoch = false;
          return finalizeDisposal(dispose);
        };
        wrapper.then = async (onFulfilled, onRejected) => {
          return Promise.resolve(task).then(() => disposeAsync).then(onFulfilled, onRejected);
        };
        return wrapper;
      }
      /**
      * Return metadata for currently registered effects.
      *
      * @returns one {@link EffectMeta} tree per labeled live effect.
      */
      getEffects() {
        return [...this._disposables].map((dispose) => dispose[symbols.effect]).filter(Boolean);
      }
      _getState() {
        if (this.uid === null) return 4;
        if (this._error) return 3;
        if (this._runner.epoch !== INACTIVE) return 2;
        return 0;
      }
      _updateState(callback) {
        const oldState = this.state;
        this.state = callback() ?? this._getState();
        if (oldState === this.state) return;
        this.context.emit("internal/status", this, oldState);
        if (oldState !== 2 && this.state !== 2) return;
        for (const key of Reflect.ownKeys(this.ctx.reflect.store)) {
          const impl = this.ctx.reflect.store[key];
          if (impl.fiber !== this) continue;
          this.ctx.reflect.notify([impl.name]);
        }
      }
      _checkImpl(name) {
        const impl = this.ctx.reflect._getImpl(name, true);
        if (!impl) return delete this._store[name];
        try {
          if (impl.check && !impl.check.call(getTraceable(this.ctx, impl.value))) return delete this._store[name];
        } catch (error) {
          impl.fiber.ctx.logger.error(error);
          return delete this._store[name];
        }
        this._store[name] = impl;
      }
      _refresh() {
        let epoch = false;
        epoch = "";
        for (const name of Object.keys(this.inject)) {
          const impl = this._store[name];
          if (!impl) {
            epoch = INACTIVE;
            break;
          }
          epoch += ":" + impl.fiber.uid;
        }
        this._setEpoch(epoch);
      }
      _setEpoch(epoch) {
        const oldEpoch = this._runner.epoch;
        if (epoch === oldEpoch) return;
        this._runner.epoch = epoch;
        if (this.inertia) return;
        this._updateState(() => {
          if (epoch !== INACTIVE && oldEpoch === INACTIVE) {
            this.inertia = this._reload();
            return 1;
          } else {
            this.inertia = this._unload();
            return 5;
          }
        });
      }
      _resolveConfig(config) {
        config = this.context.waterfall(this, "internal/config", config, () => config);
        return this.runtime ? resolveConfig(this.runtime, config) : config;
      }
      async _reload() {
        this.store = { ...this._store };
        const oldEpoch = this._runner.epoch;
        try {
          await Promise.resolve();
          if (this._runner.epoch === oldEpoch) {
            this.config = this._resolveConfig(this._config);
            await this._execute(this._runner);
            this._error = void 0;
          }
        } catch (reason) {
          this.ctx.logger.error(reason);
          this._error = reason;
          this._runner.epoch = INACTIVE;
        }
        this._updateState(() => {
          if (this._runner.epoch === oldEpoch) this.inertia = void 0;
          else {
            this.inertia = this._unload();
            return 5;
          }
        });
      }
      async _unload() {
        await Promise.all(this._disposables.clear().map(async (dispose) => {
          try {
            await composeError(async (info) => {
              await Promise.resolve();
              info.error = /* @__PURE__ */ new Error();
              await runDisposable(dispose);
            }, this._runner.getOuterStack);
          } catch (reason) {
            this.ctx.logger.error(reason);
          }
        }));
        this.store = void 0;
        this._updateState(() => {
          if (this._runner.epoch === INACTIVE) this.inertia = void 0;
          else {
            this.inertia = this._reload();
            return 1;
          }
        });
      }
      /**
      * Wait for current lifecycle work and rethrow startup errors.
      *
      * @returns this fiber, once it has settled into a stable state.
      * @throws the config-validation or plugin-startup error, if any.
      */
      async await() {
        while (this.inertia) await this.inertia;
        if (this._error) throw this._error;
        return this;
      }
      /**
      * Dispose and immediately reload this plugin with its current config.
      *
      * @returns a promise resolving once the reload settled.
      * @throws {CordisError} `INACTIVE_EFFECT` when the fiber is already disposed.
      */
      async restart() {
        this.assertActive();
        this._setEpoch(INACTIVE);
        this._refresh();
        await this.await();
      }
      /**
      * Validate and apply new config, then restart the plugin.
      *
      * Runs the `internal/update` waterfall first, so update hooks (and HMR)
      * can veto or replace the restart.
      *
      * @param config — the new raw config; validated before anything restarts.
      * @param noSave — hint for persistence hooks not to write the change back.
      * @returns nothing; the restart runs behind the `internal/update` waterfall.
      * @throws {ValidationError} when the new config fails validation.
      */
      update(config, noSave = false) {
        this.assertActive();
        this._config = config;
        if (this.state !== 2) {
          this._error = void 0;
          this._setEpoch(INACTIVE);
          this._refresh();
          return;
        }
        config = this._resolveConfig(config);
        this.context.waterfall(this, "internal/update", config, noSave, () => {
          this.config = config;
          this._error = void 0;
          return this.restart();
        });
      }
    };
    (function(Inject2) {
      function resolve6(inject, result = /* @__PURE__ */ Object.create(null)) {
        if (!inject) return result;
        if (Array.isArray(inject)) for (const name of inject) result[name] = null;
        else if (Reflect.has(inject, symbols.checkProto)) {
          Object.assign(result, resolve6(Object.getPrototypeOf(inject)));
          for (const name of Object.keys(inject)) result[name] = inject[name] ?? null;
        } else for (const name of Object.keys(inject)) result[name] = inject[name] ?? null;
        return result;
      }
      Inject2.resolve = resolve6;
    })(Inject || (Inject = {}));
    RegistryService = class {
      ctx;
      _counter = 0;
      _internal = /* @__PURE__ */ new Map();
      constructor(ctx) {
        this.ctx = ctx;
        defineProperty(this, symbols.tracker, {
          property: "ctx",
          noShadow: true
        });
      }
      /** Allocate the next fiber uid (increments on every read). */
      get counter() {
        return ++this._counter;
      }
      /** Number of registered plugin runtimes. */
      get size() {
        return this._internal.size;
      }
      /**
      * Resolve a supported plugin shape to its executable callback.
      *
      * @param plugin — a function, class, or `{ apply }` object plugin.
      * @returns the callback identifying the plugin, or `undefined` if invalid.
      */
      resolve(plugin) {
        try {
          if (typeof plugin === "function") return plugin;
          if (isApplicable(plugin)) return plugin.apply;
        } catch {
        }
      }
      /**
      * Look up the runtime record for a plugin.
      *
      * @param plugin — any supported plugin shape.
      * @returns the runtime, or `undefined` when the plugin is not registered.
      */
      get(plugin) {
        const key = this.resolve(plugin);
        return key && this._internal.get(key);
      }
      /**
      * Check whether a plugin has a registered runtime.
      *
      * @param plugin — any supported plugin shape.
      * @returns `true` when at least one fiber of the plugin exists.
      */
      has(plugin) {
        const key = this.resolve(plugin);
        return !!key && this._internal.has(key);
      }
      /**
      * Dispose every running fiber for a plugin and remove its runtime record.
      *
      * @param plugin — any supported plugin shape.
      * @returns the removed runtime, or `undefined` when none was registered.
      */
      delete(plugin) {
        const key = this.resolve(plugin);
        const runtime = key && this._internal.get(key);
        if (!runtime) return;
        this._internal.delete(key);
        for (const fiber of runtime.fibers) fiber.dispose();
        return runtime;
      }
      /** Iterate the registered plugin callbacks. */
      keys() {
        return this._internal.keys();
      }
      /** Iterate the registered plugin runtimes. */
      values() {
        return this._internal.values();
      }
      /** Iterate `[callback, runtime]` pairs. */
      entries() {
        return this._internal.entries();
      }
      /**
      * Visit every registered runtime.
      *
      * @param callback — receives each runtime and its identifying callback.
      */
      forEach(callback) {
        return this._internal.forEach(callback);
      }
      /**
      * Start a callback once the requested dependencies are available.
      *
      * @param inject — required services, as an array or a name → config map.
      * @param callback — plugin body called with `(ctx, config)`.
      * @returns the fiber; awaiting it settles once loading finished.
      */
      inject(inject, callback) {
        return this.plugin({
          inject,
          apply: callback,
          name: callback.name
        });
      }
      /**
      * Start a plugin in the current context and return its fiber.
      *
      * Creates (or reuses) the plugin's runtime record, then starts a new fiber
      * under the current context. Throws if `plugin` is not a supported shape or
      * if the current fiber is already disposed.
      *
      * @param plugin — a function, class, or `{ apply }` object plugin.
      * @param config — the plugin config, validated against its `Config` schema.
      * @param getOuterStack — captures the caller stack for effect diagnostics.
      * @returns the fiber; awaiting it settles once loading finished.
      */
      plugin(plugin, config, getOuterStack = buildOuterStack()) {
        const callback = this.resolve(plugin);
        if (!callback) throw new Error('invalid plugin, expect function or object with an "apply" method, received ' + typeof plugin);
        this.ctx.fiber.assertActive();
        let runtime = this._internal.get(callback);
        if (!runtime) {
          let name = plugin.name;
          if (name === "apply") name = void 0;
          runtime = {
            name,
            callback,
            fibers: new DisposableList(),
            Config: plugin.Config
          };
          this._internal.set(callback, runtime);
        }
        const fiber = new Fiber(this.ctx, config, Inject.resolve(plugin.inject), runtime, getOuterStack);
        const wrapped = Object.create(fiber);
        wrapped.then = (onFulfilled, onRejected) => {
          return fiber.await().then(onFulfilled, onRejected);
        };
        return wrapped;
      }
    };
    Context = class Context2 {
      /** Symbol key under which a disposer exposes its {@link EffectMeta} diagnostics tree. */
      static effect = symbols.effect;
      /** Symbol key for a context's listener filter, consulted on every event dispatch. */
      static filter = symbols.filter;
      /** Symbol key of the isolation map (see the `Context[symbols.isolate]` property). */
      static isolate = symbols.isolate;
      /** Symbol key of the intercept map (see the `Context[symbols.intercept]` property). */
      static intercept = symbols.intercept;
      /**
      * Returns true for Cordis context proxies and context prototypes.
      *
      * Works across realms and across multiple copies of cordis, because the
      * brand is keyed by a global symbol rather than by `instanceof`.
      *
      * @param value — the value to test.
      * @returns `true` if `value` is a Cordis context, narrowing its type.
      */
      static is(value) {
        return !!value?.[Context2.is];
      }
      static {
        Context2.is[Symbol.toPrimitive] = () => /* @__PURE__ */ Symbol.for("cordis.is");
        Context2.prototype[Context2.is] = true;
      }
      /** Create the root context and install the built-in services. */
      constructor() {
        this[symbols.isolate] = /* @__PURE__ */ Object.create(null);
        this[symbols.intercept] = /* @__PURE__ */ Object.create(null);
        const self = new Proxy(this, ReflectService.handler);
        this.root = self;
        this.baseUrl = void 0;
        this.fiber = new Fiber(self, {}, /* @__PURE__ */ Object.create(null), null, () => []);
        this.reflect = new ReflectService(self);
        this.registry = new RegistryService(self);
        this.events = new EventsService(self);
        this.logger = new LoggerService(self);
        this.fiber._disposables.clear();
        return self;
      }
      [/* @__PURE__ */ Symbol.for("nodejs.util.inspect.custom")]() {
        return `Context <${this.fiber.name}>`;
      }
      /**
      * Create a child context with extra metadata on top of the current scope.
      *
      * The child prototypally inherits every property of this context; own
      * properties of `meta` shadow the inherited ones. The parent is not mutated.
      *
      * @param meta — own properties (including symbol keys) to define on the child.
      * @returns a child context inheriting from this one.
      */
      extend(meta = {}) {
        const shadow = Reflect.getOwnPropertyDescriptor(this, symbols.shadow)?.value;
        const self = Object.create(getTraceable(this, this));
        for (const prop of Reflect.ownKeys(meta)) Object.defineProperty(self, prop, Reflect.getOwnPropertyDescriptor(meta, prop));
        if (!shadow) return self;
        return Object.assign(Object.create(self), { [symbols.shadow]: shadow });
      }
      /**
      * Create a child context with an independent service scope for `name`.
      *
      * Below the returned context, reads and writes of the service `name`
      * resolve against the new label instead of the parent's, so a different
      * implementation can be provided without affecting the parent scope.
      * Passing the same `label` to two `isolate()` calls joins their scopes.
      *
      * @param name — the service name to isolate.
      * @param label — scope label to join; defaults to a fresh unique symbol.
      * @returns a child context whose `name` service resolves in the new scope.
      */
      isolate(name, label) {
        const shadow = Object.create(this[symbols.isolate]);
        shadow[name] = label ?? Symbol(name);
        return this.extend({ [symbols.isolate]: shadow });
      }
      intercept(name, config) {
        const intercept = Object.create(this[symbols.intercept]);
        intercept[name] = config;
        return this.extend({ [symbols.intercept]: intercept });
      }
    };
    Service = class Service2 {
      ctx;
      /** Symbol key of an instance method run after construction (class plugins). */
      static init = symbols.init;
      /** Symbol key of the availability predicate passed to `ctx.provide()`. */
      static check = symbols.check;
      /** Symbol key of the phantom intercept-config type parameter. */
      static config = symbols.config;
      /** Symbol key of the call body making a service callable (e.g. `ctx.logger()`). */
      static invoke = symbols.invoke;
      /** Symbol key of the helper deriving an extended service instance. */
      static extend = symbols.extend;
      /** Symbol key of the tracker metadata used for context tracing. */
      static tracker = symbols.tracker;
      /** Symbol key of the intercept-config resolution helper below. */
      static resolveConfig = symbols.resolveConfig;
      /** The service name this instance is registered under. */
      name;
      /**
      * Register this instance as `name` in the current context.
      *
      * Calls `ctx.reflect.provide(name, this, this[Service.check])`, so the
      * service is unregistered automatically when the owning fiber unloads.
      * Services with a `[Service.invoke]` body return a callable instance.
      *
      * @param ctx — the context to register in (stored as `this.ctx`).
      * @param name — the service name; defaults to the static `provide` field.
      */
      constructor(ctx, name) {
        this.ctx = ctx;
        name ??= this.constructor["provide"];
        let self = this;
        const tracker = {
          associate: name,
          property: "ctx"
        };
        if (self[symbols.invoke]) self = createCallable(name, joinPrototype(Object.getPrototypeOf(this), Function.prototype), tracker);
        self.ctx = ctx;
        self.name = name;
        defineProperty(self, symbols.tracker, tracker);
        self.ctx.reflect.provide(name, self, this[symbols.check]);
        return self;
      }
      [symbols.filter](ctx) {
        return ctx[symbols.isolate][this.name] === this.ctx[symbols.isolate][this.name];
      }
      [symbols.extend](props) {
        let self;
        if (this[Service2.invoke]) self = createCallable(this.name, this, this[symbols.tracker]);
        else self = Object.create(this);
        return Object.assign(self, props);
      }
      /**
      * Merge intercept config from ancestors with optional base and head values.
      *
      * Entries added closer to the root apply first; `base` is prepended and
      * `head` appended. Uses `Config.merge` when the service declares one,
      * otherwise a shallow `Object.assign`.
      *
      * @param base — lowest-precedence config merged before all intercepts.
      * @param head — highest-precedence config merged after all intercepts.
      * @returns the merged config.
      */
      [symbols.resolveConfig](base, head) {
        let intercept = this.ctx[Context.intercept];
        const configs = [];
        while (this.name in intercept) {
          if (Object.hasOwn(intercept, this.name)) configs.unshift(intercept[this.name]);
          intercept = Object.getPrototypeOf(intercept);
        }
        if (base) configs.unshift(base);
        if (head) configs.push(head);
        if (this["Config"]?.merge) return this["Config"].merge(...configs);
        else return Object.assign({}, ...configs);
      }
      static [Symbol.hasInstance](instance) {
        if (!instance) return false;
        let constructor = instance.constructor;
        while (constructor) {
          constructor = constructor.prototype?.constructor;
          if (constructor === this) return true;
          constructor &&= Object.getPrototypeOf(constructor);
        }
        return false;
      }
    };
  }
});

// src/shims/owned.ts
var init_owned = __esm({
  "src/shims/owned.ts"() {
    "use strict";
  }
});

// src/shims/prompt.ts
function normalizePromptSection(value) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("prompt section must be an object with id and text");
  }
  if (Object.keys(value).some((key) => key !== "id" && key !== "text" && key !== "interpolate")) {
    throw new TypeError("prompt section supports only id and text, plus optional model-cwd interpolation");
  }
  const { id, text: text2, interpolate: interpolate2 } = value;
  if (typeof id !== "string" || !SECTION_ID.test(id)) {
    throw new TypeError("prompt section id must be lower case, start with a letter, and use a-z, 0-9, _ or - (at most 64 characters)");
  }
  if (typeof text2 !== "string" || text2.trim().length === 0) {
    throw new TypeError(`prompt section "${id}" needs non-empty text`);
  }
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(text2)) {
    throw new TypeError(`prompt section "${id}" text contains control characters`);
  }
  if (Buffer.byteLength(text2, "utf8") > MAX_PROMPT_SECTION_BYTES) {
    throw new RangeError(`prompt section "${id}" exceeds ${MAX_PROMPT_SECTION_BYTES} UTF-8 bytes`);
  }
  if (interpolate2 !== void 0 && interpolate2 !== "model-cwd") throw new TypeError("prompt interpolation supports only model-cwd");
  if (interpolate2 !== void 0) validatePromptTemplate(text2);
  return Object.freeze({ id, text: text2, ...interpolate2 === void 0 ? {} : { interpolate: interpolate2 } });
}
function validatePromptTemplate(text2) {
  for (let open = text2.indexOf("{{"); open >= 0; ) {
    const group = /^\{\{([^{}]*)\}\}/u.exec(text2.slice(open));
    if (!group) {
      if (text2.indexOf("}}", open + 2) >= 0) throw new TypeError("malformed prompt variable reference");
      open = text2.indexOf("{{", open + 2);
      continue;
    }
    if (!/^[a-z][a-z0-9_]*$/u.test(group[1])) throw new TypeError("malformed prompt variable reference");
    if (group[1] !== "model" && group[1] !== "cwd") throw new TypeError(`unknown Core prompt variable "{{${group[1]}}}"; supported variables: model, cwd`);
    open = text2.indexOf("{{", open + group[0].length);
  }
}
var MAX_PROMPT_SECTION_BYTES, MAX_PROMPT_OWNER_BYTES, MAX_PROMPT_HOST_BYTES, SECTION_ID;
var init_prompt = __esm({
  "src/shims/prompt.ts"() {
    "use strict";
    init_owned();
    MAX_PROMPT_SECTION_BYTES = 4 * 1024;
    MAX_PROMPT_OWNER_BYTES = 32 * 1024;
    MAX_PROMPT_HOST_BYTES = 128 * 1024;
    SECTION_ID = /^[a-z][a-z0-9_-]{0,63}$/u;
  }
});

// src/dsh/fiber-state.ts
var FiberState;
var init_fiber_state = __esm({
  "src/dsh/fiber-state.ts"() {
    "use strict";
    FiberState = {
      PENDING: 0,
      LOADING: 1,
      ACTIVE: 2,
      FAILED: 3,
      DISPOSED: 4,
      UNLOADING: 5
    };
  }
});

// src/dsh/upstream/loader/src/config/group.ts
var EntryGroup, Group;
var init_group = __esm({
  "src/dsh/upstream/loader/src/config/group.ts"() {
    "use strict";
    init_lib2();
    init_entry();
    EntryGroup = class {
      constructor(ctx, tree) {
        this.ctx = ctx;
        this.tree = tree;
        const entry = ctx.fiber.entry;
        if (entry) entry.subgroup = this;
      }
      ctx;
      tree;
      static key = /* @__PURE__ */ Symbol.for("cordis.group");
      data = [];
      get context() {
        return this.ctx;
      }
      async create(options) {
        const id = this.tree.ensureId(options);
        const entry = this.tree.store[id] ??= new Entry(this.ctx.loader);
        entry.parent = this;
        await entry.update(options, true, true);
        return entry.id;
      }
      unlink(options) {
        const config = this.data;
        const index = config.indexOf(options);
        if (index >= 0) config.splice(index, 1);
      }
      remove(id, isDispose = false) {
        const entry = this.tree.store[id];
        if (!entry) return;
        entry.fiber?.dispose();
        if (!isDispose) {
          this.unlink(entry.options);
        }
        delete this.tree.store[id];
        this.context.emit("loader/partial-dispose", entry, entry.options, false);
      }
      async update(config) {
        const oldConfig = this.data;
        this.data = config;
        const oldMap = Object.fromEntries(oldConfig.map((options) => [options.id, options]));
        const newMap = Object.fromEntries(config.map((options) => [options.id ?? /* @__PURE__ */ Symbol("anonymous"), options]));
        const ids = Reflect.ownKeys({ ...oldMap, ...newMap });
        await Promise.all(ids.map(async (id) => {
          if (newMap[id]) {
            await this.create(newMap[id]).catch((error) => {
              this.ctx.logger.error(error);
            });
          } else {
            this.remove(id);
          }
        }));
      }
      stop() {
        for (const options of this.data) {
          this.remove(options.id, true);
        }
      }
    };
    Group = class extends EntryGroup {
      constructor(ctx, config) {
        super(ctx, ctx.fiber.entry.parent.tree);
        this.ctx = ctx;
        this.config = config;
        ctx.on("internal/update", (config2) => {
          this.update(config2);
        });
      }
      ctx;
      config;
      static initial = [];
      static [EntryGroup.key] = true;
      async *[Service.init]() {
        yield () => this.stop();
        await this.update(this.config);
      }
    };
  }
});

// src/dsh/upstream/loader/src/config/tree.ts
var EntryTree;
var init_tree = __esm({
  "src/dsh/upstream/loader/src/config/tree.ts"() {
    "use strict";
    init_lib2();
    init_lib();
    init_group();
    EntryTree = class _EntryTree {
      static sep = ":";
      ctx;
      enableLogs;
      root;
      store = /* @__PURE__ */ Object.create(null);
      constructor(ctx) {
        this.ctx = ctx.extend({ baseUrl: ctx.baseUrl });
        this.root = new EntryGroup(this.ctx, this);
        const entry = this.ctx.fiber.entry;
        if (entry) entry.subtree = this;
      }
      get context() {
        return this.ctx;
      }
      /** Iterate entries in this tree and any nested subtrees. */
      *entries() {
        for (const entry of Object.values(this.store)) {
          yield entry;
          if (!entry.subtree) continue;
          yield* entry.subtree.entries();
        }
      }
      /** Return pending import and lifecycle tasks owned by this tree. */
      getTasks() {
        return [...this.entries()].map((entry) => entry._initTask || entry.fiber?.inertia).filter(isNonNullable);
      }
      /** Wait until this tree has no pending import or lifecycle tasks. */
      async await() {
        while (true) {
          const tasks = this.getTasks();
          if (!tasks.length) return;
          await Promise.allSettled(tasks);
        }
      }
      ensureId(options) {
        if (!options.id) {
          do {
            options.id = Math.random().toString(16).slice(2, 10);
          } while (this.store[options.id]);
        }
        return options.id;
      }
      /** Resolve an entry by id, including nested ids separated by `EntryTree.sep`. */
      resolve(id) {
        const parts = id.split(_EntryTree.sep);
        let tree = this;
        const final = parts.pop();
        for (const part of parts) {
          tree = tree.store[part]?.subtree;
          if (!tree) throw new Error(`cannot resolve entry ${id}`);
        }
        const entry = tree.store[final];
        if (!entry) throw new Error(`cannot resolve entry ${id}`);
        return entry;
      }
      resolveGroup(id) {
        if (!id) return this.root;
        const entry = this.resolve(id);
        if (!entry.subgroup) throw new Error(`entry ${id} is not a group`);
        return entry.subgroup;
      }
      /** Create an entry in the root group or a nested group. */
      async create(options, parent = null, position = Infinity) {
        const group = this.resolveGroup(parent);
        group.data.splice(position, 0, options);
        group.tree.write();
        return group.create(options);
      }
      /** Stop and remove an entry from its parent group. */
      remove(id) {
        const entry = this.resolve(id);
        entry.parent.remove(id);
        entry.parent.tree.write();
      }
      /** Update an entry and optionally move it to another group. */
      async update(id, options, parent, position) {
        const entry = this.resolve(id);
        const source = entry.parent;
        if (parent !== void 0) {
          const target = this.resolveGroup(parent);
          source.unlink(entry.options);
          target.data.splice(position ?? Infinity, 0, entry.options);
          target.tree.write();
          entry.parent = target;
        }
        source.tree.write();
        return entry.update(options, false, true);
      }
      /** Import a plugin module from a specifier or `cordis:` builtin. */
      import(name, getOuterStack) {
        if (name.startsWith("cordis:")) {
          return this.ctx.loader.builtins[name.slice(7)];
        }
        return composeError(async (info) => {
          info.offset += 3;
          if (this.ctx.loader.internal) {
            return await this.ctx.loader.internal.import(name, this.ctx.baseUrl, {});
          } else if (name.startsWith(".")) {
            return await import(
              /* @vite-ignore */
              new URL(name, this.ctx.baseUrl).href
            );
          } else {
            return await import(
              /* @vite-ignore */
              name
            );
          }
        }, getOuterStack);
      }
    };
  }
});

// src/dsh/upstream/loader/src/config/utils.ts
function interpolate(ctx, value) {
  if (isJsExpr(value)) {
    return evaluate(ctx, value.__jsExpr);
  } else if (!value || typeof value !== "object") {
    return value;
  } else if (Array.isArray(value)) {
    return value.map((item) => interpolate(ctx, item));
  } else {
    return mapValues(value, (item) => interpolate(ctx, item));
  }
}
function isJsExpr(value) {
  return value instanceof Object && "__jsExpr" in value;
}
var evaluate;
var init_utils = __esm({
  "src/dsh/upstream/loader/src/config/utils.ts"() {
    "use strict";
    init_lib();
    evaluate = new Function("ctx", "expr", `
  with (ctx) {
    return eval(expr)
  }
`);
  }
});

// src/dsh/upstream/loader/src/config/entry.ts
function takeEntries(object, keys) {
  const indexed = object;
  const result = [];
  for (const key of keys) {
    if (!(key in object)) continue;
    result.push([key, indexed[key]]);
    delete indexed[key];
  }
  return result;
}
function sortKeys(object, prepend = ["id", "name"], append = ["config"]) {
  const part1 = takeEntries(object, prepend);
  const part2 = takeEntries(object, append);
  const rest = takeEntries(object, Object.keys(object)).sort(([a], [b]) => a.localeCompare(b));
  return Object.assign(object, Object.fromEntries([...part1, ...rest, ...part2]));
}
var Entry;
var init_entry = __esm({
  "src/dsh/upstream/loader/src/config/entry.ts"() {
    "use strict";
    init_lib();
    init_tree();
    init_utils();
    Entry = class _Entry {
      constructor(loader2) {
        this.loader = loader2;
        this.ctx = loader2.ctx.extend({ [_Entry.key]: this });
        this.context.emit("loader/entry-init", this);
      }
      loader;
      static key = /* @__PURE__ */ Symbol.for("cordis.entry");
      ctx;
      fiber;
      parent;
      // safety: call `entry.update()` immediately after creating an entry
      options = {};
      subgroup;
      subtree;
      _initTask;
      get context() {
        return this.ctx;
      }
      get id() {
        let id = this.options.id;
        if (this.parent.tree.ctx.fiber.entry) {
          id = this.parent.tree.ctx.fiber.entry.id + EntryTree.sep + id;
        }
        return id;
      }
      /** True when this entry or any owning parent entry is disabled. */
      get disabled() {
        if (this.options.group) return false;
        let entry = this;
        do {
          if (this.disabledOf(entry.options)) return true;
          entry = entry.parent.ctx.fiber.entry;
        } while (entry);
        return false;
      }
      /**
       * Effective disabled state: a `!!js` expression evaluates against the loader
       * context. The raw node stays in the options, so write-back keeps the form.
       */
      disabledOf(options) {
        return isJsExpr(options.disabled) ? Boolean(this.evaluate(options.disabled.__jsExpr)) : Boolean(options.disabled);
      }
      evaluate(expr) {
        return evaluate(this.ctx, expr);
      }
      _patchContext(diff) {
        this.context.waterfall("loader/patch-context", this, () => {
          Object.setPrototypeOf(this.ctx, this.parent.ctx);
          if (this.fiber?.uid && (diff.includes("config") || this.options.group)) {
            this.fiber.update(this.options.config, true);
          }
        });
      }
      async refresh() {
        if (this.fiber) return;
        if (this.disabled) return;
        await this.init();
      }
      /** Merge new options, restart as needed, and persist through the parent tree. */
      async update(options, create = false, force = false) {
        const legacy = { ...this.options };
        if (create) {
          this.options = options;
        } else {
          for (const [key, value] of Object.entries(options)) {
            if (isNullable(value)) {
              delete this.options[key];
            } else {
              this.options[key] = value;
            }
          }
        }
        sortKeys(this.options);
        if (this.disabled) {
          this.fiber?.dispose();
          return;
        }
        if (this.fiber?.uid) {
          const diff = Object.keys({ ...this.options, ...legacy }).filter((key) => !deepEqual(
            this.options[key],
            legacy[key]
          ));
          if (!diff.length && !force) return;
          this.context.emit("loader/partial-dispose", this, legacy, true);
          this._patchContext(diff);
        } else {
          await this.init();
        }
      }
      getOuterStack = () => {
        let entry = this;
        const result = [];
        do {
          result.push(`    at ${entry.parent.tree.ctx.baseUrl}#${entry.options.id}`);
          entry = entry.parent.ctx.fiber.entry;
        } while (entry);
        return result;
      };
      /** Import and start the configured plugin if it is not already running. */
      async init() {
        try {
          await (this._initTask ??= this._init());
        } finally {
          this._initTask = void 0;
        }
        const notify = () => {
          if (this.loader.getTasks().length) return;
          this.ctx.reflect.notify(["loader"]);
        };
        this.fiber?.await().then(notify, notify);
      }
      async _init() {
        let exports;
        try {
          exports = await this.parent.tree.import(this.options.name, this.getOuterStack);
        } catch (error) {
          this.ctx.logger.error(error);
          return;
        } finally {
          this._initTask = void 0;
        }
        const plugin = this.loader.unwrapExports(exports);
        this._patchContext([]);
        this.loader.showLog(this, "apply");
        this.fiber = this.ctx.registry.plugin(plugin, this.options.config, this.getOuterStack);
      }
    };
  }
});

// src/dsh/upstream/loader/src/config/isolate.ts
function swap(target, source) {
  for (const key of Reflect.ownKeys(target)) {
    Reflect.deleteProperty(target, key);
  }
  for (const key of Reflect.ownKeys(source || {})) {
    Reflect.defineProperty(target, key, Reflect.getOwnPropertyDescriptor(source, key));
  }
}
function isolate(ctx) {
  const realms = /* @__PURE__ */ Object.create(null);
  const delims = /* @__PURE__ */ Object.create(null);
  function access2(entry, name, create = false) {
    let realm;
    const label = entry.options.isolate?.[name];
    if (!label) return;
    if (label === true) {
      realm = entry.realm ??= new LocalRealm(entry);
    } else if (create) {
      realm = realms[label] ??= new GlobalRealm(label);
    } else {
      realm = realms[label];
    }
    return realm?.access(name, create);
  }
  ctx.on("loader/entry-init", (entry) => {
    entry.ctx[Context.intercept] = Object.create(entry.ctx[Context.intercept]);
    entry.ctx[Context.isolate] = Object.create(entry.ctx[Context.isolate]);
  });
  ctx.on("loader/patch-context", (entry, next) => {
    const newMap = Object.create(entry.parent.ctx[Context.isolate]);
    for (const name of Object.keys(entry.options.isolate ?? {})) {
      newMap[name] = access2(entry, name, true);
    }
    const diff = /* @__PURE__ */ Object.create(null);
    const oldMap = entry.ctx[Context.isolate];
    for (const name in { ...newMap, ...delims }) {
      if (newMap[name] === oldMap[name]) continue;
      const delim = delims[name] ??= /* @__PURE__ */ Symbol(`delim:${name}`);
      entry.ctx[delim] = /* @__PURE__ */ Symbol(`${name}#${entry.id}`);
      for (const symbol of [oldMap[name], newMap[name]]) {
        const impl = symbol && entry.ctx.reflect.store[symbol];
        if (!impl) continue;
        if (!impl.fiber) {
          entry.ctx.logger.warn(new Error(`expected service ${name} to be implemented`));
          continue;
        }
        diff[name] = [oldMap[name], newMap[name], entry.ctx[delim], impl.fiber.ctx[delim]];
        if (entry.ctx[delim] !== impl.fiber.ctx[delim]) break;
      }
    }
    Object.setPrototypeOf(entry.ctx[Context.isolate], entry.parent.ctx[Context.isolate]);
    Object.setPrototypeOf(entry.ctx[Context.intercept], entry.parent.ctx[Context.intercept]);
    swap(entry.ctx[Context.isolate], newMap);
    swap(entry.ctx[Context.intercept], entry.options.intercept);
    next();
    for (const [symbol1, symbol2, flag1, flag2] of Object.values(diff)) {
      if (flag1 === flag2 && entry.ctx.reflect.store[symbol1] && !entry.ctx.reflect.store[symbol2]) {
        entry.ctx.reflect.store[symbol2] = entry.ctx.reflect.store[symbol1];
        delete entry.ctx.reflect.store[symbol1];
      }
    }
    ctx.reflect.notify(Object.keys(diff), (ctx2, name) => {
      const [symbol1, symbol2, flag1, flag2] = diff[name];
      const symbol3 = ctx2[Context.isolate][name];
      const flag3 = ctx2[delims[name]];
      return (symbol1 === symbol3 || symbol2 === symbol3) && flag1 === flag3 !== (flag1 === flag2);
    });
    for (const name in delims) {
      if (!Reflect.ownKeys(newMap).includes(name)) {
        delete entry.ctx[delims[name]];
      }
    }
  });
  ctx.on("loader/partial-dispose", (entry, legacy, active) => {
    for (const [name, label] of Object.entries(legacy.isolate ?? {})) {
      if (label === true) continue;
      if (active && entry.options.isolate?.[name] === label) continue;
      const realm = realms[label];
      if (!realm) continue;
      for (const entry2 of ctx.loader.entries()) {
        if (entry2.options.isolate?.[name] === realm.label) return;
      }
      realm.delete(name);
      if (!realm.size) {
        delete realms[realm.label];
      }
    }
  });
}
var Realm, LocalRealm, GlobalRealm;
var init_isolate = __esm({
  "src/dsh/upstream/loader/src/config/isolate.ts"() {
    "use strict";
    init_lib2();
    Realm = class {
      store = /* @__PURE__ */ Object.create(null);
      access(key, create = false) {
        if (create) {
          return this.store[key] ??= /* @__PURE__ */ Symbol(`${key}${this.suffix}`);
        } else {
          return this.store[key] ?? /* @__PURE__ */ Symbol(`${key}${this.suffix}`);
        }
      }
      delete(key) {
        delete this.store[key];
      }
      get size() {
        return Object.keys(this.store).length;
      }
    };
    LocalRealm = class extends Realm {
      constructor(entry) {
        super();
        this.entry = entry;
      }
      entry;
      get suffix() {
        return "#" + this.entry.options.id;
      }
    };
    GlobalRealm = class extends Realm {
      constructor(label) {
        super();
        this.label = label;
      }
      label;
      get suffix() {
        return "@" + this.label;
      }
    };
  }
});

// src/dsh/upstream/loader/src/internal.ts
import { createRequire } from "node:module";
var ModuleLoader;
var init_internal = __esm({
  "src/dsh/upstream/loader/src/internal.ts"() {
    "use strict";
    ((ModuleLoader2) => {
      let _cachedLoader;
      function requireInternal(id) {
        const require3 = createRequire(import.meta.url);
        if (process.execArgv.includes("--expose-internals")) {
          try {
            return require3(id);
          } catch {
          }
        }
        try {
          return require3("node-addon-require-builtin").requireBuiltin(id);
        } catch {
        }
      }
      function fromInternal() {
        if (_cachedLoader) return _cachedLoader;
        const [major] = process.versions.node.split(".").map(Number);
        if (major < 22) return;
        const raw = requireInternal("internal/modules/esm/loader")?.getOrInitializeCascadedLoader();
        if (!raw) return;
        const version2 = typeof raw.getOrCreateModuleJob === "function" ? "v2" : typeof raw.getModuleJobForImport === "function" ? "v1" : void 0;
        if (!version2) return;
        return _cachedLoader = Object.assign(raw, { version: version2 });
      }
      ModuleLoader2.fromInternal = fromInternal;
    })(ModuleLoader || (ModuleLoader = {}));
  }
});

// src/dsh/upstream/loader/src/index.ts
var Loader;
var init_src = __esm({
  "src/dsh/upstream/loader/src/index.ts"() {
    "use strict";
    init_lib2();
    init_lib();
    init_entry();
    init_group();
    init_isolate();
    init_tree();
    init_utils();
    init_fiber_state();
    init_entry();
    init_group();
    init_isolate();
    init_tree();
    init_utils();
    init_internal();
    Loader = class extends EntryTree {
      constructor(ctx, config = {}) {
        super(ctx);
        this.config = config;
        if (config.baseUrl) {
          this.ctx.baseUrl = config.baseUrl;
        }
        const self = this;
        defineProperty(this, Service.tracker, {
          associate: "loader",
          property: "ctx",
          noShadow: true
        });
        ctx.reflect.provide("loader", this, this[Service.check]);
        ctx.on("internal/config", function(_config, next) {
          const config2 = next();
          if (!this.entry || this.parent.fiber?.entry === this.entry) return config2;
          const plugin = this.runtime?.callback;
          if (plugin?.[EntryGroup.key]) return config2;
          return interpolate(this.ctx, config2);
        }, { global: true });
        ctx.on("internal/update", function(config2, noSave, next) {
          if (!this.entry || noSave || this.parent.fiber?.entry === this.entry) return next();
          const unparse = this.runtime?.Config?.simplify;
          this.entry.options.config = unparse ? unparse(config2) : config2;
          this.entry.parent.tree.write();
          return next();
        }, { global: true, prepend: true });
        ctx.on("internal/update", function(config2, _, next) {
          if (!this.entry || this.parent.fiber?.entry === this.entry) return next();
          self.showLog(this.entry, "reload");
          return next();
        }, { global: true });
        ctx.on("internal/plugin", (fiber) => {
          if (fiber.parent[Entry.key] && !fiber.entry) {
            fiber.entry = fiber.parent[Entry.key];
            Inject.resolve(fiber.entry.options.inject, fiber.inject);
          }
          if (fiber.uid) return;
          if (!fiber.entry) return;
          if (fiber.parent.fiber?.entry === fiber.entry) return;
          if (!ctx.registry.has(fiber.runtime.callback)) return;
          const treeOwner = fiber.entry.parent.tree.ctx.fiber;
          if (!treeOwner.uid || treeOwner.state === FiberState.UNLOADING) return;
          this.showLog(fiber.entry, "unload");
          if (fiber.entry.disabled) return;
          fiber.entry.options.disabled = true;
          fiber.entry.parent.tree.write();
        });
        ctx.plugin(isolate);
      }
      config;
      envData = process.env.CORDIS_SHARED ? JSON.parse(process.env.CORDIS_SHARED) : { startTime: Date.now() };
      name = "loader";
      // Codewhale host delta: native internals/addon probing is unavailable.
      // EntryTree retains upstream's documented native ESM fallback.
      internal = void 0;
      builtins = /* @__PURE__ */ Object.create(null);
      write() {
      }
      [Service.check]() {
        const config = Service.prototype[Service.resolveConfig].call(this);
        if (config.await && this.getTasks().length) return false;
        return true;
      }
      showLog(entry, type2) {
        if (entry.options.group || !entry.parent.tree.enableLogs) return;
        this.ctx.root.logger?.("loader").info("%s plugin %C", type2, entry.options.name);
      }
      /** Return the loader entry id that owns `fiber`, if any. */
      locate(fiber = this.ctx.fiber) {
        while (1) {
          if (fiber.entry) return fiber.entry.id;
          const next = fiber.parent.fiber;
          if (fiber === next) return;
          fiber = next;
        }
      }
      /** Hook for hosts that can restart the process on full-reload requests. */
      exit() {
      }
      /** Normalize ESM/CJS/default export shapes before applying a plugin. */
      unwrapExports(exports) {
        if (isNullable(exports)) return exports;
        exports = exports.default ?? exports;
        if (!exports.__esModule) return exports;
        return exports.default ?? exports;
      }
    };
  }
});

// node_modules/js-yaml/dist/js-yaml.mjs
function getDefaultExportFromCjs(x) {
  return x && x.__esModule && Object.prototype.hasOwnProperty.call(x, "default") ? x["default"] : x;
}
function requireCommon() {
  if (hasRequiredCommon) return common;
  hasRequiredCommon = 1;
  function isNothing(subject) {
    return typeof subject === "undefined" || subject === null;
  }
  function isObject2(subject) {
    return typeof subject === "object" && subject !== null;
  }
  function toArray(sequence) {
    if (Array.isArray(sequence)) return sequence;
    else if (isNothing(sequence)) return [];
    return [sequence];
  }
  function extend2(target, source) {
    if (source) {
      const sourceKeys = Object.keys(source);
      for (let index = 0, length = sourceKeys.length; index < length; index += 1) {
        const key = sourceKeys[index];
        target[key] = source[key];
      }
    }
    return target;
  }
  function repeat(string, count) {
    let result = "";
    for (let cycle = 0; cycle < count; cycle += 1) {
      result += string;
    }
    return result;
  }
  function isNegativeZero(number) {
    return number === 0 && Number.NEGATIVE_INFINITY === 1 / number;
  }
  common.isNothing = isNothing;
  common.isObject = isObject2;
  common.toArray = toArray;
  common.repeat = repeat;
  common.isNegativeZero = isNegativeZero;
  common.extend = extend2;
  return common;
}
function requireException() {
  if (hasRequiredException) return exception;
  hasRequiredException = 1;
  function formatError(exception2, compact) {
    let where = "";
    const message = exception2.reason || "(unknown reason)";
    if (!exception2.mark) return message;
    if (exception2.mark.name) {
      where += 'in "' + exception2.mark.name + '" ';
    }
    where += "(" + (exception2.mark.line + 1) + ":" + (exception2.mark.column + 1) + ")";
    if (!compact && exception2.mark.snippet) {
      where += "\n\n" + exception2.mark.snippet;
    }
    return message + " " + where;
  }
  function YAMLException2(reason, mark) {
    Error.call(this);
    this.name = "YAMLException";
    this.reason = reason;
    this.mark = mark;
    this.message = formatError(this, false);
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, this.constructor);
    } else {
      this.stack = new Error().stack || "";
    }
  }
  YAMLException2.prototype = Object.create(Error.prototype);
  YAMLException2.prototype.constructor = YAMLException2;
  YAMLException2.prototype.toString = function toString2(compact) {
    return this.name + ": " + formatError(this, compact);
  };
  exception = YAMLException2;
  return exception;
}
function requireSnippet() {
  if (hasRequiredSnippet) return snippet;
  hasRequiredSnippet = 1;
  const common2 = requireCommon();
  function getLine(buffer, lineStart, lineEnd, position, maxLineLength) {
    let head = "";
    let tail = "";
    const maxHalfLength = Math.floor(maxLineLength / 2) - 1;
    if (position - lineStart > maxHalfLength) {
      head = " ... ";
      lineStart = position - maxHalfLength + head.length;
    }
    if (lineEnd - position > maxHalfLength) {
      tail = " ...";
      lineEnd = position + maxHalfLength - tail.length;
    }
    return {
      str: head + buffer.slice(lineStart, lineEnd).replace(/\t/g, "→") + tail,
      pos: position - lineStart + head.length
      // relative position
    };
  }
  function padStart(string, max) {
    return common2.repeat(" ", max - string.length) + string;
  }
  function makeSnippet(mark, options) {
    options = Object.create(options || null);
    if (!mark.buffer) return null;
    if (!options.maxLength) options.maxLength = 79;
    if (typeof options.indent !== "number") options.indent = 1;
    if (typeof options.linesBefore !== "number") options.linesBefore = 3;
    if (typeof options.linesAfter !== "number") options.linesAfter = 2;
    const re = /\r?\n|\r|\0/g;
    const lineStarts = [0];
    const lineEnds = [];
    let match;
    let foundLineNo = -1;
    while (match = re.exec(mark.buffer)) {
      lineEnds.push(match.index);
      lineStarts.push(match.index + match[0].length);
      if (mark.position <= match.index && foundLineNo < 0) {
        foundLineNo = lineStarts.length - 2;
      }
    }
    if (foundLineNo < 0) foundLineNo = lineStarts.length - 1;
    let result = "";
    const lineNoLength = Math.min(mark.line + options.linesAfter, lineEnds.length).toString().length;
    const maxLineLength = options.maxLength - (options.indent + lineNoLength + 3);
    for (let i2 = 1; i2 <= options.linesBefore; i2++) {
      if (foundLineNo - i2 < 0) break;
      const line2 = getLine(
        mark.buffer,
        lineStarts[foundLineNo - i2],
        lineEnds[foundLineNo - i2],
        mark.position - (lineStarts[foundLineNo] - lineStarts[foundLineNo - i2]),
        maxLineLength
      );
      result = common2.repeat(" ", options.indent) + padStart((mark.line - i2 + 1).toString(), lineNoLength) + " | " + line2.str + "\n" + result;
    }
    const line = getLine(mark.buffer, lineStarts[foundLineNo], lineEnds[foundLineNo], mark.position, maxLineLength);
    result += common2.repeat(" ", options.indent) + padStart((mark.line + 1).toString(), lineNoLength) + " | " + line.str + "\n";
    result += common2.repeat("-", options.indent + lineNoLength + 3 + line.pos) + "^\n";
    for (let i2 = 1; i2 <= options.linesAfter; i2++) {
      if (foundLineNo + i2 >= lineEnds.length) break;
      const line2 = getLine(
        mark.buffer,
        lineStarts[foundLineNo + i2],
        lineEnds[foundLineNo + i2],
        mark.position - (lineStarts[foundLineNo] - lineStarts[foundLineNo + i2]),
        maxLineLength
      );
      result += common2.repeat(" ", options.indent) + padStart((mark.line + i2 + 1).toString(), lineNoLength) + " | " + line2.str + "\n";
    }
    return result.replace(/\n$/, "");
  }
  snippet = makeSnippet;
  return snippet;
}
function requireType() {
  if (hasRequiredType) return type;
  hasRequiredType = 1;
  const YAMLException2 = requireException();
  const TYPE_CONSTRUCTOR_OPTIONS = [
    "kind",
    "multi",
    "resolve",
    "construct",
    "instanceOf",
    "predicate",
    "represent",
    "representName",
    "defaultStyle",
    "styleAliases"
  ];
  const YAML_NODE_KINDS = [
    "scalar",
    "sequence",
    "mapping"
  ];
  function compileStyleAliases(map2) {
    const result = {};
    if (map2 !== null) {
      Object.keys(map2).forEach(function(style) {
        map2[style].forEach(function(alias) {
          result[String(alias)] = style;
        });
      });
    }
    return result;
  }
  function Type2(tag, options) {
    options = options || {};
    Object.keys(options).forEach(function(name) {
      if (TYPE_CONSTRUCTOR_OPTIONS.indexOf(name) === -1) {
        throw new YAMLException2('Unknown option "' + name + '" is met in definition of "' + tag + '" YAML type.');
      }
    });
    this.options = options;
    this.tag = tag;
    this.kind = options["kind"] || null;
    this.resolve = options["resolve"] || function() {
      return true;
    };
    this.construct = options["construct"] || function(data2) {
      return data2;
    };
    this.instanceOf = options["instanceOf"] || null;
    this.predicate = options["predicate"] || null;
    this.represent = options["represent"] || null;
    this.representName = options["representName"] || null;
    this.defaultStyle = options["defaultStyle"] || null;
    this.multi = options["multi"] || false;
    this.styleAliases = compileStyleAliases(options["styleAliases"] || null);
    if (YAML_NODE_KINDS.indexOf(this.kind) === -1) {
      throw new YAMLException2('Unknown kind "' + this.kind + '" is specified for "' + tag + '" YAML type.');
    }
  }
  type = Type2;
  return type;
}
function requireSchema() {
  if (hasRequiredSchema) return schema;
  hasRequiredSchema = 1;
  const YAMLException2 = requireException();
  const Type2 = requireType();
  function compileList(schema22, name) {
    const result = [];
    schema22[name].forEach(function(currentType) {
      let newIndex = result.length;
      result.forEach(function(previousType, previousIndex) {
        if (previousType.tag === currentType.tag && previousType.kind === currentType.kind && previousType.multi === currentType.multi) {
          newIndex = previousIndex;
        }
      });
      result[newIndex] = currentType;
    });
    return result;
  }
  function compileMap() {
    const result = {
      scalar: {},
      sequence: {},
      mapping: {},
      fallback: {},
      multi: {
        scalar: [],
        sequence: [],
        mapping: [],
        fallback: []
      }
    };
    function collectType(type2) {
      if (type2.multi) {
        result.multi[type2.kind].push(type2);
        result.multi["fallback"].push(type2);
      } else {
        result[type2.kind][type2.tag] = result["fallback"][type2.tag] = type2;
      }
    }
    for (let index = 0, length = arguments.length; index < length; index += 1) {
      arguments[index].forEach(collectType);
    }
    return result;
  }
  function Schema2(definition) {
    return this.extend(definition);
  }
  Schema2.prototype.extend = function extend2(definition) {
    let implicit = [];
    let explicit = [];
    if (definition instanceof Type2) {
      explicit.push(definition);
    } else if (Array.isArray(definition)) {
      explicit = explicit.concat(definition);
    } else if (definition && (Array.isArray(definition.implicit) || Array.isArray(definition.explicit))) {
      if (definition.implicit) implicit = implicit.concat(definition.implicit);
      if (definition.explicit) explicit = explicit.concat(definition.explicit);
    } else {
      throw new YAMLException2("Schema.extend argument should be a Type, [ Type ], or a schema definition ({ implicit: [...], explicit: [...] })");
    }
    implicit.forEach(function(type2) {
      if (!(type2 instanceof Type2)) {
        throw new YAMLException2("Specified list of YAML types (or a single Type object) contains a non-Type object.");
      }
      if (type2.loadKind && type2.loadKind !== "scalar") {
        throw new YAMLException2("There is a non-scalar type in the implicit list of a schema. Implicit resolving of such types is not supported.");
      }
      if (type2.multi) {
        throw new YAMLException2("There is a multi type in the implicit list of a schema. Multi tags can only be listed as explicit.");
      }
    });
    explicit.forEach(function(type2) {
      if (!(type2 instanceof Type2)) {
        throw new YAMLException2("Specified list of YAML types (or a single Type object) contains a non-Type object.");
      }
    });
    const result = Object.create(Schema2.prototype);
    result.implicit = (this.implicit || []).concat(implicit);
    result.explicit = (this.explicit || []).concat(explicit);
    result.compiledImplicit = compileList(result, "implicit");
    result.compiledExplicit = compileList(result, "explicit");
    result.compiledTypeMap = compileMap(result.compiledImplicit, result.compiledExplicit);
    return result;
  };
  schema = Schema2;
  return schema;
}
function requireStr() {
  if (hasRequiredStr) return str;
  hasRequiredStr = 1;
  const Type2 = requireType();
  str = new Type2("tag:yaml.org,2002:str", {
    kind: "scalar",
    construct: function(data2) {
      return data2 !== null ? data2 : "";
    }
  });
  return str;
}
function requireSeq() {
  if (hasRequiredSeq) return seq;
  hasRequiredSeq = 1;
  const Type2 = requireType();
  seq = new Type2("tag:yaml.org,2002:seq", {
    kind: "sequence",
    construct: function(data2) {
      return data2 !== null ? data2 : [];
    }
  });
  return seq;
}
function requireMap() {
  if (hasRequiredMap) return map;
  hasRequiredMap = 1;
  const Type2 = requireType();
  map = new Type2("tag:yaml.org,2002:map", {
    kind: "mapping",
    construct: function(data2) {
      return data2 !== null ? data2 : {};
    }
  });
  return map;
}
function requireFailsafe() {
  if (hasRequiredFailsafe) return failsafe;
  hasRequiredFailsafe = 1;
  const Schema2 = requireSchema();
  failsafe = new Schema2({
    explicit: [
      requireStr(),
      requireSeq(),
      requireMap()
    ]
  });
  return failsafe;
}
function require_null() {
  if (hasRequired_null) return _null;
  hasRequired_null = 1;
  const Type2 = requireType();
  function resolveYamlNull(data2) {
    if (data2 === null) return true;
    const max = data2.length;
    return max === 1 && data2 === "~" || max === 4 && (data2 === "null" || data2 === "Null" || data2 === "NULL");
  }
  function constructYamlNull() {
    return null;
  }
  function isNull(object) {
    return object === null;
  }
  _null = new Type2("tag:yaml.org,2002:null", {
    kind: "scalar",
    resolve: resolveYamlNull,
    construct: constructYamlNull,
    predicate: isNull,
    represent: {
      canonical: function() {
        return "~";
      },
      lowercase: function() {
        return "null";
      },
      uppercase: function() {
        return "NULL";
      },
      camelcase: function() {
        return "Null";
      },
      empty: function() {
        return "";
      }
    },
    defaultStyle: "lowercase"
  });
  return _null;
}
function requireBool() {
  if (hasRequiredBool) return bool;
  hasRequiredBool = 1;
  const Type2 = requireType();
  function resolveYamlBoolean(data2) {
    if (data2 === null) return false;
    const max = data2.length;
    return max === 4 && (data2 === "true" || data2 === "True" || data2 === "TRUE") || max === 5 && (data2 === "false" || data2 === "False" || data2 === "FALSE");
  }
  function constructYamlBoolean(data2) {
    return data2 === "true" || data2 === "True" || data2 === "TRUE";
  }
  function isBoolean(object) {
    return Object.prototype.toString.call(object) === "[object Boolean]";
  }
  bool = new Type2("tag:yaml.org,2002:bool", {
    kind: "scalar",
    resolve: resolveYamlBoolean,
    construct: constructYamlBoolean,
    predicate: isBoolean,
    represent: {
      lowercase: function(object) {
        return object ? "true" : "false";
      },
      uppercase: function(object) {
        return object ? "TRUE" : "FALSE";
      },
      camelcase: function(object) {
        return object ? "True" : "False";
      }
    },
    defaultStyle: "lowercase"
  });
  return bool;
}
function requireInt() {
  if (hasRequiredInt) return int;
  hasRequiredInt = 1;
  const common2 = requireCommon();
  const Type2 = requireType();
  function isHexCode(c) {
    return c >= 48 && c <= 57 || c >= 65 && c <= 70 || c >= 97 && c <= 102;
  }
  function isOctCode(c) {
    return c >= 48 && c <= 55;
  }
  function isDecCode(c) {
    return c >= 48 && c <= 57;
  }
  function resolveYamlInteger(data2) {
    if (data2 === null) return false;
    const max = data2.length;
    let index = 0;
    let hasDigits = false;
    if (!max) return false;
    let ch = data2[index];
    if (ch === "-" || ch === "+") {
      ch = data2[++index];
    }
    if (ch === "0") {
      if (index + 1 === max) return true;
      ch = data2[++index];
      if (ch === "b") {
        index++;
        for (; index < max; index++) {
          ch = data2[index];
          if (ch !== "0" && ch !== "1") return false;
          hasDigits = true;
        }
        return hasDigits && isFinite(parseYamlInteger(data2));
      }
      if (ch === "x") {
        index++;
        for (; index < max; index++) {
          if (!isHexCode(data2.charCodeAt(index))) return false;
          hasDigits = true;
        }
        return hasDigits && isFinite(parseYamlInteger(data2));
      }
      if (ch === "o") {
        index++;
        for (; index < max; index++) {
          if (!isOctCode(data2.charCodeAt(index))) return false;
          hasDigits = true;
        }
        return hasDigits && isFinite(parseYamlInteger(data2));
      }
    }
    for (; index < max; index++) {
      if (!isDecCode(data2.charCodeAt(index))) {
        return false;
      }
      hasDigits = true;
    }
    if (!hasDigits) return false;
    return isFinite(parseYamlInteger(data2));
  }
  function parseYamlInteger(data2) {
    let value = data2;
    let sign = 1;
    let ch = value[0];
    if (ch === "-" || ch === "+") {
      if (ch === "-") sign = -1;
      value = value.slice(1);
      ch = value[0];
    }
    if (value === "0") return 0;
    if (ch === "0") {
      if (value[1] === "b") return sign * parseInt(value.slice(2), 2);
      if (value[1] === "x") return sign * parseInt(value.slice(2), 16);
      if (value[1] === "o") return sign * parseInt(value.slice(2), 8);
    }
    return sign * parseInt(value, 10);
  }
  function constructYamlInteger(data2) {
    return parseYamlInteger(data2);
  }
  function isInteger(object) {
    return Object.prototype.toString.call(object) === "[object Number]" && (object % 1 === 0 && !common2.isNegativeZero(object));
  }
  int = new Type2("tag:yaml.org,2002:int", {
    kind: "scalar",
    resolve: resolveYamlInteger,
    construct: constructYamlInteger,
    predicate: isInteger,
    represent: {
      binary: function(obj) {
        return obj >= 0 ? "0b" + obj.toString(2) : "-0b" + obj.toString(2).slice(1);
      },
      octal: function(obj) {
        return obj >= 0 ? "0o" + obj.toString(8) : "-0o" + obj.toString(8).slice(1);
      },
      decimal: function(obj) {
        return obj.toString(10);
      },
      hexadecimal: function(obj) {
        return obj >= 0 ? "0x" + obj.toString(16).toUpperCase() : "-0x" + obj.toString(16).toUpperCase().slice(1);
      }
    },
    defaultStyle: "decimal",
    styleAliases: {
      binary: [2, "bin"],
      octal: [8, "oct"],
      decimal: [10, "dec"],
      hexadecimal: [16, "hex"]
    }
  });
  return int;
}
function requireFloat() {
  if (hasRequiredFloat) return float;
  hasRequiredFloat = 1;
  const common2 = requireCommon();
  const Type2 = requireType();
  const YAML_FLOAT_PATTERN = new RegExp(
    // 2.5e4, 2.5 and integers
    "^(?:[-+]?(?:[0-9]+)(?:\\.[0-9]*)?(?:[eE][-+]?[0-9]+)?|\\.[0-9]+(?:[eE][-+]?[0-9]+)?|[-+]?\\.(?:inf|Inf|INF)|\\.(?:nan|NaN|NAN))$"
  );
  const YAML_FLOAT_SPECIAL_PATTERN = new RegExp(
    "^(?:[-+]?\\.(?:inf|Inf|INF)|\\.(?:nan|NaN|NAN))$"
  );
  function resolveYamlFloat(data2) {
    if (data2 === null) return false;
    if (!YAML_FLOAT_PATTERN.test(data2)) {
      return false;
    }
    if (isFinite(parseFloat(data2, 10))) {
      return true;
    }
    return YAML_FLOAT_SPECIAL_PATTERN.test(data2);
  }
  function constructYamlFloat(data2) {
    let value = data2.toLowerCase();
    const sign = value[0] === "-" ? -1 : 1;
    if ("+-".indexOf(value[0]) >= 0) {
      value = value.slice(1);
    }
    if (value === ".inf") {
      return sign === 1 ? Number.POSITIVE_INFINITY : Number.NEGATIVE_INFINITY;
    } else if (value === ".nan") {
      return NaN;
    }
    return sign * parseFloat(value, 10);
  }
  const SCIENTIFIC_WITHOUT_DOT = /^[-+]?[0-9]+e/;
  function representYamlFloat(object, style) {
    if (isNaN(object)) {
      switch (style) {
        case "lowercase":
          return ".nan";
        case "uppercase":
          return ".NAN";
        case "camelcase":
          return ".NaN";
      }
    } else if (Number.POSITIVE_INFINITY === object) {
      switch (style) {
        case "lowercase":
          return ".inf";
        case "uppercase":
          return ".INF";
        case "camelcase":
          return ".Inf";
      }
    } else if (Number.NEGATIVE_INFINITY === object) {
      switch (style) {
        case "lowercase":
          return "-.inf";
        case "uppercase":
          return "-.INF";
        case "camelcase":
          return "-.Inf";
      }
    } else if (common2.isNegativeZero(object)) {
      return "-0.0";
    }
    const res = object.toString(10);
    return SCIENTIFIC_WITHOUT_DOT.test(res) ? res.replace("e", ".e") : res;
  }
  function isFloat(object) {
    return Object.prototype.toString.call(object) === "[object Number]" && (object % 1 !== 0 || common2.isNegativeZero(object));
  }
  float = new Type2("tag:yaml.org,2002:float", {
    kind: "scalar",
    resolve: resolveYamlFloat,
    construct: constructYamlFloat,
    predicate: isFloat,
    represent: representYamlFloat,
    defaultStyle: "lowercase"
  });
  return float;
}
function requireJson() {
  if (hasRequiredJson) return json;
  hasRequiredJson = 1;
  json = requireFailsafe().extend({
    implicit: [
      require_null(),
      requireBool(),
      requireInt(),
      requireFloat()
    ]
  });
  return json;
}
function requireCore() {
  if (hasRequiredCore) return core;
  hasRequiredCore = 1;
  core = requireJson();
  return core;
}
function requireTimestamp() {
  if (hasRequiredTimestamp) return timestamp;
  hasRequiredTimestamp = 1;
  const Type2 = requireType();
  const YAML_DATE_REGEXP = new RegExp(
    "^([0-9][0-9][0-9][0-9])-([0-9][0-9])-([0-9][0-9])$"
  );
  const YAML_TIMESTAMP_REGEXP = new RegExp(
    "^([0-9][0-9][0-9][0-9])-([0-9][0-9]?)-([0-9][0-9]?)(?:[Tt]|[ \\t]+)([0-9][0-9]?):([0-9][0-9]):([0-9][0-9])(?:\\.([0-9]*))?(?:[ \\t]*(Z|([-+])([0-9][0-9]?)(?::([0-9][0-9]))?))?$"
  );
  function resolveYamlTimestamp(data2) {
    if (data2 === null) return false;
    if (YAML_DATE_REGEXP.exec(data2) !== null) return true;
    if (YAML_TIMESTAMP_REGEXP.exec(data2) !== null) return true;
    return false;
  }
  function constructYamlTimestamp(data2) {
    let fraction = 0;
    let delta = null;
    let match = YAML_DATE_REGEXP.exec(data2);
    if (match === null) match = YAML_TIMESTAMP_REGEXP.exec(data2);
    if (match === null) throw new Error("Date resolve error");
    const year = +match[1];
    const month = +match[2] - 1;
    const day = +match[3];
    if (!match[4]) {
      return new Date(Date.UTC(year, month, day));
    }
    const hour = +match[4];
    const minute = +match[5];
    const second = +match[6];
    if (match[7]) {
      fraction = match[7].slice(0, 3);
      while (fraction.length < 3) {
        fraction += "0";
      }
      fraction = +fraction;
    }
    if (match[9]) {
      const tzHour = +match[10];
      const tzMinute = +(match[11] || 0);
      delta = (tzHour * 60 + tzMinute) * 6e4;
      if (match[9] === "-") delta = -delta;
    }
    const date = new Date(Date.UTC(year, month, day, hour, minute, second, fraction));
    if (delta) date.setTime(date.getTime() - delta);
    return date;
  }
  function representYamlTimestamp(object) {
    return object.toISOString();
  }
  timestamp = new Type2("tag:yaml.org,2002:timestamp", {
    kind: "scalar",
    resolve: resolveYamlTimestamp,
    construct: constructYamlTimestamp,
    instanceOf: Date,
    represent: representYamlTimestamp
  });
  return timestamp;
}
function requireMerge() {
  if (hasRequiredMerge) return merge;
  hasRequiredMerge = 1;
  const Type2 = requireType();
  function resolveYamlMerge(data2) {
    return data2 === "<<" || data2 === null;
  }
  merge = new Type2("tag:yaml.org,2002:merge", {
    kind: "scalar",
    resolve: resolveYamlMerge
  });
  return merge;
}
function requireBinary() {
  if (hasRequiredBinary) return binary;
  hasRequiredBinary = 1;
  const Type2 = requireType();
  const BASE64_MAP = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=\n\r";
  function resolveYamlBinary(data2) {
    if (data2 === null) return false;
    let bitlen = 0;
    const max = data2.length;
    const map2 = BASE64_MAP;
    for (let idx = 0; idx < max; idx++) {
      const code = map2.indexOf(data2.charAt(idx));
      if (code > 64) continue;
      if (code < 0) return false;
      bitlen += 6;
    }
    return bitlen % 8 === 0;
  }
  function constructYamlBinary(data2) {
    const input = data2.replace(/[\r\n=]/g, "");
    const max = input.length;
    const map2 = BASE64_MAP;
    let bits = 0;
    const result = [];
    for (let idx = 0; idx < max; idx++) {
      if (idx % 4 === 0 && idx) {
        result.push(bits >> 16 & 255);
        result.push(bits >> 8 & 255);
        result.push(bits & 255);
      }
      bits = bits << 6 | map2.indexOf(input.charAt(idx));
    }
    const tailbits = max % 4 * 6;
    if (tailbits === 0) {
      result.push(bits >> 16 & 255);
      result.push(bits >> 8 & 255);
      result.push(bits & 255);
    } else if (tailbits === 18) {
      result.push(bits >> 10 & 255);
      result.push(bits >> 2 & 255);
    } else if (tailbits === 12) {
      result.push(bits >> 4 & 255);
    }
    return new Uint8Array(result);
  }
  function representYamlBinary(object) {
    let result = "";
    let bits = 0;
    const max = object.length;
    const map2 = BASE64_MAP;
    for (let idx = 0; idx < max; idx++) {
      if (idx % 3 === 0 && idx) {
        result += map2[bits >> 18 & 63];
        result += map2[bits >> 12 & 63];
        result += map2[bits >> 6 & 63];
        result += map2[bits & 63];
      }
      bits = (bits << 8) + object[idx];
    }
    const tail = max % 3;
    if (tail === 0) {
      result += map2[bits >> 18 & 63];
      result += map2[bits >> 12 & 63];
      result += map2[bits >> 6 & 63];
      result += map2[bits & 63];
    } else if (tail === 2) {
      result += map2[bits >> 10 & 63];
      result += map2[bits >> 4 & 63];
      result += map2[bits << 2 & 63];
      result += map2[64];
    } else if (tail === 1) {
      result += map2[bits >> 2 & 63];
      result += map2[bits << 4 & 63];
      result += map2[64];
      result += map2[64];
    }
    return result;
  }
  function isBinary(obj) {
    return Object.prototype.toString.call(obj) === "[object Uint8Array]";
  }
  binary = new Type2("tag:yaml.org,2002:binary", {
    kind: "scalar",
    resolve: resolveYamlBinary,
    construct: constructYamlBinary,
    predicate: isBinary,
    represent: representYamlBinary
  });
  return binary;
}
function requireOmap() {
  if (hasRequiredOmap) return omap;
  hasRequiredOmap = 1;
  const Type2 = requireType();
  const _hasOwnProperty = Object.prototype.hasOwnProperty;
  const _toString = Object.prototype.toString;
  function resolveYamlOmap(data2) {
    if (data2 === null) return true;
    const objectKeys = {};
    const object = data2;
    for (let index = 0, length = object.length; index < length; index += 1) {
      const pair = object[index];
      let pairHasKey = false;
      if (_toString.call(pair) !== "[object Object]") return false;
      let pairKey;
      for (pairKey in pair) {
        if (_hasOwnProperty.call(pair, pairKey)) {
          if (!pairHasKey) pairHasKey = true;
          else return false;
        }
      }
      if (!pairHasKey) return false;
      if (_hasOwnProperty.call(objectKeys, pairKey)) return false;
      Object.defineProperty(objectKeys, pairKey, { value: true });
    }
    return true;
  }
  function constructYamlOmap(data2) {
    return data2 !== null ? data2 : [];
  }
  omap = new Type2("tag:yaml.org,2002:omap", {
    kind: "sequence",
    resolve: resolveYamlOmap,
    construct: constructYamlOmap
  });
  return omap;
}
function requirePairs() {
  if (hasRequiredPairs) return pairs;
  hasRequiredPairs = 1;
  const Type2 = requireType();
  const _toString = Object.prototype.toString;
  function resolveYamlPairs(data2) {
    if (data2 === null) return true;
    const object = data2;
    const result = new Array(object.length);
    for (let index = 0, length = object.length; index < length; index += 1) {
      const pair = object[index];
      if (_toString.call(pair) !== "[object Object]") return false;
      const keys = Object.keys(pair);
      if (keys.length !== 1) return false;
      result[index] = [keys[0], pair[keys[0]]];
    }
    return true;
  }
  function constructYamlPairs(data2) {
    if (data2 === null) return [];
    const object = data2;
    const result = new Array(object.length);
    for (let index = 0, length = object.length; index < length; index += 1) {
      const pair = object[index];
      const keys = Object.keys(pair);
      result[index] = [keys[0], pair[keys[0]]];
    }
    return result;
  }
  pairs = new Type2("tag:yaml.org,2002:pairs", {
    kind: "sequence",
    resolve: resolveYamlPairs,
    construct: constructYamlPairs
  });
  return pairs;
}
function requireSet() {
  if (hasRequiredSet) return set;
  hasRequiredSet = 1;
  const Type2 = requireType();
  const _hasOwnProperty = Object.prototype.hasOwnProperty;
  function resolveYamlSet(data2) {
    if (data2 === null) return true;
    const object = data2;
    for (const key in object) {
      if (_hasOwnProperty.call(object, key)) {
        if (object[key] !== null) return false;
      }
    }
    return true;
  }
  function constructYamlSet(data2) {
    return data2 !== null ? data2 : {};
  }
  set = new Type2("tag:yaml.org,2002:set", {
    kind: "mapping",
    resolve: resolveYamlSet,
    construct: constructYamlSet
  });
  return set;
}
function require_default() {
  if (hasRequired_default) return _default;
  hasRequired_default = 1;
  _default = requireCore().extend({
    implicit: [
      requireTimestamp(),
      requireMerge()
    ],
    explicit: [
      requireBinary(),
      requireOmap(),
      requirePairs(),
      requireSet()
    ]
  });
  return _default;
}
function requireLoader() {
  if (hasRequiredLoader) return loader;
  hasRequiredLoader = 1;
  const common2 = requireCommon();
  const YAMLException2 = requireException();
  const makeSnippet = requireSnippet();
  const DEFAULT_SCHEMA2 = require_default();
  const _hasOwnProperty = Object.prototype.hasOwnProperty;
  const CONTEXT_FLOW_IN = 1;
  const CONTEXT_FLOW_OUT = 2;
  const CONTEXT_BLOCK_IN = 3;
  const CONTEXT_BLOCK_OUT = 4;
  const CHOMPING_CLIP = 1;
  const CHOMPING_STRIP = 2;
  const CHOMPING_KEEP = 3;
  const PATTERN_NON_PRINTABLE = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x84\x86-\x9F\uFFFE\uFFFF]|[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?:[^\uD800-\uDBFF]|^)[\uDC00-\uDFFF]/;
  const PATTERN_NON_ASCII_LINE_BREAKS = /[\x85\u2028\u2029]/;
  const PATTERN_FLOW_INDICATORS = /[,\[\]{}]/;
  const PATTERN_TAG_HANDLE = /^(?:!|!!|![0-9A-Za-z-]+!)$/;
  const PATTERN_TAG_URI = /^(?:!|[^,\[\]{}])(?:%[0-9a-f]{2}|[0-9a-z\-#;/?:@&=+$,_.!~*'()\[\]])*$/i;
  function _class(obj) {
    return Object.prototype.toString.call(obj);
  }
  function isEol(c) {
    return c === 10 || c === 13;
  }
  function isWhiteSpace(c) {
    return c === 9 || c === 32;
  }
  function isWsOrEol(c) {
    return c === 9 || c === 32 || c === 10 || c === 13;
  }
  function isFlowIndicator(c) {
    return c === 44 || c === 91 || c === 93 || c === 123 || c === 125;
  }
  function fromHexCode(c) {
    if (c >= 48 && c <= 57) {
      return c - 48;
    }
    const lc = c | 32;
    if (lc >= 97 && lc <= 102) {
      return lc - 97 + 10;
    }
    return -1;
  }
  function escapedHexLen(c) {
    if (c === 120) {
      return 2;
    }
    if (c === 117) {
      return 4;
    }
    if (c === 85) {
      return 8;
    }
    return 0;
  }
  function fromDecimalCode(c) {
    if (c >= 48 && c <= 57) {
      return c - 48;
    }
    return -1;
  }
  function simpleEscapeSequence(c) {
    switch (c) {
      case 48:
        return "\0";
      case 97:
        return "\x07";
      case 98:
        return "\b";
      case 116:
        return "	";
      case 9:
        return "	";
      case 110:
        return "\n";
      case 118:
        return "\v";
      case 102:
        return "\f";
      case 114:
        return "\r";
      case 101:
        return "\x1B";
      case 32:
        return " ";
      case 34:
        return '"';
      case 47:
        return "/";
      case 92:
        return "\\";
      case 78:
        return "";
      case 95:
        return " ";
      case 76:
        return "\u2028";
      case 80:
        return "\u2029";
      default:
        return "";
    }
  }
  function charFromCodepoint(c) {
    if (c <= 65535) {
      return String.fromCharCode(c);
    }
    return String.fromCharCode(
      (c - 65536 >> 10) + 55296,
      (c - 65536 & 1023) + 56320
    );
  }
  function setProperty(object, key, value) {
    if (key === "__proto__") {
      Object.defineProperty(object, key, {
        configurable: true,
        enumerable: true,
        writable: true,
        value
      });
    } else {
      object[key] = value;
    }
  }
  const simpleEscapeCheck = new Array(256);
  const simpleEscapeMap = new Array(256);
  for (let i2 = 0; i2 < 256; i2++) {
    simpleEscapeCheck[i2] = simpleEscapeSequence(i2) ? 1 : 0;
    simpleEscapeMap[i2] = simpleEscapeSequence(i2);
  }
  function State(input, options) {
    this.input = input;
    this.filename = options["filename"] || null;
    this.schema = options["schema"] || DEFAULT_SCHEMA2;
    this.onWarning = options["onWarning"] || null;
    this.legacy = options["legacy"] || false;
    this.json = options["json"] || false;
    this.listener = options["listener"] || null;
    this.maxDepth = typeof options["maxDepth"] === "number" ? options["maxDepth"] : 100;
    this.maxTotalMergeKeys = typeof options["maxTotalMergeKeys"] === "number" ? options["maxTotalMergeKeys"] : 1e4;
    this.implicitTypes = this.schema.compiledImplicit;
    this.typeMap = this.schema.compiledTypeMap;
    this.length = input.length;
    this.position = 0;
    this.line = 0;
    this.lineStart = 0;
    this.lineIndent = 0;
    this.depth = 0;
    this.totalMergeKeys = 0;
    this.firstTabInLine = -1;
    this.documents = [];
    this.anchorMapTransactions = [];
  }
  function generateError(state, message) {
    const mark = {
      name: state.filename,
      buffer: state.input.slice(0, -1),
      // omit trailing \0
      position: state.position,
      line: state.line,
      column: state.position - state.lineStart
    };
    mark.snippet = makeSnippet(mark);
    return new YAMLException2(message, mark);
  }
  function throwError(state, message) {
    throw generateError(state, message);
  }
  function throwWarning(state, message) {
    if (state.onWarning) {
      state.onWarning.call(null, generateError(state, message));
    }
  }
  function storeAnchor(state, name, value) {
    const transactions = state.anchorMapTransactions;
    if (transactions.length !== 0) {
      const transaction = transactions[transactions.length - 1];
      if (!_hasOwnProperty.call(transaction, name)) {
        transaction[name] = {
          existed: _hasOwnProperty.call(state.anchorMap, name),
          value: state.anchorMap[name]
        };
      }
    }
    state.anchorMap[name] = value;
  }
  function beginAnchorTransaction(state) {
    state.anchorMapTransactions.push(/* @__PURE__ */ Object.create(null));
  }
  function commitAnchorTransaction(state) {
    const transaction = state.anchorMapTransactions.pop();
    const transactions = state.anchorMapTransactions;
    if (transactions.length === 0) return;
    const parent = transactions[transactions.length - 1];
    const names = Object.keys(transaction);
    for (let index = 0, length = names.length; index < length; index += 1) {
      const name = names[index];
      if (!_hasOwnProperty.call(parent, name)) {
        parent[name] = transaction[name];
      }
    }
  }
  function rollbackAnchorTransaction(state) {
    const transaction = state.anchorMapTransactions.pop();
    const names = Object.keys(transaction);
    for (let index = names.length - 1; index >= 0; index -= 1) {
      const entry = transaction[names[index]];
      if (entry.existed) {
        state.anchorMap[names[index]] = entry.value;
      } else {
        delete state.anchorMap[names[index]];
      }
    }
  }
  function snapshotState(state) {
    return {
      position: state.position,
      line: state.line,
      lineStart: state.lineStart,
      lineIndent: state.lineIndent,
      firstTabInLine: state.firstTabInLine,
      tag: state.tag,
      anchor: state.anchor,
      kind: state.kind,
      result: state.result
    };
  }
  function restoreState(state, snapshot) {
    state.position = snapshot.position;
    state.line = snapshot.line;
    state.lineStart = snapshot.lineStart;
    state.lineIndent = snapshot.lineIndent;
    state.firstTabInLine = snapshot.firstTabInLine;
    state.tag = snapshot.tag;
    state.anchor = snapshot.anchor;
    state.kind = snapshot.kind;
    state.result = snapshot.result;
  }
  const directiveHandlers = {
    YAML: function handleYamlDirective(state, name, args) {
      if (state.version !== null) {
        throwError(state, "duplication of %YAML directive");
      }
      if (args.length !== 1) {
        throwError(state, "YAML directive accepts exactly one argument");
      }
      const match = /^([0-9]+)\.([0-9]+)$/.exec(args[0]);
      if (match === null) {
        throwError(state, "ill-formed argument of the YAML directive");
      }
      const major = parseInt(match[1], 10);
      const minor = parseInt(match[2], 10);
      if (major !== 1) {
        throwError(state, "unacceptable YAML version of the document");
      }
      state.version = args[0];
      state.checkLineBreaks = minor < 2;
      if (minor !== 1 && minor !== 2) {
        throwWarning(state, "unsupported YAML version of the document");
      }
    },
    TAG: function handleTagDirective(state, name, args) {
      let prefix;
      if (args.length !== 2) {
        throwError(state, "TAG directive accepts exactly two arguments");
      }
      const handle = args[0];
      prefix = args[1];
      if (!PATTERN_TAG_HANDLE.test(handle)) {
        throwError(state, "ill-formed tag handle (first argument) of the TAG directive");
      }
      if (_hasOwnProperty.call(state.tagMap, handle)) {
        throwError(state, 'there is a previously declared suffix for "' + handle + '" tag handle');
      }
      if (!PATTERN_TAG_URI.test(prefix)) {
        throwError(state, "ill-formed tag prefix (second argument) of the TAG directive");
      }
      try {
        prefix = decodeURIComponent(prefix);
      } catch (err) {
        throwError(state, "tag prefix is malformed: " + prefix);
      }
      state.tagMap[handle] = prefix;
    }
  };
  function captureSegment(state, start, end, checkJson) {
    if (start < end) {
      const _result = state.input.slice(start, end);
      if (checkJson) {
        for (let _position = 0, _length = _result.length; _position < _length; _position += 1) {
          const _character = _result.charCodeAt(_position);
          if (!(_character === 9 || _character >= 32 && _character <= 1114111)) {
            throwError(state, "expected valid JSON character");
          }
        }
      } else if (PATTERN_NON_PRINTABLE.test(_result)) {
        throwError(state, "the stream contains non-printable characters");
      }
      state.result += _result;
    }
  }
  function chargeMergeWork(state) {
    state.totalMergeKeys++;
    if (state.maxTotalMergeKeys !== -1 && state.totalMergeKeys > state.maxTotalMergeKeys) {
      throwError(state, "merge keys exceeded maxTotalMergeKeys (" + state.maxTotalMergeKeys + ")");
    }
  }
  function mergeMappings(state, destination, source, overridableKeys) {
    if (!common2.isObject(source)) {
      throwError(state, "cannot merge mappings; the provided source object is unacceptable");
    }
    chargeMergeWork(state);
    const sourceKeys = Object.keys(source);
    for (let index = 0, quantity = sourceKeys.length; index < quantity; index += 1) {
      const key = sourceKeys[index];
      chargeMergeWork(state);
      if (!_hasOwnProperty.call(destination, key)) {
        setProperty(destination, key, source[key]);
        overridableKeys[key] = true;
      }
    }
  }
  function storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, valueNode, startLine, startLineStart, startPos) {
    if (Array.isArray(keyNode)) {
      keyNode = Array.prototype.slice.call(keyNode);
      for (let index = 0, quantity = keyNode.length; index < quantity; index += 1) {
        if (Array.isArray(keyNode[index])) {
          throwError(state, "nested arrays are not supported inside keys");
        }
        if (typeof keyNode === "object" && _class(keyNode[index]) === "[object Object]") {
          keyNode[index] = "[object Object]";
        }
      }
    }
    if (typeof keyNode === "object" && _class(keyNode) === "[object Object]") {
      keyNode = "[object Object]";
    }
    keyNode = String(keyNode);
    if (_result === null) {
      _result = {};
    }
    if (keyTag === "tag:yaml.org,2002:merge") {
      if (Array.isArray(valueNode)) {
        if (valueNode.length > 100) {
          throwError(state, "abnormal merge sequence size");
        }
        for (let index = 0, quantity = valueNode.length; index < quantity; index += 1) {
          mergeMappings(state, _result, valueNode[index], overridableKeys);
        }
      } else {
        mergeMappings(state, _result, valueNode, overridableKeys);
      }
    } else {
      if (!state.json && !_hasOwnProperty.call(overridableKeys, keyNode) && _hasOwnProperty.call(_result, keyNode)) {
        state.line = startLine || state.line;
        state.lineStart = startLineStart || state.lineStart;
        state.position = startPos || state.position;
        throwError(state, "duplicated mapping key");
      }
      setProperty(_result, keyNode, valueNode);
      delete overridableKeys[keyNode];
    }
    return _result;
  }
  function readLineBreak(state) {
    const ch = state.input.charCodeAt(state.position);
    if (ch === 10) {
      state.position++;
    } else if (ch === 13) {
      state.position++;
      if (state.input.charCodeAt(state.position) === 10) {
        state.position++;
      }
    } else {
      throwError(state, "a line break is expected");
    }
    state.line += 1;
    state.lineStart = state.position;
    state.firstTabInLine = -1;
  }
  function skipSeparationSpace(state, allowComments, checkIndent) {
    let lineBreaks = 0;
    let ch = state.input.charCodeAt(state.position);
    while (ch !== 0) {
      while (isWhiteSpace(ch)) {
        if (ch === 9 && state.firstTabInLine === -1) {
          state.firstTabInLine = state.position;
        }
        ch = state.input.charCodeAt(++state.position);
      }
      if (allowComments && ch === 35) {
        do {
          ch = state.input.charCodeAt(++state.position);
        } while (ch !== 10 && ch !== 13 && ch !== 0);
      }
      if (isEol(ch)) {
        readLineBreak(state);
        ch = state.input.charCodeAt(state.position);
        lineBreaks++;
        state.lineIndent = 0;
        while (ch === 32) {
          state.lineIndent++;
          ch = state.input.charCodeAt(++state.position);
        }
      } else {
        break;
      }
    }
    if (checkIndent !== -1 && lineBreaks !== 0 && state.lineIndent < checkIndent) {
      throwWarning(state, "deficient indentation");
    }
    return lineBreaks;
  }
  function testDocumentSeparator(state) {
    let _position = state.position;
    let ch = state.input.charCodeAt(_position);
    if ((ch === 45 || ch === 46) && ch === state.input.charCodeAt(_position + 1) && ch === state.input.charCodeAt(_position + 2)) {
      _position += 3;
      ch = state.input.charCodeAt(_position);
      if (ch === 0 || isWsOrEol(ch)) {
        return true;
      }
    }
    return false;
  }
  function writeFoldedLines(state, count) {
    if (count === 1) {
      state.result += " ";
    } else if (count > 1) {
      state.result += common2.repeat("\n", count - 1);
    }
  }
  function readPlainScalar(state, nodeIndent, withinFlowCollection) {
    let captureStart;
    let captureEnd;
    let hasPendingContent;
    let _line;
    let _lineStart;
    let _lineIndent;
    const _kind = state.kind;
    const _result = state.result;
    let ch = state.input.charCodeAt(state.position);
    if (isWsOrEol(ch) || isFlowIndicator(ch) || ch === 35 || ch === 38 || ch === 42 || ch === 33 || ch === 124 || ch === 62 || ch === 39 || ch === 34 || ch === 37 || ch === 64 || ch === 96) {
      return false;
    }
    if (ch === 63 || ch === 45) {
      const following = state.input.charCodeAt(state.position + 1);
      if (isWsOrEol(following) || withinFlowCollection && isFlowIndicator(following)) {
        return false;
      }
    }
    state.kind = "scalar";
    state.result = "";
    captureStart = captureEnd = state.position;
    hasPendingContent = false;
    while (ch !== 0) {
      if (ch === 58) {
        const following = state.input.charCodeAt(state.position + 1);
        if (isWsOrEol(following) || withinFlowCollection && isFlowIndicator(following)) {
          break;
        }
      } else if (ch === 35) {
        const preceding = state.input.charCodeAt(state.position - 1);
        if (isWsOrEol(preceding)) {
          break;
        }
      } else if (state.position === state.lineStart && testDocumentSeparator(state) || withinFlowCollection && isFlowIndicator(ch)) {
        break;
      } else if (isEol(ch)) {
        _line = state.line;
        _lineStart = state.lineStart;
        _lineIndent = state.lineIndent;
        skipSeparationSpace(state, false, -1);
        if (state.lineIndent >= nodeIndent) {
          hasPendingContent = true;
          ch = state.input.charCodeAt(state.position);
          continue;
        } else {
          state.position = captureEnd;
          state.line = _line;
          state.lineStart = _lineStart;
          state.lineIndent = _lineIndent;
          break;
        }
      }
      if (hasPendingContent) {
        captureSegment(state, captureStart, captureEnd, false);
        writeFoldedLines(state, state.line - _line);
        captureStart = captureEnd = state.position;
        hasPendingContent = false;
      }
      if (!isWhiteSpace(ch)) {
        captureEnd = state.position + 1;
      }
      ch = state.input.charCodeAt(++state.position);
    }
    captureSegment(state, captureStart, captureEnd, false);
    if (state.result) {
      return true;
    }
    state.kind = _kind;
    state.result = _result;
    return false;
  }
  function readSingleQuotedScalar(state, nodeIndent) {
    let captureStart;
    let captureEnd;
    let ch = state.input.charCodeAt(state.position);
    if (ch !== 39) {
      return false;
    }
    state.kind = "scalar";
    state.result = "";
    state.position++;
    captureStart = captureEnd = state.position;
    while ((ch = state.input.charCodeAt(state.position)) !== 0) {
      if (ch === 39) {
        captureSegment(state, captureStart, state.position, true);
        ch = state.input.charCodeAt(++state.position);
        if (ch === 39) {
          captureStart = state.position;
          state.position++;
          captureEnd = state.position;
        } else {
          return true;
        }
      } else if (isEol(ch)) {
        captureSegment(state, captureStart, captureEnd, true);
        writeFoldedLines(state, skipSeparationSpace(state, false, nodeIndent));
        captureStart = captureEnd = state.position;
      } else if (state.position === state.lineStart && testDocumentSeparator(state)) {
        throwError(state, "unexpected end of the document within a single quoted scalar");
      } else {
        state.position++;
        if (!isWhiteSpace(ch)) {
          captureEnd = state.position;
        }
      }
    }
    throwError(state, "unexpected end of the stream within a single quoted scalar");
  }
  function readDoubleQuotedScalar(state, nodeIndent) {
    let captureStart;
    let captureEnd;
    let tmp;
    let ch = state.input.charCodeAt(state.position);
    if (ch !== 34) {
      return false;
    }
    state.kind = "scalar";
    state.result = "";
    state.position++;
    captureStart = captureEnd = state.position;
    while ((ch = state.input.charCodeAt(state.position)) !== 0) {
      if (ch === 34) {
        captureSegment(state, captureStart, state.position, true);
        state.position++;
        return true;
      } else if (ch === 92) {
        captureSegment(state, captureStart, state.position, true);
        ch = state.input.charCodeAt(++state.position);
        if (isEol(ch)) {
          skipSeparationSpace(state, false, nodeIndent);
        } else if (ch < 256 && simpleEscapeCheck[ch]) {
          state.result += simpleEscapeMap[ch];
          state.position++;
        } else if ((tmp = escapedHexLen(ch)) > 0) {
          let hexLength = tmp;
          let hexResult = 0;
          for (; hexLength > 0; hexLength--) {
            ch = state.input.charCodeAt(++state.position);
            if ((tmp = fromHexCode(ch)) >= 0) {
              hexResult = (hexResult << 4) + tmp;
            } else {
              throwError(state, "expected hexadecimal character");
            }
          }
          state.result += charFromCodepoint(hexResult);
          state.position++;
        } else {
          throwError(state, "unknown escape sequence");
        }
        captureStart = captureEnd = state.position;
      } else if (isEol(ch)) {
        captureSegment(state, captureStart, captureEnd, true);
        writeFoldedLines(state, skipSeparationSpace(state, false, nodeIndent));
        captureStart = captureEnd = state.position;
      } else if (state.position === state.lineStart && testDocumentSeparator(state)) {
        throwError(state, "unexpected end of the document within a double quoted scalar");
      } else {
        state.position++;
        if (!isWhiteSpace(ch)) {
          captureEnd = state.position;
        }
      }
    }
    throwError(state, "unexpected end of the stream within a double quoted scalar");
  }
  function readFlowCollection(state, nodeIndent) {
    let readNext = true;
    let _line;
    let _lineStart;
    let _pos;
    const _tag = state.tag;
    let _result;
    const _anchor = state.anchor;
    let terminator;
    let isPair;
    let isExplicitPair;
    let isMapping;
    const overridableKeys = /* @__PURE__ */ Object.create(null);
    let keyNode;
    let keyTag;
    let valueNode;
    let ch = state.input.charCodeAt(state.position);
    if (ch === 91) {
      terminator = 93;
      isMapping = false;
      _result = [];
    } else if (ch === 123) {
      terminator = 125;
      isMapping = true;
      _result = {};
    } else {
      return false;
    }
    if (state.anchor !== null) {
      storeAnchor(state, state.anchor, _result);
    }
    ch = state.input.charCodeAt(++state.position);
    while (ch !== 0) {
      skipSeparationSpace(state, true, nodeIndent);
      ch = state.input.charCodeAt(state.position);
      if (ch === terminator) {
        state.position++;
        state.tag = _tag;
        state.anchor = _anchor;
        state.kind = isMapping ? "mapping" : "sequence";
        state.result = _result;
        return true;
      } else if (!readNext) {
        throwError(state, "missed comma between flow collection entries");
      } else if (ch === 44) {
        throwError(state, "expected the node content, but found ','");
      }
      keyTag = keyNode = valueNode = null;
      isPair = isExplicitPair = false;
      if (ch === 63) {
        const following = state.input.charCodeAt(state.position + 1);
        if (isWsOrEol(following)) {
          isPair = isExplicitPair = true;
          state.position++;
          skipSeparationSpace(state, true, nodeIndent);
        }
      }
      _line = state.line;
      _lineStart = state.lineStart;
      _pos = state.position;
      composeNode(state, nodeIndent, CONTEXT_FLOW_IN, false, true);
      keyTag = state.tag;
      keyNode = state.result;
      skipSeparationSpace(state, true, nodeIndent);
      ch = state.input.charCodeAt(state.position);
      if ((isExplicitPair || state.line === _line) && ch === 58) {
        isPair = true;
        ch = state.input.charCodeAt(++state.position);
        skipSeparationSpace(state, true, nodeIndent);
        composeNode(state, nodeIndent, CONTEXT_FLOW_IN, false, true);
        valueNode = state.result;
      }
      if (isMapping) {
        storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, valueNode, _line, _lineStart, _pos);
      } else if (isPair) {
        _result.push(storeMappingPair(state, null, overridableKeys, keyTag, keyNode, valueNode, _line, _lineStart, _pos));
      } else {
        _result.push(keyNode);
      }
      skipSeparationSpace(state, true, nodeIndent);
      ch = state.input.charCodeAt(state.position);
      if (ch === 44) {
        readNext = true;
        ch = state.input.charCodeAt(++state.position);
      } else {
        readNext = false;
      }
    }
    throwError(state, "unexpected end of the stream within a flow collection");
  }
  function readBlockScalar(state, nodeIndent) {
    let folding;
    let chomping = CHOMPING_CLIP;
    let didReadContent = false;
    let detectedIndent = false;
    let textIndent = nodeIndent;
    let emptyLines = 0;
    let atMoreIndented = false;
    let tmp;
    let ch = state.input.charCodeAt(state.position);
    if (ch === 124) {
      folding = false;
    } else if (ch === 62) {
      folding = true;
    } else {
      return false;
    }
    state.kind = "scalar";
    state.result = "";
    while (ch !== 0) {
      ch = state.input.charCodeAt(++state.position);
      if (ch === 43 || ch === 45) {
        if (CHOMPING_CLIP === chomping) {
          chomping = ch === 43 ? CHOMPING_KEEP : CHOMPING_STRIP;
        } else {
          throwError(state, "repeat of a chomping mode identifier");
        }
      } else if ((tmp = fromDecimalCode(ch)) >= 0) {
        if (tmp === 0) {
          throwError(state, "bad explicit indentation width of a block scalar; it cannot be less than one");
        } else if (!detectedIndent) {
          textIndent = nodeIndent + tmp - 1;
          detectedIndent = true;
        } else {
          throwError(state, "repeat of an indentation width identifier");
        }
      } else {
        break;
      }
    }
    if (isWhiteSpace(ch)) {
      do {
        ch = state.input.charCodeAt(++state.position);
      } while (isWhiteSpace(ch));
      if (ch === 35) {
        do {
          ch = state.input.charCodeAt(++state.position);
        } while (!isEol(ch) && ch !== 0);
      }
    }
    while (ch !== 0) {
      readLineBreak(state);
      state.lineIndent = 0;
      ch = state.input.charCodeAt(state.position);
      while ((!detectedIndent || state.lineIndent < textIndent) && ch === 32) {
        state.lineIndent++;
        ch = state.input.charCodeAt(++state.position);
      }
      if (!detectedIndent && state.lineIndent > textIndent) {
        textIndent = state.lineIndent;
      }
      if (isEol(ch)) {
        emptyLines++;
        continue;
      }
      if (!detectedIndent && textIndent === 0) {
        throwError(state, "missing indentation for block scalar");
      }
      if (state.lineIndent < textIndent) {
        if (chomping === CHOMPING_KEEP) {
          state.result += common2.repeat("\n", didReadContent ? 1 + emptyLines : emptyLines);
        } else if (chomping === CHOMPING_CLIP) {
          if (didReadContent) {
            state.result += "\n";
          }
        }
        break;
      }
      if (folding) {
        if (isWhiteSpace(ch)) {
          atMoreIndented = true;
          state.result += common2.repeat("\n", didReadContent ? 1 + emptyLines : emptyLines);
        } else if (atMoreIndented) {
          atMoreIndented = false;
          state.result += common2.repeat("\n", emptyLines + 1);
        } else if (emptyLines === 0) {
          if (didReadContent) {
            state.result += " ";
          }
        } else {
          state.result += common2.repeat("\n", emptyLines);
        }
      } else {
        state.result += common2.repeat("\n", didReadContent ? 1 + emptyLines : emptyLines);
      }
      didReadContent = true;
      detectedIndent = true;
      emptyLines = 0;
      const captureStart = state.position;
      while (!isEol(ch) && ch !== 0) {
        ch = state.input.charCodeAt(++state.position);
      }
      captureSegment(state, captureStart, state.position, false);
    }
    return true;
  }
  function readBlockSequence(state, nodeIndent) {
    const _tag = state.tag;
    const _anchor = state.anchor;
    const _result = [];
    let detected = false;
    if (state.firstTabInLine !== -1) return false;
    if (state.anchor !== null) {
      storeAnchor(state, state.anchor, _result);
    }
    let ch = state.input.charCodeAt(state.position);
    while (ch !== 0) {
      if (state.firstTabInLine !== -1) {
        state.position = state.firstTabInLine;
        throwError(state, "tab characters must not be used in indentation");
      }
      if (ch !== 45) {
        break;
      }
      const following = state.input.charCodeAt(state.position + 1);
      if (!isWsOrEol(following)) {
        break;
      }
      detected = true;
      state.position++;
      if (skipSeparationSpace(state, true, -1)) {
        if (state.lineIndent <= nodeIndent) {
          _result.push(null);
          ch = state.input.charCodeAt(state.position);
          continue;
        }
      }
      const _line = state.line;
      composeNode(state, nodeIndent, CONTEXT_BLOCK_IN, false, true);
      _result.push(state.result);
      skipSeparationSpace(state, true, -1);
      ch = state.input.charCodeAt(state.position);
      if ((state.line === _line || state.lineIndent > nodeIndent) && ch !== 0) {
        throwError(state, "bad indentation of a sequence entry");
      } else if (state.lineIndent < nodeIndent) {
        break;
      }
    }
    if (detected) {
      state.tag = _tag;
      state.anchor = _anchor;
      state.kind = "sequence";
      state.result = _result;
      return true;
    }
    return false;
  }
  function readBlockMapping(state, nodeIndent, flowIndent) {
    let allowCompact;
    let _keyLine;
    let _keyLineStart;
    let _keyPos;
    const _tag = state.tag;
    const _anchor = state.anchor;
    const _result = {};
    const overridableKeys = /* @__PURE__ */ Object.create(null);
    let keyTag = null;
    let keyNode = null;
    let valueNode = null;
    let atExplicitKey = false;
    let detected = false;
    if (state.firstTabInLine !== -1) return false;
    if (state.anchor !== null) {
      storeAnchor(state, state.anchor, _result);
    }
    let ch = state.input.charCodeAt(state.position);
    while (ch !== 0) {
      if (!atExplicitKey && state.firstTabInLine !== -1) {
        state.position = state.firstTabInLine;
        throwError(state, "tab characters must not be used in indentation");
      }
      const following = state.input.charCodeAt(state.position + 1);
      const _line = state.line;
      if ((ch === 63 || ch === 58) && isWsOrEol(following)) {
        if (ch === 63) {
          if (atExplicitKey) {
            storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, null, _keyLine, _keyLineStart, _keyPos);
            keyTag = keyNode = valueNode = null;
          }
          detected = true;
          atExplicitKey = true;
          allowCompact = true;
        } else if (atExplicitKey) {
          atExplicitKey = false;
          allowCompact = true;
        } else {
          throwError(state, "incomplete explicit mapping pair; a key node is missed; or followed by a non-tabulated empty line");
        }
        state.position += 1;
        ch = following;
      } else {
        _keyLine = state.line;
        _keyLineStart = state.lineStart;
        _keyPos = state.position;
        if (!composeNode(state, flowIndent, CONTEXT_FLOW_OUT, false, true)) {
          break;
        }
        if (state.line === _line) {
          ch = state.input.charCodeAt(state.position);
          while (isWhiteSpace(ch)) {
            ch = state.input.charCodeAt(++state.position);
          }
          if (ch === 58) {
            ch = state.input.charCodeAt(++state.position);
            if (!isWsOrEol(ch)) {
              throwError(state, "a whitespace character is expected after the key-value separator within a block mapping");
            }
            if (atExplicitKey) {
              storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, null, _keyLine, _keyLineStart, _keyPos);
              keyTag = keyNode = valueNode = null;
            }
            detected = true;
            atExplicitKey = false;
            allowCompact = false;
            keyTag = state.tag;
            keyNode = state.result;
          } else if (detected) {
            throwError(state, "can not read an implicit mapping pair; a colon is missed");
          } else {
            state.tag = _tag;
            state.anchor = _anchor;
            return true;
          }
        } else if (detected) {
          throwError(state, "can not read a block mapping entry; a multiline key may not be an implicit key");
        } else {
          state.tag = _tag;
          state.anchor = _anchor;
          return true;
        }
      }
      if (state.line === _line || state.lineIndent > nodeIndent) {
        if (atExplicitKey) {
          _keyLine = state.line;
          _keyLineStart = state.lineStart;
          _keyPos = state.position;
        }
        if (composeNode(state, nodeIndent, CONTEXT_BLOCK_OUT, true, allowCompact)) {
          if (atExplicitKey) {
            keyNode = state.result;
          } else {
            valueNode = state.result;
          }
        }
        if (!atExplicitKey) {
          storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, valueNode, _keyLine, _keyLineStart, _keyPos);
          keyTag = keyNode = valueNode = null;
        }
        skipSeparationSpace(state, true, -1);
        ch = state.input.charCodeAt(state.position);
      }
      if ((state.line === _line || state.lineIndent > nodeIndent) && ch !== 0) {
        throwError(state, "bad indentation of a mapping entry");
      } else if (state.lineIndent < nodeIndent) {
        break;
      }
    }
    if (atExplicitKey) {
      storeMappingPair(state, _result, overridableKeys, keyTag, keyNode, null, _keyLine, _keyLineStart, _keyPos);
    }
    if (detected) {
      state.tag = _tag;
      state.anchor = _anchor;
      state.kind = "mapping";
      state.result = _result;
    }
    return detected;
  }
  function readTagProperty(state) {
    let isVerbatim = false;
    let isNamed = false;
    let tagHandle;
    let tagName;
    let ch = state.input.charCodeAt(state.position);
    if (ch !== 33) return false;
    if (state.tag !== null) {
      throwError(state, "duplication of a tag property");
    }
    ch = state.input.charCodeAt(++state.position);
    if (ch === 60) {
      isVerbatim = true;
      ch = state.input.charCodeAt(++state.position);
    } else if (ch === 33) {
      isNamed = true;
      tagHandle = "!!";
      ch = state.input.charCodeAt(++state.position);
    } else {
      tagHandle = "!";
    }
    let _position = state.position;
    if (isVerbatim) {
      do {
        ch = state.input.charCodeAt(++state.position);
      } while (ch !== 0 && ch !== 62);
      if (state.position < state.length) {
        tagName = state.input.slice(_position, state.position);
        ch = state.input.charCodeAt(++state.position);
      } else {
        throwError(state, "unexpected end of the stream within a verbatim tag");
      }
    } else {
      while (ch !== 0 && !isWsOrEol(ch)) {
        if (ch === 33) {
          if (!isNamed) {
            tagHandle = state.input.slice(_position - 1, state.position + 1);
            if (!PATTERN_TAG_HANDLE.test(tagHandle)) {
              throwError(state, "named tag handle cannot contain such characters");
            }
            isNamed = true;
            _position = state.position + 1;
          } else {
            throwError(state, "tag suffix cannot contain exclamation marks");
          }
        }
        ch = state.input.charCodeAt(++state.position);
      }
      tagName = state.input.slice(_position, state.position);
      if (PATTERN_FLOW_INDICATORS.test(tagName)) {
        throwError(state, "tag suffix cannot contain flow indicator characters");
      }
    }
    if (tagName && !PATTERN_TAG_URI.test(tagName)) {
      throwError(state, "tag name cannot contain such characters: " + tagName);
    }
    try {
      tagName = decodeURIComponent(tagName);
    } catch (err) {
      throwError(state, "tag name is malformed: " + tagName);
    }
    if (isVerbatim) {
      state.tag = tagName;
    } else if (_hasOwnProperty.call(state.tagMap, tagHandle)) {
      state.tag = state.tagMap[tagHandle] + tagName;
    } else if (tagHandle === "!") {
      state.tag = "!" + tagName;
    } else if (tagHandle === "!!") {
      state.tag = "tag:yaml.org,2002:" + tagName;
    } else {
      throwError(state, 'undeclared tag handle "' + tagHandle + '"');
    }
    return true;
  }
  function readAnchorProperty(state) {
    let ch = state.input.charCodeAt(state.position);
    if (ch !== 38) return false;
    if (state.anchor !== null) {
      throwError(state, "duplication of an anchor property");
    }
    ch = state.input.charCodeAt(++state.position);
    const _position = state.position;
    while (ch !== 0 && !isWsOrEol(ch) && !isFlowIndicator(ch)) {
      ch = state.input.charCodeAt(++state.position);
    }
    if (state.position === _position) {
      throwError(state, "name of an anchor node must contain at least one character");
    }
    state.anchor = state.input.slice(_position, state.position);
    return true;
  }
  function readAlias(state) {
    let ch = state.input.charCodeAt(state.position);
    if (ch !== 42) return false;
    ch = state.input.charCodeAt(++state.position);
    const _position = state.position;
    while (ch !== 0 && !isWsOrEol(ch) && !isFlowIndicator(ch)) {
      ch = state.input.charCodeAt(++state.position);
    }
    if (state.position === _position) {
      throwError(state, "name of an alias node must contain at least one character");
    }
    const alias = state.input.slice(_position, state.position);
    if (!_hasOwnProperty.call(state.anchorMap, alias)) {
      throwError(state, 'unidentified alias "' + alias + '"');
    }
    state.result = state.anchorMap[alias];
    skipSeparationSpace(state, true, -1);
    return true;
  }
  function tryReadBlockMappingFromProperty(state, propertyStart, nodeIndent, flowIndent) {
    const fallbackState = snapshotState(state);
    beginAnchorTransaction(state);
    restoreState(state, propertyStart);
    state.tag = null;
    state.anchor = null;
    state.kind = null;
    state.result = null;
    if (readBlockMapping(state, nodeIndent, flowIndent) && state.kind === "mapping") {
      commitAnchorTransaction(state);
      return true;
    }
    rollbackAnchorTransaction(state);
    restoreState(state, fallbackState);
    return false;
  }
  function composeNode(state, parentIndent, nodeContext, allowToSeek, allowCompact) {
    let allowBlockScalars;
    let allowBlockCollections;
    let indentStatus = 1;
    let atNewLine = false;
    let hasContent = false;
    let propertyStart = null;
    let type2;
    let flowIndent;
    let blockIndent;
    if (state.depth >= state.maxDepth) {
      throwError(state, "nesting exceeded maxDepth (" + state.maxDepth + ")");
    }
    state.depth += 1;
    if (state.listener !== null) {
      state.listener("open", state);
    }
    state.tag = null;
    state.anchor = null;
    state.kind = null;
    state.result = null;
    const allowBlockStyles = allowBlockScalars = allowBlockCollections = CONTEXT_BLOCK_OUT === nodeContext || CONTEXT_BLOCK_IN === nodeContext;
    if (allowToSeek) {
      if (skipSeparationSpace(state, true, -1)) {
        atNewLine = true;
        if (state.lineIndent > parentIndent) {
          indentStatus = 1;
        } else if (state.lineIndent === parentIndent) {
          indentStatus = 0;
        } else if (state.lineIndent < parentIndent) {
          indentStatus = -1;
        }
      }
    }
    if (indentStatus === 1) {
      while (true) {
        const ch = state.input.charCodeAt(state.position);
        const propertyState = snapshotState(state);
        if (atNewLine && (ch === 33 && state.tag !== null || ch === 38 && state.anchor !== null)) {
          break;
        }
        if (!readTagProperty(state) && !readAnchorProperty(state)) {
          break;
        }
        if (propertyStart === null) {
          propertyStart = propertyState;
        }
        if (skipSeparationSpace(state, true, -1)) {
          atNewLine = true;
          allowBlockCollections = allowBlockStyles;
          if (state.lineIndent > parentIndent) {
            indentStatus = 1;
          } else if (state.lineIndent === parentIndent) {
            indentStatus = 0;
          } else if (state.lineIndent < parentIndent) {
            indentStatus = -1;
          }
        } else {
          allowBlockCollections = false;
        }
      }
    }
    if (allowBlockCollections) {
      allowBlockCollections = atNewLine || allowCompact;
    }
    if (indentStatus === 1 || CONTEXT_BLOCK_OUT === nodeContext) {
      if (CONTEXT_FLOW_IN === nodeContext || CONTEXT_FLOW_OUT === nodeContext) {
        flowIndent = parentIndent;
      } else {
        flowIndent = parentIndent + 1;
      }
      blockIndent = state.position - state.lineStart;
      if (indentStatus === 1) {
        if (allowBlockCollections && (readBlockSequence(state, blockIndent) || readBlockMapping(state, blockIndent, flowIndent)) || readFlowCollection(state, flowIndent)) {
          hasContent = true;
        } else {
          const ch = state.input.charCodeAt(state.position);
          if (propertyStart !== null && allowBlockStyles && !allowBlockCollections && ch !== 124 && ch !== 62 && tryReadBlockMappingFromProperty(
            state,
            propertyStart,
            propertyStart.position - propertyStart.lineStart,
            flowIndent
          )) {
            hasContent = true;
          } else if (allowBlockScalars && readBlockScalar(state, flowIndent) || readSingleQuotedScalar(state, flowIndent) || readDoubleQuotedScalar(state, flowIndent)) {
            hasContent = true;
          } else if (readAlias(state)) {
            hasContent = true;
            if (state.tag !== null || state.anchor !== null) {
              throwError(state, "alias node should not have any properties");
            }
          } else if (readPlainScalar(state, flowIndent, CONTEXT_FLOW_IN === nodeContext)) {
            hasContent = true;
            if (state.tag === null) {
              state.tag = "?";
            }
          }
          if (state.anchor !== null) {
            storeAnchor(state, state.anchor, state.result);
          }
        }
      } else if (indentStatus === 0) {
        hasContent = allowBlockCollections && readBlockSequence(state, blockIndent);
      }
    }
    if (state.tag === null) {
      if (state.anchor !== null) {
        storeAnchor(state, state.anchor, state.result);
      }
    } else if (state.tag === "?") {
      if (state.result !== null && state.kind !== "scalar") {
        throwError(state, 'unacceptable node kind for !<?> tag; it should be "scalar", not "' + state.kind + '"');
      }
      for (let typeIndex = 0, typeQuantity = state.implicitTypes.length; typeIndex < typeQuantity; typeIndex += 1) {
        type2 = state.implicitTypes[typeIndex];
        if (type2.resolve(state.result)) {
          state.result = type2.construct(state.result);
          state.tag = type2.tag;
          if (state.anchor !== null) {
            storeAnchor(state, state.anchor, state.result);
          }
          break;
        }
      }
    } else if (state.tag !== "!") {
      if (_hasOwnProperty.call(state.typeMap[state.kind || "fallback"], state.tag)) {
        type2 = state.typeMap[state.kind || "fallback"][state.tag];
      } else {
        type2 = null;
        const typeList = state.typeMap.multi[state.kind || "fallback"];
        for (let typeIndex = 0, typeQuantity = typeList.length; typeIndex < typeQuantity; typeIndex += 1) {
          if (state.tag.slice(0, typeList[typeIndex].tag.length) === typeList[typeIndex].tag) {
            type2 = typeList[typeIndex];
            break;
          }
        }
      }
      if (!type2) {
        throwError(state, "unknown tag !<" + state.tag + ">");
      }
      if (state.result !== null && type2.kind !== state.kind) {
        throwError(state, "unacceptable node kind for !<" + state.tag + '> tag; it should be "' + type2.kind + '", not "' + state.kind + '"');
      }
      if (!type2.resolve(state.result, state.tag)) {
        throwError(state, "cannot resolve a node with !<" + state.tag + "> explicit tag");
      } else {
        state.result = type2.construct(state.result, state.tag);
        if (state.anchor !== null) {
          storeAnchor(state, state.anchor, state.result);
        }
      }
    }
    if (state.listener !== null) {
      state.listener("close", state);
    }
    state.depth -= 1;
    return state.tag !== null || state.anchor !== null || hasContent;
  }
  function readDocument(state) {
    const documentStart = state.position;
    let hasDirectives = false;
    let ch;
    state.version = null;
    state.checkLineBreaks = state.legacy;
    state.tagMap = /* @__PURE__ */ Object.create(null);
    state.anchorMap = /* @__PURE__ */ Object.create(null);
    while ((ch = state.input.charCodeAt(state.position)) !== 0) {
      skipSeparationSpace(state, true, -1);
      ch = state.input.charCodeAt(state.position);
      if (state.lineIndent > 0 || ch !== 37) {
        break;
      }
      hasDirectives = true;
      ch = state.input.charCodeAt(++state.position);
      let _position = state.position;
      while (ch !== 0 && !isWsOrEol(ch)) {
        ch = state.input.charCodeAt(++state.position);
      }
      const directiveName = state.input.slice(_position, state.position);
      const directiveArgs = [];
      if (directiveName.length < 1) {
        throwError(state, "directive name must not be less than one character in length");
      }
      while (ch !== 0) {
        while (isWhiteSpace(ch)) {
          ch = state.input.charCodeAt(++state.position);
        }
        if (ch === 35) {
          do {
            ch = state.input.charCodeAt(++state.position);
          } while (ch !== 0 && !isEol(ch));
          break;
        }
        if (isEol(ch)) break;
        _position = state.position;
        while (ch !== 0 && !isWsOrEol(ch)) {
          ch = state.input.charCodeAt(++state.position);
        }
        directiveArgs.push(state.input.slice(_position, state.position));
      }
      if (ch !== 0) readLineBreak(state);
      if (_hasOwnProperty.call(directiveHandlers, directiveName)) {
        directiveHandlers[directiveName](state, directiveName, directiveArgs);
      } else {
        throwWarning(state, 'unknown document directive "' + directiveName + '"');
      }
    }
    skipSeparationSpace(state, true, -1);
    if (state.lineIndent === 0 && state.input.charCodeAt(state.position) === 45 && state.input.charCodeAt(state.position + 1) === 45 && state.input.charCodeAt(state.position + 2) === 45) {
      state.position += 3;
      skipSeparationSpace(state, true, -1);
    } else if (hasDirectives) {
      throwError(state, "directives end mark is expected");
    }
    composeNode(state, state.lineIndent - 1, CONTEXT_BLOCK_OUT, false, true);
    skipSeparationSpace(state, true, -1);
    if (state.checkLineBreaks && PATTERN_NON_ASCII_LINE_BREAKS.test(state.input.slice(documentStart, state.position))) {
      throwWarning(state, "non-ASCII line breaks are interpreted as content");
    }
    state.documents.push(state.result);
    if (state.position === state.lineStart && testDocumentSeparator(state)) {
      if (state.input.charCodeAt(state.position) === 46) {
        state.position += 3;
        skipSeparationSpace(state, true, -1);
      }
      return;
    }
    if (state.position < state.length - 1) {
      throwError(state, "end of the stream or a document separator is expected");
    }
  }
  function loadDocuments(input, options) {
    input = String(input);
    options = options || {};
    if (input.length !== 0) {
      if (input.charCodeAt(input.length - 1) !== 10 && input.charCodeAt(input.length - 1) !== 13) {
        input += "\n";
      }
      if (input.charCodeAt(0) === 65279) {
        input = input.slice(1);
      }
    }
    const state = new State(input, options);
    const nullpos = input.indexOf("\0");
    if (nullpos !== -1) {
      state.position = nullpos;
      throwError(state, "null byte is not allowed in input");
    }
    state.input += "\0";
    while (state.input.charCodeAt(state.position) === 32) {
      state.lineIndent += 1;
      state.position += 1;
    }
    while (state.position < state.length - 1) {
      readDocument(state);
    }
    return state.documents;
  }
  function loadAll2(input, iterator, options) {
    if (iterator !== null && typeof iterator === "object" && typeof options === "undefined") {
      options = iterator;
      iterator = null;
    }
    const documents = loadDocuments(input, options);
    if (typeof iterator !== "function") {
      return documents;
    }
    for (let index = 0, length = documents.length; index < length; index += 1) {
      iterator(documents[index]);
    }
  }
  function load2(input, options) {
    const documents = loadDocuments(input, options);
    if (documents.length === 0) {
      return void 0;
    } else if (documents.length === 1) {
      return documents[0];
    }
    throw new YAMLException2("expected a single document in the stream, but found more");
  }
  loader.loadAll = loadAll2;
  loader.load = load2;
  return loader;
}
function requireDumper() {
  if (hasRequiredDumper) return dumper;
  hasRequiredDumper = 1;
  const common2 = requireCommon();
  const YAMLException2 = requireException();
  const DEFAULT_SCHEMA2 = require_default();
  const _toString = Object.prototype.toString;
  const _hasOwnProperty = Object.prototype.hasOwnProperty;
  const CHAR_BOM = 65279;
  const CHAR_TAB = 9;
  const CHAR_LINE_FEED = 10;
  const CHAR_CARRIAGE_RETURN = 13;
  const CHAR_SPACE = 32;
  const CHAR_EXCLAMATION = 33;
  const CHAR_DOUBLE_QUOTE = 34;
  const CHAR_SHARP = 35;
  const CHAR_PERCENT = 37;
  const CHAR_AMPERSAND = 38;
  const CHAR_SINGLE_QUOTE = 39;
  const CHAR_ASTERISK = 42;
  const CHAR_COMMA = 44;
  const CHAR_MINUS = 45;
  const CHAR_COLON = 58;
  const CHAR_EQUALS = 61;
  const CHAR_GREATER_THAN = 62;
  const CHAR_QUESTION = 63;
  const CHAR_COMMERCIAL_AT = 64;
  const CHAR_LEFT_SQUARE_BRACKET = 91;
  const CHAR_RIGHT_SQUARE_BRACKET = 93;
  const CHAR_GRAVE_ACCENT = 96;
  const CHAR_LEFT_CURLY_BRACKET = 123;
  const CHAR_VERTICAL_LINE = 124;
  const CHAR_RIGHT_CURLY_BRACKET = 125;
  const ESCAPE_SEQUENCES = {};
  ESCAPE_SEQUENCES[0] = "\\0";
  ESCAPE_SEQUENCES[7] = "\\a";
  ESCAPE_SEQUENCES[8] = "\\b";
  ESCAPE_SEQUENCES[9] = "\\t";
  ESCAPE_SEQUENCES[10] = "\\n";
  ESCAPE_SEQUENCES[11] = "\\v";
  ESCAPE_SEQUENCES[12] = "\\f";
  ESCAPE_SEQUENCES[13] = "\\r";
  ESCAPE_SEQUENCES[27] = "\\e";
  ESCAPE_SEQUENCES[34] = '\\"';
  ESCAPE_SEQUENCES[92] = "\\\\";
  ESCAPE_SEQUENCES[133] = "\\N";
  ESCAPE_SEQUENCES[160] = "\\_";
  ESCAPE_SEQUENCES[8232] = "\\L";
  ESCAPE_SEQUENCES[8233] = "\\P";
  const DEPRECATED_BOOLEANS_SYNTAX = [
    "y",
    "Y",
    "yes",
    "Yes",
    "YES",
    "on",
    "On",
    "ON",
    "n",
    "N",
    "no",
    "No",
    "NO",
    "off",
    "Off",
    "OFF"
  ];
  const DEPRECATED_BASE60_SYNTAX = /^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$/;
  function compileStyleMap(schema22, map2) {
    if (map2 === null) return {};
    const result = {};
    const keys = Object.keys(map2);
    for (let index = 0, length = keys.length; index < length; index += 1) {
      let tag = keys[index];
      let style = String(map2[tag]);
      if (tag.slice(0, 2) === "!!") {
        tag = "tag:yaml.org,2002:" + tag.slice(2);
      }
      const type2 = schema22.compiledTypeMap["fallback"][tag];
      if (type2 && _hasOwnProperty.call(type2.styleAliases, style)) {
        style = type2.styleAliases[style];
      }
      result[tag] = style;
    }
    return result;
  }
  function encodeHex(character) {
    let handle;
    let length;
    const string = character.toString(16).toUpperCase();
    if (character <= 255) {
      handle = "x";
      length = 2;
    } else if (character <= 65535) {
      handle = "u";
      length = 4;
    } else if (character <= 4294967295) {
      handle = "U";
      length = 8;
    } else {
      throw new YAMLException2("code point within a string may not be greater than 0xFFFFFFFF");
    }
    return "\\" + handle + common2.repeat("0", length - string.length) + string;
  }
  const QUOTING_TYPE_SINGLE = 1;
  const QUOTING_TYPE_DOUBLE = 2;
  function State(options) {
    this.schema = options["schema"] || DEFAULT_SCHEMA2;
    this.indent = Math.max(1, options["indent"] || 2);
    this.noArrayIndent = options["noArrayIndent"] || false;
    this.skipInvalid = options["skipInvalid"] || false;
    this.flowLevel = common2.isNothing(options["flowLevel"]) ? -1 : options["flowLevel"];
    this.styleMap = compileStyleMap(this.schema, options["styles"] || null);
    this.sortKeys = options["sortKeys"] || false;
    this.lineWidth = options["lineWidth"] || 80;
    this.noRefs = options["noRefs"] || false;
    this.noCompatMode = options["noCompatMode"] || false;
    this.condenseFlow = options["condenseFlow"] || false;
    this.quotingType = options["quotingType"] === '"' ? QUOTING_TYPE_DOUBLE : QUOTING_TYPE_SINGLE;
    this.forceQuotes = options["forceQuotes"] || false;
    this.replacer = typeof options["replacer"] === "function" ? options["replacer"] : null;
    this.implicitTypes = this.schema.compiledImplicit;
    this.explicitTypes = this.schema.compiledExplicit;
    this.tag = null;
    this.result = "";
    this.duplicates = [];
    this.usedDuplicates = null;
  }
  function indentString(string, spaces) {
    const ind = common2.repeat(" ", spaces);
    let position = 0;
    let result = "";
    const length = string.length;
    while (position < length) {
      let line;
      const next = string.indexOf("\n", position);
      if (next === -1) {
        line = string.slice(position);
        position = length;
      } else {
        line = string.slice(position, next + 1);
        position = next + 1;
      }
      if (line.length && line !== "\n") result += ind;
      result += line;
    }
    return result;
  }
  function generateNextLine(state, level) {
    return "\n" + common2.repeat(" ", state.indent * level);
  }
  function testImplicitResolving(state, str2) {
    for (let index = 0, length = state.implicitTypes.length; index < length; index += 1) {
      const type2 = state.implicitTypes[index];
      if (type2.resolve(str2)) {
        return true;
      }
    }
    return false;
  }
  function isWhitespace(c) {
    return c === CHAR_SPACE || c === CHAR_TAB;
  }
  function isPrintable(c) {
    return c >= 32 && c <= 126 || c >= 161 && c <= 55295 && c !== 8232 && c !== 8233 || c >= 57344 && c <= 65533 && c !== CHAR_BOM || c >= 65536 && c <= 1114111;
  }
  function isNsCharOrWhitespace(c) {
    return isPrintable(c) && c !== CHAR_BOM && // - b-char
    c !== CHAR_CARRIAGE_RETURN && c !== CHAR_LINE_FEED;
  }
  function isPlainSafe(c, prev, inblock) {
    const cIsNsCharOrWhitespace = isNsCharOrWhitespace(c);
    const cIsNsChar = cIsNsCharOrWhitespace && !isWhitespace(c);
    return (
      // ns-plain-safe
      (inblock ? cIsNsCharOrWhitespace : cIsNsCharOrWhitespace && // - c-flow-indicator
      c !== CHAR_COMMA && c !== CHAR_LEFT_SQUARE_BRACKET && c !== CHAR_RIGHT_SQUARE_BRACKET && c !== CHAR_LEFT_CURLY_BRACKET && c !== CHAR_RIGHT_CURLY_BRACKET) && // ns-plain-char
      c !== CHAR_SHARP && // false on '#'
      !(prev === CHAR_COLON && !cIsNsChar) || // false on ': '
      isNsCharOrWhitespace(prev) && !isWhitespace(prev) && c === CHAR_SHARP || // change to true on '[^ ]#'
      prev === CHAR_COLON && cIsNsChar
    );
  }
  function isPlainSafeFirst(c) {
    return isPrintable(c) && c !== CHAR_BOM && !isWhitespace(c) && // - s-white
    // - (c-indicator ::=
    // “-” | “?” | “:” | “,” | “[” | “]” | “{” | “}”
    c !== CHAR_MINUS && c !== CHAR_QUESTION && c !== CHAR_COLON && c !== CHAR_COMMA && c !== CHAR_LEFT_SQUARE_BRACKET && c !== CHAR_RIGHT_SQUARE_BRACKET && c !== CHAR_LEFT_CURLY_BRACKET && c !== CHAR_RIGHT_CURLY_BRACKET && // | “#” | “&” | “*” | “!” | “|” | “=” | “>” | “'” | “"”
    c !== CHAR_SHARP && c !== CHAR_AMPERSAND && c !== CHAR_ASTERISK && c !== CHAR_EXCLAMATION && c !== CHAR_VERTICAL_LINE && c !== CHAR_EQUALS && c !== CHAR_GREATER_THAN && c !== CHAR_SINGLE_QUOTE && c !== CHAR_DOUBLE_QUOTE && // | “%” | “@” | “`”)
    c !== CHAR_PERCENT && c !== CHAR_COMMERCIAL_AT && c !== CHAR_GRAVE_ACCENT;
  }
  function isPlainSafeLast(c) {
    return !isWhitespace(c) && c !== CHAR_COLON;
  }
  function codePointAt(string, pos) {
    const first = string.charCodeAt(pos);
    let second;
    if (first >= 55296 && first <= 56319 && pos + 1 < string.length) {
      second = string.charCodeAt(pos + 1);
      if (second >= 56320 && second <= 57343) {
        return (first - 55296) * 1024 + second - 56320 + 65536;
      }
    }
    return first;
  }
  function needIndentIndicator(string) {
    const leadingSpaceRe = /^\n* /;
    return leadingSpaceRe.test(string);
  }
  const STYLE_PLAIN = 1;
  const STYLE_SINGLE = 2;
  const STYLE_LITERAL = 3;
  const STYLE_FOLDED = 4;
  const STYLE_DOUBLE = 5;
  function chooseScalarStyle(string, singleLineOnly, indentPerLevel, lineWidth, testAmbiguousType, quotingType, forceQuotes, inblock) {
    let i2;
    let char = 0;
    let prevChar = null;
    let hasLineBreak = false;
    let hasFoldableLine = false;
    const shouldTrackWidth = lineWidth !== -1;
    let previousLineBreak = -1;
    let plain2 = isPlainSafeFirst(codePointAt(string, 0)) && isPlainSafeLast(codePointAt(string, string.length - 1));
    if (singleLineOnly || forceQuotes) {
      for (i2 = 0; i2 < string.length; char >= 65536 ? i2 += 2 : i2++) {
        char = codePointAt(string, i2);
        if (!isPrintable(char)) {
          return STYLE_DOUBLE;
        }
        plain2 = plain2 && isPlainSafe(char, prevChar, inblock);
        prevChar = char;
      }
    } else {
      for (i2 = 0; i2 < string.length; char >= 65536 ? i2 += 2 : i2++) {
        char = codePointAt(string, i2);
        if (char === CHAR_LINE_FEED) {
          hasLineBreak = true;
          if (shouldTrackWidth) {
            hasFoldableLine = hasFoldableLine || // Foldable line = too long, and not more-indented.
            i2 - previousLineBreak - 1 > lineWidth && string[previousLineBreak + 1] !== " ";
            previousLineBreak = i2;
          }
        } else if (!isPrintable(char)) {
          return STYLE_DOUBLE;
        }
        plain2 = plain2 && isPlainSafe(char, prevChar, inblock);
        prevChar = char;
      }
      hasFoldableLine = hasFoldableLine || shouldTrackWidth && (i2 - previousLineBreak - 1 > lineWidth && string[previousLineBreak + 1] !== " ");
    }
    if (!hasLineBreak && !hasFoldableLine) {
      if (plain2 && !forceQuotes && !testAmbiguousType(string)) {
        return STYLE_PLAIN;
      }
      return quotingType === QUOTING_TYPE_DOUBLE ? STYLE_DOUBLE : STYLE_SINGLE;
    }
    if (indentPerLevel > 9 && needIndentIndicator(string)) {
      return STYLE_DOUBLE;
    }
    if (!forceQuotes) {
      return hasFoldableLine ? STYLE_FOLDED : STYLE_LITERAL;
    }
    return quotingType === QUOTING_TYPE_DOUBLE ? STYLE_DOUBLE : STYLE_SINGLE;
  }
  function writeScalar(state, string, level, iskey, inblock) {
    state.dump = (function() {
      if (string.length === 0) {
        return state.quotingType === QUOTING_TYPE_DOUBLE ? '""' : "''";
      }
      if (!state.noCompatMode) {
        if (DEPRECATED_BOOLEANS_SYNTAX.indexOf(string) !== -1 || DEPRECATED_BASE60_SYNTAX.test(string)) {
          return state.quotingType === QUOTING_TYPE_DOUBLE ? '"' + string + '"' : "'" + string + "'";
        }
      }
      const indent = state.indent * Math.max(1, level);
      const lineWidth = state.lineWidth === -1 ? -1 : Math.max(Math.min(state.lineWidth, 40), state.lineWidth - indent);
      const singleLineOnly = iskey || // No block styles in flow mode.
      state.flowLevel > -1 && level >= state.flowLevel;
      function testAmbiguity(string2) {
        return testImplicitResolving(state, string2);
      }
      switch (chooseScalarStyle(
        string,
        singleLineOnly,
        state.indent,
        lineWidth,
        testAmbiguity,
        state.quotingType,
        state.forceQuotes && !iskey,
        inblock
      )) {
        case STYLE_PLAIN:
          return string;
        case STYLE_SINGLE:
          return "'" + string.replace(/'/g, "''") + "'";
        case STYLE_LITERAL:
          return "|" + blockHeader(string, state.indent) + dropEndingNewline(indentString(string, indent));
        case STYLE_FOLDED:
          return ">" + blockHeader(string, state.indent) + dropEndingNewline(indentString(foldString(string, lineWidth), indent));
        case STYLE_DOUBLE:
          return '"' + escapeString(string) + '"';
        default:
          throw new YAMLException2("impossible error: invalid scalar style");
      }
    })();
  }
  function blockHeader(string, indentPerLevel) {
    const indentIndicator = needIndentIndicator(string) ? String(indentPerLevel) : "";
    const clip = string[string.length - 1] === "\n";
    const keep = clip && (string[string.length - 2] === "\n" || string === "\n");
    const chomp = keep ? "+" : clip ? "" : "-";
    return indentIndicator + chomp + "\n";
  }
  function dropEndingNewline(string) {
    return string[string.length - 1] === "\n" ? string.slice(0, -1) : string;
  }
  function foldString(string, width) {
    const lineRe = /(\n+)([^\n]*)/g;
    let result = (function() {
      let nextLF = string.indexOf("\n");
      nextLF = nextLF !== -1 ? nextLF : string.length;
      lineRe.lastIndex = nextLF;
      return foldLine(string.slice(0, nextLF), width);
    })();
    let prevMoreIndented = string[0] === "\n" || string[0] === " ";
    let moreIndented;
    let match;
    while (match = lineRe.exec(string)) {
      const prefix = match[1];
      const line = match[2];
      moreIndented = line[0] === " ";
      result += prefix + (!prevMoreIndented && !moreIndented && line !== "" ? "\n" : "") + foldLine(line, width);
      prevMoreIndented = moreIndented;
    }
    return result;
  }
  function foldLine(line, width) {
    if (line === "" || line[0] === " ") return line;
    const breakRe = / [^ ]/g;
    let match;
    let start = 0;
    let end;
    let curr = 0;
    let next = 0;
    let result = "";
    while (match = breakRe.exec(line)) {
      next = match.index;
      if (next - start > width) {
        end = curr > start ? curr : next;
        result += "\n" + line.slice(start, end);
        start = end + 1;
      }
      curr = next;
    }
    result += "\n";
    if (line.length - start > width && curr > start) {
      result += line.slice(start, curr) + "\n" + line.slice(curr + 1);
    } else {
      result += line.slice(start);
    }
    return result.slice(1);
  }
  function escapeString(string) {
    let result = "";
    let char = 0;
    for (let i2 = 0; i2 < string.length; char >= 65536 ? i2 += 2 : i2++) {
      char = codePointAt(string, i2);
      const escapeSeq = ESCAPE_SEQUENCES[char];
      if (!escapeSeq && isPrintable(char)) {
        result += string[i2];
        if (char >= 65536) result += string[i2 + 1];
      } else {
        result += escapeSeq || encodeHex(char);
      }
    }
    return result;
  }
  function writeFlowSequence(state, level, object) {
    let _result = "";
    const _tag = state.tag;
    for (let index = 0, length = object.length; index < length; index += 1) {
      let value = object[index];
      if (state.replacer) {
        value = state.replacer.call(object, String(index), value);
      }
      if (writeNode(state, level, value, false, false) || typeof value === "undefined" && writeNode(state, level, null, false, false)) {
        if (_result !== "") _result += "," + (!state.condenseFlow ? " " : "");
        _result += state.dump;
      }
    }
    state.tag = _tag;
    state.dump = "[" + _result + "]";
  }
  function writeBlockSequence(state, level, object, compact) {
    let _result = "";
    const _tag = state.tag;
    for (let index = 0, length = object.length; index < length; index += 1) {
      let value = object[index];
      if (state.replacer) {
        value = state.replacer.call(object, String(index), value);
      }
      if (writeNode(state, level + 1, value, true, true, false, true) || typeof value === "undefined" && writeNode(state, level + 1, null, true, true, false, true)) {
        if (!compact || _result !== "") {
          _result += generateNextLine(state, level);
        }
        if (state.dump && CHAR_LINE_FEED === state.dump.charCodeAt(0)) {
          _result += "-";
        } else {
          _result += "- ";
        }
        _result += state.dump;
      }
    }
    state.tag = _tag;
    state.dump = _result || "[]";
  }
  function writeFlowMapping(state, level, object) {
    let _result = "";
    const _tag = state.tag;
    const objectKeyList = Object.keys(object);
    for (let index = 0, length = objectKeyList.length; index < length; index += 1) {
      let pairBuffer = "";
      if (_result !== "") pairBuffer += ", ";
      if (state.condenseFlow) pairBuffer += '"';
      const objectKey = objectKeyList[index];
      let objectValue = object[objectKey];
      if (state.replacer) {
        objectValue = state.replacer.call(object, objectKey, objectValue);
      }
      if (!writeNode(state, level, objectKey, false, false)) {
        continue;
      }
      if (state.dump.length > 1024) pairBuffer += "? ";
      pairBuffer += state.dump + (state.condenseFlow ? '"' : "") + ":" + (state.condenseFlow ? "" : " ");
      if (!writeNode(state, level, objectValue, false, false)) {
        continue;
      }
      pairBuffer += state.dump;
      _result += pairBuffer;
    }
    state.tag = _tag;
    state.dump = "{" + _result + "}";
  }
  function writeBlockMapping(state, level, object, compact) {
    let _result = "";
    const _tag = state.tag;
    const objectKeyList = Object.keys(object);
    if (state.sortKeys === true) {
      objectKeyList.sort();
    } else if (typeof state.sortKeys === "function") {
      objectKeyList.sort(state.sortKeys);
    } else if (state.sortKeys) {
      throw new YAMLException2("sortKeys must be a boolean or a function");
    }
    for (let index = 0, length = objectKeyList.length; index < length; index += 1) {
      let pairBuffer = "";
      if (!compact || _result !== "") {
        pairBuffer += generateNextLine(state, level);
      }
      const objectKey = objectKeyList[index];
      let objectValue = object[objectKey];
      if (state.replacer) {
        objectValue = state.replacer.call(object, objectKey, objectValue);
      }
      if (!writeNode(state, level + 1, objectKey, true, true, true)) {
        continue;
      }
      const explicitPair = state.tag !== null && state.tag !== "?" || state.dump && state.dump.length > 1024;
      if (explicitPair) {
        if (state.dump && CHAR_LINE_FEED === state.dump.charCodeAt(0)) {
          pairBuffer += "?";
        } else {
          pairBuffer += "? ";
        }
      }
      pairBuffer += state.dump;
      if (explicitPair) {
        pairBuffer += generateNextLine(state, level);
      }
      if (!writeNode(state, level + 1, objectValue, true, explicitPair)) {
        continue;
      }
      if (state.dump && CHAR_LINE_FEED === state.dump.charCodeAt(0)) {
        pairBuffer += ":";
      } else {
        pairBuffer += ": ";
      }
      pairBuffer += state.dump;
      _result += pairBuffer;
    }
    state.tag = _tag;
    state.dump = _result || "{}";
  }
  function detectType(state, object, explicit) {
    const typeList = explicit ? state.explicitTypes : state.implicitTypes;
    for (let index = 0, length = typeList.length; index < length; index += 1) {
      const type2 = typeList[index];
      if ((type2.instanceOf || type2.predicate) && (!type2.instanceOf || typeof object === "object" && object instanceof type2.instanceOf) && (!type2.predicate || type2.predicate(object))) {
        if (explicit) {
          if (type2.multi && type2.representName) {
            state.tag = type2.representName(object);
          } else {
            state.tag = type2.tag;
          }
        } else {
          state.tag = "?";
        }
        if (type2.represent) {
          const style = state.styleMap[type2.tag] || type2.defaultStyle;
          let _result;
          if (_toString.call(type2.represent) === "[object Function]") {
            _result = type2.represent(object, style);
          } else if (_hasOwnProperty.call(type2.represent, style)) {
            _result = type2.represent[style](object, style);
          } else {
            throw new YAMLException2("!<" + type2.tag + '> tag resolver accepts not "' + style + '" style');
          }
          state.dump = _result;
        }
        return true;
      }
    }
    return false;
  }
  function writeNode(state, level, object, block, compact, iskey, isblockseq) {
    state.tag = null;
    state.dump = object;
    if (!detectType(state, object, false)) {
      detectType(state, object, true);
    }
    const type2 = _toString.call(state.dump);
    const inblock = block;
    if (block) {
      block = state.flowLevel < 0 || state.flowLevel > level;
    }
    const objectOrArray = type2 === "[object Object]" || type2 === "[object Array]";
    let duplicateIndex;
    let duplicate;
    if (objectOrArray) {
      duplicateIndex = state.duplicates.indexOf(object);
      duplicate = duplicateIndex !== -1;
    }
    if (state.tag !== null && state.tag !== "?" || duplicate || state.indent !== 2 && level > 0) {
      compact = false;
    }
    if (duplicate && state.usedDuplicates[duplicateIndex]) {
      state.dump = "*ref_" + duplicateIndex;
    } else {
      if (objectOrArray && duplicate && !state.usedDuplicates[duplicateIndex]) {
        state.usedDuplicates[duplicateIndex] = true;
      }
      if (type2 === "[object Object]") {
        if (block && Object.keys(state.dump).length !== 0) {
          writeBlockMapping(state, level, state.dump, compact);
          if (duplicate) {
            state.dump = "&ref_" + duplicateIndex + state.dump;
          }
        } else {
          writeFlowMapping(state, level, state.dump);
          if (duplicate) {
            state.dump = "&ref_" + duplicateIndex + " " + state.dump;
          }
        }
      } else if (type2 === "[object Array]") {
        if (block && state.dump.length !== 0) {
          if (state.noArrayIndent && !isblockseq && level > 0) {
            writeBlockSequence(state, level - 1, state.dump, compact);
          } else {
            writeBlockSequence(state, level, state.dump, compact);
          }
          if (duplicate) {
            state.dump = "&ref_" + duplicateIndex + state.dump;
          }
        } else {
          writeFlowSequence(state, level, state.dump);
          if (duplicate) {
            state.dump = "&ref_" + duplicateIndex + " " + state.dump;
          }
        }
      } else if (type2 === "[object String]") {
        if (state.tag !== "?") {
          writeScalar(state, state.dump, level, iskey, inblock);
        }
      } else if (type2 === "[object Undefined]") {
        return false;
      } else {
        if (state.skipInvalid) return false;
        throw new YAMLException2("unacceptable kind of an object to dump " + type2);
      }
      if (state.tag !== null && state.tag !== "?") {
        let tagStr = encodeURI(
          state.tag[0] === "!" ? state.tag.slice(1) : state.tag
        ).replace(/!/g, "%21");
        if (state.tag[0] === "!") {
          tagStr = "!" + tagStr;
        } else if (tagStr.slice(0, 18) === "tag:yaml.org,2002:") {
          tagStr = "!!" + tagStr.slice(18);
        } else {
          tagStr = "!<" + tagStr + ">";
        }
        state.dump = tagStr + " " + state.dump;
      }
    }
    return true;
  }
  function getDuplicateReferences(object, state) {
    const objects = [];
    const duplicatesIndexes = [];
    inspectNode(object, objects, duplicatesIndexes);
    const length = duplicatesIndexes.length;
    for (let index = 0; index < length; index += 1) {
      state.duplicates.push(objects[duplicatesIndexes[index]]);
    }
    state.usedDuplicates = new Array(length);
  }
  function inspectNode(object, objects, duplicatesIndexes) {
    if (object !== null && typeof object === "object") {
      const index = objects.indexOf(object);
      if (index !== -1) {
        if (duplicatesIndexes.indexOf(index) === -1) {
          duplicatesIndexes.push(index);
        }
      } else {
        objects.push(object);
        if (Array.isArray(object)) {
          for (let i2 = 0, length = object.length; i2 < length; i2 += 1) {
            inspectNode(object[i2], objects, duplicatesIndexes);
          }
        } else {
          const objectKeyList = Object.keys(object);
          for (let i2 = 0, length = objectKeyList.length; i2 < length; i2 += 1) {
            inspectNode(object[objectKeyList[i2]], objects, duplicatesIndexes);
          }
        }
      }
    }
  }
  function dump2(input, options) {
    options = options || {};
    const state = new State(options);
    if (!state.noRefs) getDuplicateReferences(input, state);
    let value = input;
    if (state.replacer) {
      value = state.replacer.call({ "": value }, "", value);
    }
    if (writeNode(state, 0, value, true, true)) return state.dump + "\n";
    return "";
  }
  dumper.dump = dump2;
  return dumper;
}
function requireJsYaml() {
  if (hasRequiredJsYaml) return jsYaml;
  hasRequiredJsYaml = 1;
  const loader2 = requireLoader();
  const dumper2 = requireDumper();
  function renamed(from, to) {
    return function() {
      throw new Error("Function yaml." + from + " is removed in js-yaml 4. Use yaml." + to + " instead, which is now safe by default.");
    };
  }
  jsYaml.Type = requireType();
  jsYaml.Schema = requireSchema();
  jsYaml.FAILSAFE_SCHEMA = requireFailsafe();
  jsYaml.JSON_SCHEMA = requireJson();
  jsYaml.CORE_SCHEMA = requireCore();
  jsYaml.DEFAULT_SCHEMA = require_default();
  jsYaml.load = loader2.load;
  jsYaml.loadAll = loader2.loadAll;
  jsYaml.dump = dumper2.dump;
  jsYaml.YAMLException = requireException();
  jsYaml.types = {
    binary: requireBinary(),
    float: requireFloat(),
    map: requireMap(),
    null: require_null(),
    pairs: requirePairs(),
    set: requireSet(),
    timestamp: requireTimestamp(),
    bool: requireBool(),
    int: requireInt(),
    merge: requireMerge(),
    omap: requireOmap(),
    seq: requireSeq(),
    str: requireStr()
  };
  jsYaml.safeLoad = renamed("safeLoad", "load");
  jsYaml.safeLoadAll = renamed("safeLoadAll", "loadAll");
  jsYaml.safeDump = renamed("safeDump", "dump");
  return jsYaml;
}
var jsYaml, loader, common, hasRequiredCommon, exception, hasRequiredException, snippet, hasRequiredSnippet, type, hasRequiredType, schema, hasRequiredSchema, str, hasRequiredStr, seq, hasRequiredSeq, map, hasRequiredMap, failsafe, hasRequiredFailsafe, _null, hasRequired_null, bool, hasRequiredBool, int, hasRequiredInt, float, hasRequiredFloat, json, hasRequiredJson, core, hasRequiredCore, timestamp, hasRequiredTimestamp, merge, hasRequiredMerge, binary, hasRequiredBinary, omap, hasRequiredOmap, pairs, hasRequiredPairs, set, hasRequiredSet, _default, hasRequired_default, hasRequiredLoader, dumper, hasRequiredDumper, hasRequiredJsYaml, jsYamlExports, yaml, Type, Schema, FAILSAFE_SCHEMA, JSON_SCHEMA, CORE_SCHEMA, DEFAULT_SCHEMA, load, loadAll, dump, YAMLException, types, safeLoad, safeLoadAll, safeDump;
var init_js_yaml = __esm({
  "node_modules/js-yaml/dist/js-yaml.mjs"() {
    jsYaml = {};
    loader = {};
    common = {};
    dumper = {};
    jsYamlExports = requireJsYaml();
    yaml = /* @__PURE__ */ getDefaultExportFromCjs(jsYamlExports);
    ({
      Type,
      Schema,
      FAILSAFE_SCHEMA,
      JSON_SCHEMA,
      CORE_SCHEMA,
      DEFAULT_SCHEMA,
      load,
      loadAll,
      dump,
      YAMLException,
      types,
      safeLoad,
      safeLoadAll,
      safeDump
    } = yaml);
  }
});

// src/dsh/upstream/include/src/index.ts
import { extname } from "node:path";
import { access, constants, readFile, rename, writeFile } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
function retryableWriteError(error) {
  const code = error?.code;
  return code === "EACCES" || code === "EBUSY" || code === "EPERM";
}
function applyEntryPatches(data2, patches, warn) {
  if (!patches?.length) return [...data2];
  data2 = structuredClone(data2);
  const entryMap = /* @__PURE__ */ new Map();
  const buildMap = (entries) => {
    for (const entry of entries) {
      if (entry.id) entryMap.set(entry.id, entry);
      if (entry.group && Array.isArray(entry.config)) {
        buildMap(entry.config);
      }
    }
  };
  buildMap(data2);
  for (const patch of patches) {
    const { id, insert, name, ...overrides } = patch;
    if (insert) {
      if (id) {
        const target2 = entryMap.get(id);
        if (!target2) {
          warn("patch insert: entry %C not found", id);
          continue;
        }
        if (!target2.group) {
          warn("patch insert: entry %C is not a group", id);
          continue;
        }
        if (!Array.isArray(target2.config)) target2.config = [];
        target2.config.push(...insert);
      } else {
        data2.push(...insert);
      }
      buildMap(insert);
      continue;
    }
    if (!id) {
      warn("patch: id is required for non-insert patches");
      continue;
    }
    const target = entryMap.get(id);
    if (!target) {
      warn("patch: entry %C not found", id);
      continue;
    }
    if (name && name !== target.name) {
      warn("patch: name mismatch for %C (expected %C, got %C), skipping", id, target.name, name);
      continue;
    }
    for (const [key, value] of Object.entries(overrides)) {
      if (key === "id") continue;
      target[key] = value;
    }
  }
  return data2;
}
var JsExpr, entryListSchema, schema2, writable, supported, WRITE_RETRY_LIMIT, WRITE_RETRY_DELAY_MS, Include;
var init_src2 = __esm({
  "src/dsh/upstream/include/src/index.ts"() {
    "use strict";
    init_src();
    init_lib2();
    init_js_yaml();
    JsExpr = new Type("tag:yaml.org,2002:js", {
      kind: "scalar",
      resolve: (data2) => typeof data2 === "string",
      construct: (data2) => ({ __jsExpr: data2 }),
      predicate: isJsExpr,
      represent: (data2) => data2.__jsExpr
    });
    entryListSchema = JSON_SCHEMA.extend(JsExpr);
    schema2 = entryListSchema;
    writable = {
      ".json": "application/json",
      ".yaml": "application/yaml",
      ".yml": "application/yaml"
    };
    supported = new Set(Object.keys(writable));
    WRITE_RETRY_LIMIT = 10;
    WRITE_RETRY_DELAY_MS = 50;
    Include = class extends EntryTree {
      constructor(ctx, config) {
        super(ctx);
        this.config = config;
        this.enableLogs = config.enableLogs ?? ctx.fiber.entry?.parent.tree.enableLogs ?? false;
        this.filename = fileURLToPath(new URL(this.config.path, this.ctx.baseUrl));
        const ext = extname(this.filename);
        if (!supported.has(ext)) {
          throw new Error(`extension "${ext}" not supported`);
        }
        this.type = writable[ext];
        this.readonly = !this.type;
        this.ctx.baseUrl = new URL(".", pathToFileURL(this.filename)).href;
        ctx.on("internal/update", (config2, _, next) => {
          if (config2.path !== this.config.path) return next();
          this.config = config2;
          this.root.update(this.applyPatches(this.data, config2.patches)).catch((error) => {
            this.ctx.logger.warn("config update at %C failed", this.filename);
            this.ctx.logger.warn(error);
          });
        });
      }
      config;
      static inject = ["loader"];
      // Tree-carrier marker (the Group plugin declares the same): this config is
      // entry and patch lists, so the Loader's `internal/config` interpolation
      // keeps it literal — a `!!js` expression inside a nested row's config
      // belongs to that row's fiber, resolving lazily in the row's own context.
      // Include's own fields (`path`, `enableLogs`) therefore stay literal too.
      static [EntryGroup.key] = true;
      filename;
      type;
      readonly;
      content;
      data;
      writeTask;
      pendingWrite;
      writeQueue = Promise.resolve();
      async checkAccess() {
        if (!this.type) return;
        try {
          await access(this.filename, constants.W_OK);
        } catch {
          this.readonly = true;
        }
      }
      async read(forced = false) {
        const content = await readFile(this.filename, "utf8");
        if (!forced && this.content === content) return false;
        let data2;
        if (this.type === "application/yaml") {
          data2 = load(content, { schema: entryListSchema });
        } else if (this.type === "application/json") {
          data2 = JSON.parse(content);
        } else {
          const module = await import(
            /* @vite-ignore */
            this.filename
          );
          data2 = module.default || module;
        }
        if (!Array.isArray(data2)) {
          throw new TypeError(`config file must be a top-level array of entries: ${this.filename}`);
        }
        this.content = content;
        this.data = data2;
        await this.checkAccess();
        return true;
      }
      applyPatches(data2, patches = this.config.patches) {
        return applyEntryPatches(data2, patches, (message, ...args) => {
          this.ctx.root.logger?.("loader").warn(message, ...args);
        });
      }
      async *[Service.init]() {
        try {
          await this.read();
        } catch (error) {
          if (error?.code !== "ENOENT") throw error;
          if (this.config.initial) {
            await this._writeFile(this.config.initial);
            await this.read(true);
          } else {
            throw new Error(`config file not found: ${this.filename}`);
          }
        }
        yield () => this.stop();
        await this.root.update(this.applyPatches(this.data));
      }
      async stop() {
        try {
          await this.flushWrite();
        } finally {
          this.root.stop();
          await this.flushWrite();
        }
      }
      /**
       * Re-read the file and refresh child entries when content changed. An
       * unreadable or unparsable file logs a warning and keeps the last good
       * tree: a hot-reload of a live app must never take the process down.
       */
      async refresh() {
        try {
          if (!await this.read()) return;
          await this.root.update(this.applyPatches(this.data));
        } catch (error) {
          this.ctx.logger.warn("config reload at %C failed; keeping the running tree", this.filename);
          this.ctx.logger.warn(error);
        }
      }
      async _writeFile(config) {
        if (this.readonly) {
          throw new Error(`cannot overwrite readonly config`);
        }
        if (this.type === "application/yaml") {
          this.content = dump(config, { schema: schema2 });
        } else if (this.type === "application/json") {
          this.content = JSON.stringify(config, null, 2);
        }
        await writeFile(this.filename + ".tmp", this.content);
        for (let retry = 0; ; retry++) {
          try {
            await rename(this.filename + ".tmp", this.filename);
            return;
          } catch (error) {
            if (!retryableWriteError(error) || retry >= WRITE_RETRY_LIMIT) throw error;
            await delay((retry + 1) * WRITE_RETRY_DELAY_MS);
          }
        }
      }
      writeFile(config) {
        clearTimeout(this.writeTask);
        this.pendingWrite = config;
        this.writeTask = setTimeout(() => {
          void this.flushWrite();
        }, 0);
      }
      flushWrite() {
        clearTimeout(this.writeTask);
        this.writeTask = void 0;
        const config = this.pendingWrite;
        this.pendingWrite = void 0;
        if (config === void 0) return this.writeQueue;
        const run = this.writeQueue.then(
          () => this._writeFile(config),
          () => this._writeFile(config)
        );
        this.writeQueue = run;
        void run.catch((error) => {
          this.ctx.root.logger?.("loader").warn("failed to write config file %C", this.filename);
          this.ctx.root.logger?.("loader").warn(error);
        });
        return run;
      }
      /** Schedule a write of the current root entry data. */
      write() {
        this.context.emit("loader/config-update");
        return this.writeFile(this.root.data);
      }
    };
  }
});

// src/json.ts
function isJson(value, depth = 0) {
  if (depth > 64) return false;
  if (value === null) return true;
  switch (typeof value) {
    case "boolean":
    case "string":
      return true;
    case "number":
      return Number.isFinite(value);
    case "object": {
      const array = Array.isArray(value);
      if (!array && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) return false;
      const keys = Reflect.ownKeys(value);
      if (array && keys.length !== value.length + 1) return false;
      for (const key of keys) {
        if (array && key === "length") continue;
        if (typeof key !== "string") return false;
        if (array && (!/^(0|[1-9]\d*)$/u.test(key) || Number(key) >= value.length)) return false;
        const descriptor = Object.getOwnPropertyDescriptor(value, key);
        if (!descriptor.enumerable || !("value" in descriptor) || !isJson(descriptor.value, depth + 1)) return false;
      }
      return true;
    }
    default:
      return false;
  }
}
var init_json = __esm({
  "src/json.ts"() {
    "use strict";
  }
});

// src/shims/mcp.ts
function normalizeMcp(value) {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((k) => k !== "serverName" && k !== "server")) throw new TypeError("MCP needs serverName and server");
  const { serverName, server } = value;
  if (typeof serverName !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$/.test(serverName) || !server || Array.isArray(server) || typeof server !== "object" || !isJson(server)) throw new TypeError("MCP definition must be bounded plain JSON");
  for (const key of ["headers", "env"]) if (server[key] !== void 0 && (!server[key] || typeof server[key] !== "object" || Array.isArray(server[key]) || Object.keys(server[key]).length)) throw new TypeError("MCP credentials remain under Rust authentication authority");
  if (typeof server.url === "string") {
    const url = new URL(server.url);
    if (url.username || url.password || url.search || url.hash) throw new TypeError("MCP endpoint credentials and query data remain under Rust authority");
  }
  if (Buffer.byteLength(JSON.stringify(server)) > 64 * 1024) throw new RangeError("MCP definition exceeds 64 KiB");
  return Object.freeze({ serverName, server: structuredClone(server) });
}
var init_mcp = __esm({
  "src/shims/mcp.ts"() {
    "use strict";
    init_json();
    init_owned();
  }
});

// src/dsh/mcp.ts
function reviewedMcpModule() {
  return {
    name: "codewhale-reviewed-mcp",
    inject: ["mcp"],
    apply(ctx, config) {
      if (!config || Array.isArray(config) || Object.keys(config).some((k) => !["transport", "serverName", "command", "args", "env", "cwd", "url", "headers", "toolCallTimeoutMs", "failOnStartupError"].includes(k))) throw new Error("MCP row uses unsupported configuration; Core owns reconnect and instruction bounds");
      if (typeof config.serverName !== "string") throw new Error("MCP serverName must be literal");
      const options = {};
      if (config.toolCallTimeoutMs !== void 0) {
        const timeout = config.toolCallTimeoutMs;
        if (typeof timeout !== "number" || !Number.isInteger(timeout) || timeout < 1e3 || timeout > 864e5 || timeout % 1e3 !== 0) throw new Error("MCP timeout must be whole seconds within Core bounds");
        options.execute_timeout = timeout / 1e3;
      }
      if (config.failOnStartupError === true) throw new Error("failOnStartupError requires explicit Core MCP connection qualification before activation");
      if (config.failOnStartupError !== void 0 && config.failOnStartupError !== false) throw new Error("MCP startup gate must be a boolean");
      let server;
      if (config.transport === "stdio") {
        const cwd = config.cwd ?? "";
        if (typeof cwd !== "string" || cwd !== "" && cwd !== "." && (!cwd || /[\\:\u0000-\u001f]/u.test(cwd) || cwd.split("/").some((p) => !p || p === "." || p === ".."))) throw new Error("MCP cwd must be a reviewed bundle-relative directory");
        server = { type: "stdio", command: config.command, args: config.args ?? [], env: config.env ?? {}, cwd: cwd === "" || cwd === "." ? "source" : `source/${cwd}` };
      } else if (config.transport === "streamable-http" || config.transport === "sse") server = { type: config.transport, url: config.url, headers: config.headers ?? {} };
      else throw new Error("MCP transport must be stdio, streamable-http or sse");
      if (Object.keys(options).length) server.extensions = { "net.codewhale": options };
      ctx.mcp.registerServer(normalizeMcp({ serverName: config.serverName, server }));
    }
  };
}
var init_mcp2 = __esm({
  "src/dsh/mcp.ts"() {
    "use strict";
    init_mcp();
  }
});

// src/shims/skills.ts
function normalizeSkillRoot(value) {
  if (typeof value !== "object" || value === null || Array.isArray(value) || Object.keys(value).some((key) => key !== "path")) throw new TypeError("skill root supports only path");
  const { path } = value;
  if (typeof path !== "string" || !path || Buffer.byteLength(path, "utf8") > 512 || /[\\:\u0000-\u001f\u007f-\u009f]/u.test(path) || path.split("/").some((part) => !part || part === "." || part === "..")) {
    throw new TypeError("skill root path must be a bounded bundle-relative path with normal slash-separated components");
  }
  return Object.freeze({ path });
}
var init_skills = __esm({
  "src/shims/skills.ts"() {
    "use strict";
    init_owned();
  }
});

// src/dsh/skill-filesystem.ts
function reviewedSkillModule() {
  return { name: "codewhale-reviewed-skills", inject: ["skills"], apply(ctx, config) {
    if (!config || Array.isArray(config) || Object.keys(config).some((k) => !["providerName", "includeDefaultRoots", "customSkillDirs", "watch"].includes(k))) throw new Error("filesystem row requires reviewed roots; ambient roots and watcher controls are unsupported");
    if (config.includeDefaultRoots !== false || config.watch !== false) throw new Error("filesystem row must explicitly disable default roots and watchers");
    if (config.providerName !== void 0 && (typeof config.providerName !== "string" || !config.providerName || config.providerName.length > 64)) throw new Error("skill providerName must be bounded");
    if (!Array.isArray(config.customSkillDirs) || !config.customSkillDirs.length || config.customSkillDirs.length > 8) throw new Error("filesystem row needs 1–8 reviewed roots");
    const roots = config.customSkillDirs.map((path) => normalizeSkillRoot({ path }));
    const seen = /* @__PURE__ */ new Set();
    for (const { path } of roots) {
      if (seen.has(path)) throw new Error("duplicate filesystem root");
      seen.add(path);
    }
    for (const { path } of roots) ctx.skills.registerRoot({ path: `source/${path}` });
  } };
}
var init_skill_filesystem = __esm({
  "src/dsh/skill-filesystem.ts"() {
    "use strict";
    init_skills();
  }
});

// src/dsh/canonical-path.ts
import { realpathSync } from "node:fs";
import { posix, win32 } from "node:path";
function canonicalPath(path, platform = process.platform) {
  if (platform !== "win32") return realpathSync(path);
  try {
    return stripVerbatim(realpathSync.native(path), platform);
  } catch (error) {
    const code = error && typeof error === "object" && "code" in error ? error.code : void 0;
    if (code === "EPERM" || code === "EACCES") {
      return stripVerbatim(win32.normalize(path), platform);
    }
    throw error;
  }
}
function stripVerbatim(path, platform = process.platform) {
  if (platform !== "win32") return path;
  if (/^[\\/]{2}\?[\\/]UNC[\\/]/i.test(path)) return `\\\\${path.slice(8)}`;
  if (/^[\\/]{2}\?[\\/]/.test(path)) return path.slice(4);
  return path;
}
function pathKey(path, platform = process.platform) {
  if (platform !== "win32") return posix.normalize(path);
  return win32.normalize(stripVerbatim(path, platform)).toLowerCase();
}
function samePath(a, b, platform = process.platform) {
  return pathKey(a, platform) === pathKey(b, platform);
}
function insideKey(root, target, platform = process.platform) {
  const path = platform === "win32" ? win32 : posix;
  const inside = path.relative(stripVerbatim(root, platform), stripVerbatim(target, platform));
  if (inside === ".." || inside.startsWith(`..${path.sep}`) || path.isAbsolute(inside)) return void 0;
  return platform === "win32" ? inside.split(win32.sep).join("/") : inside;
}
function isUnlinkedInside(root, key, target, platform = process.platform) {
  let canonicalRoot, canonicalTarget;
  try {
    canonicalRoot = canonicalPath(root, platform);
    canonicalTarget = canonicalPath(target, platform);
  } catch {
    return false;
  }
  return unlinkedKeyMatches(canonicalRoot, key, canonicalTarget, platform);
}
function unlinkedKeyMatches(canonicalRoot, key, canonicalTarget, platform = process.platform) {
  if (!key || key.split("/").some((part) => !part || part === "." || part === "..")) return false;
  const path = platform === "win32" ? win32 : posix;
  return samePath(path.join(stripVerbatim(canonicalRoot, platform), ...key.split("/")), canonicalTarget, platform);
}
var init_canonical_path = __esm({
  "src/dsh/canonical-path.ts"() {
    "use strict";
  }
});

// src/dsh/upstream/hooks/hook-protocol/src/matcher.ts
function isMatchAll(matcher) {
  return matcher === void 0 || matcher === "" || matcher === "*";
}
function compileRegex(pattern) {
  try {
    return new RegExp(pattern);
  } catch (_syntaxError) {
    return void 0;
  }
}
function matcherDiagnostic(matcher, mode) {
  if (isMatchAll(matcher)) return void 0;
  const pattern = matcher;
  if (mode === "claude-code" && CLAUDE_LITERAL.test(pattern)) return void 0;
  return compileRegex(pattern) === void 0 ? `invalid ${mode} regex matcher ${JSON.stringify(pattern)}` : void 0;
}
var CLAUDE_LITERAL;
var init_matcher = __esm({
  "src/dsh/upstream/hooks/hook-protocol/src/matcher.ts"() {
    "use strict";
    CLAUDE_LITERAL = /^[A-Za-z0-9_|]+$/;
  }
});

// src/dsh/upstream/hooks/hooks-claude-code/src/config.ts
function asObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value : void 0;
}
function substituteCommand(command, vars) {
  let out = command;
  if (vars.pluginRoot !== void 0) out = out.split("${CLAUDE_PLUGIN_ROOT}").join(vars.pluginRoot);
  if (vars.projectDir !== void 0) out = out.split("${CLAUDE_PROJECT_DIR}").join(vars.projectDir);
  return out;
}
function parseClaudeCodeConfig(raw, vars = {}) {
  const config = {};
  const skipped = [];
  const root = asObject(raw);
  const hooksMap = root ? asObject(root.hooks) ?? root : void 0;
  if (!hooksMap) return { config, skipped };
  for (const event of CLAUDE_EVENTS) {
    const rawGroups = hooksMap[event];
    if (!Array.isArray(rawGroups)) continue;
    const groups = [];
    for (const rawGroup of rawGroups) {
      const group = asObject(rawGroup);
      if (!group || !Array.isArray(group.hooks)) continue;
      const commands = [];
      for (const rawHook of group.hooks) {
        const hook = asObject(rawHook);
        if (!hook) continue;
        const type2 = typeof hook.type === "string" ? hook.type : "command";
        if (type2 !== "command") {
          skipped.push({ event, type: type2 });
          continue;
        }
        if (typeof hook.command !== "string") continue;
        commands.push({
          command: substituteCommand(hook.command, vars),
          ...typeof hook.timeout === "number" ? { timeoutSec: hook.timeout } : {}
        });
      }
      if (commands.length === 0) continue;
      const matcher = event === "UserPromptSubmit" || event === "Stop" ? void 0 : typeof group.matcher === "string" ? group.matcher : void 0;
      const diagnostic = matcherDiagnostic(matcher, "claude-code");
      if (diagnostic !== void 0) throw new SyntaxError(`${diagnostic} on event ${JSON.stringify(event)}`);
      groups.push({
        ...matcher !== void 0 ? { matcher } : {},
        hooks: commands
      });
    }
    if (groups.length > 0) config[event] = groups;
  }
  return { config, skipped };
}
var CLAUDE_EVENTS;
var init_config = __esm({
  "src/dsh/upstream/hooks/hooks-claude-code/src/config.ts"() {
    "use strict";
    init_matcher();
    CLAUDE_EVENTS = [
      "SessionStart",
      "UserPromptSubmit",
      "PreToolUse",
      "PostToolUse",
      "Stop",
      "SubagentStart",
      "SubagentStop"
    ];
  }
});

// src/dsh/upstream/hooks/hooks-codex/src/config.ts
function asObject2(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value : void 0;
}
function parseCodexConfig(raw) {
  const config = {};
  const skipped = [];
  const root = asObject2(raw);
  const hooksMap = root ? asObject2(root.hooks) ?? root : void 0;
  if (!hooksMap) return { config, skipped };
  for (const event of CODEX_EVENTS) {
    const rawGroups = hooksMap[event];
    if (!Array.isArray(rawGroups)) continue;
    const groups = [];
    for (const rawGroup of rawGroups) {
      const group = asObject2(rawGroup);
      if (!group || !Array.isArray(group.hooks)) continue;
      const commands = [];
      for (const rawHook of group.hooks) {
        const hook = asObject2(rawHook);
        if (!hook) continue;
        const type2 = typeof hook.type === "string" ? hook.type : "command";
        if (type2 !== "command") {
          skipped.push({ event, reason: `unsupported "${type2}" hook` });
          continue;
        }
        if (hook.async === true) {
          skipped.push({ event, reason: "async hook" });
          continue;
        }
        if (typeof hook.command !== "string") continue;
        const timeout = typeof hook.timeout === "number" ? hook.timeout : typeof hook.timeoutSec === "number" ? hook.timeoutSec : void 0;
        commands.push({ command: hook.command, ...timeout !== void 0 ? { timeoutSec: timeout } : {} });
      }
      if (commands.length === 0) continue;
      const matcher = event === "UserPromptSubmit" || event === "Stop" ? void 0 : typeof group.matcher === "string" ? group.matcher : void 0;
      const diagnostic = matcherDiagnostic(matcher, "codex");
      if (diagnostic !== void 0) throw new SyntaxError(`${diagnostic} on event ${JSON.stringify(event)}`);
      groups.push({ ...matcher !== void 0 ? { matcher } : {}, hooks: commands });
    }
    if (groups.length > 0) config[event] = groups;
  }
  return { config, skipped };
}
var CODEX_EVENTS;
var init_config2 = __esm({
  "src/dsh/upstream/hooks/hooks-codex/src/config.ts"() {
    "use strict";
    init_matcher();
    CODEX_EVENTS = ["PreToolUse", "PostToolUse", "SessionStart", "UserPromptSubmit", "Stop"];
  }
});

// src/dsh/shell-hooks.ts
import { createHash } from "node:crypto";
import { readFileSync, lstatSync } from "node:fs";
import { resolve, relative, isAbsolute, sep } from "node:path";
function reviewedHookModule(dialect, root, files) {
  return { name: `hooks-${dialect}`, inject: ["shellHooks"], apply(ctx, config) {
    if (!config || typeof config.configPath !== "string") throw new Error("hook bridge needs its reviewed configPath");
    const path = resolve(root, config.configPath);
    const inside = relative(root, path).split(sep).join("/");
    if (!inside || inside.startsWith("../") || isAbsolute(inside) || !files[inside] || !isUnlinkedInside(root, inside, path) || !lstatSync(path).isFile()) throw new Error("hook config is absent from the reviewed regular-file closure");
    const bytes = readFileSync(path);
    if (bytes.length > 1024 * 1024 || createHash("sha256").update(bytes).digest("hex") !== files[inside]) throw new Error("hook config changed after review or exceeds 1 MiB");
    if (config.projectDir !== void 0) throw new Error("explicit projectDir is unsupported; each process uses its current core workspace");
    if (config.pluginRoot !== void 0 && config.pluginRoot !== "." && config.pluginRoot !== root) throw new Error("pluginRoot must be this reviewed bundle root");
    const raw = JSON.parse(bytes.toString("utf8"));
    const parsed = dialect === "claude-code" ? parseClaudeCodeConfig(raw, { pluginRoot: root }) : parseCodexConfig(raw);
    if (parsed.skipped.length) throw new Error("hook config contains unsupported non-command or asynchronous hooks");
    let count = 0;
    for (const [point, groups] of Object.entries(parsed.config)) {
      if (point === "Stop") ctx.logger.warn("Stop runs at the core TurnEnd observer; forced continuation is unsupported");
      for (const group of groups) for (const hook of group.hooks) {
        if (++count > 128) throw new Error("hook bridge exceeds 128 commands");
        const seconds = hook.timeoutSec ?? (config.defaultTimeoutMs ?? 6e5) / 1e3;
        if (!Number.isSafeInteger(seconds) || seconds < 1 || seconds > 86400) throw new Error("hook timeout must be 1–86400 whole seconds");
        ctx.shellHooks.register({ dialect, point, ...group.matcher === void 0 ? {} : { matcher: group.matcher }, hook: { event: EVENTS[point], command: hook.command, timeout_secs: seconds, background: false, continue_on_error: false, name: `${dialect}:${point}:${count}` } });
      }
    }
    if (count === 0) throw new Error("reviewed hook config has no supported command hooks");
  } };
}
var EVENTS;
var init_shell_hooks = __esm({
  "src/dsh/shell-hooks.ts"() {
    "use strict";
    init_canonical_path();
    init_canonical_path();
    init_config();
    init_config2();
    EVENTS = { SessionStart: "session_start", UserPromptSubmit: "message_submit", PreToolUse: "tool_call_before", PostToolUse: "tool_call_after", Stop: "turn_end", SubagentStart: "subagent_spawn", SubagentStop: "subagent_complete" };
  }
});

// src/runtime.ts
import * as nodeModule from "node:module";
var require2, RUNTIME;
var init_runtime = __esm({
  "src/runtime.ts"() {
    "use strict";
    require2 = nodeModule.createRequire(import.meta.url);
    RUNTIME = typeof process.versions.bun === "string" ? { name: "bun", version: process.versions.bun } : { name: "node", version: process.versions.node };
  }
});

// node_modules/acorn/dist/acorn.mjs
function isInAstralSet(code, set2) {
  var pos = 65536;
  for (var i2 = 0; i2 < set2.length; i2 += 2) {
    pos += set2[i2];
    if (pos > code) {
      return false;
    }
    pos += set2[i2 + 1];
    if (pos >= code) {
      return true;
    }
  }
  return false;
}
function isIdentifierStart(code, astral) {
  if (code < 65) {
    return code === 36;
  }
  if (code < 91) {
    return true;
  }
  if (code < 97) {
    return code === 95;
  }
  if (code < 123) {
    return true;
  }
  if (code <= 65535) {
    return code >= 170 && nonASCIIidentifierStart.test(String.fromCharCode(code));
  }
  if (astral === false) {
    return false;
  }
  return isInAstralSet(code, astralIdentifierStartCodes);
}
function isIdentifierChar(code, astral) {
  if (code < 48) {
    return code === 36;
  }
  if (code < 58) {
    return true;
  }
  if (code < 65) {
    return false;
  }
  if (code < 91) {
    return true;
  }
  if (code < 97) {
    return code === 95;
  }
  if (code < 123) {
    return true;
  }
  if (code <= 65535) {
    return code >= 170 && nonASCIIidentifier.test(String.fromCharCode(code));
  }
  if (astral === false) {
    return false;
  }
  return isInAstralSet(code, astralIdentifierStartCodes) || isInAstralSet(code, astralIdentifierCodes);
}
function binop(name, prec) {
  return new TokenType(name, { beforeExpr: true, binop: prec });
}
function kw(name, options) {
  if (options === void 0) options = {};
  options.keyword = name;
  return keywords[name] = new TokenType(name, options);
}
function isNewLine(code) {
  return code === 10 || code === 13 || code === 8232 || code === 8233;
}
function nextLineBreak(code, from, end) {
  if (end === void 0) end = code.length;
  for (var i2 = from; i2 < end; i2++) {
    var next = code.charCodeAt(i2);
    if (isNewLine(next)) {
      return i2 < end - 1 && next === 13 && code.charCodeAt(i2 + 1) === 10 ? i2 + 2 : i2 + 1;
    }
  }
  return -1;
}
function wordsRegexp(words) {
  return regexpCache[words] || (regexpCache[words] = new RegExp("^(?:" + words.replace(/ /g, "|") + ")$"));
}
function codePointToString(code) {
  if (code <= 65535) {
    return String.fromCharCode(code);
  }
  code -= 65536;
  return String.fromCharCode((code >> 10) + 55296, (code & 1023) + 56320);
}
function getLineInfo(input, offset2) {
  for (var line = 1, cur = 0; ; ) {
    var nextBreak = nextLineBreak(input, cur, offset2);
    if (nextBreak < 0) {
      return new Position(line, offset2 - cur);
    }
    ++line;
    cur = nextBreak;
  }
}
function getOptions(opts) {
  var options = {};
  for (var opt in defaultOptions) {
    options[opt] = opts && hasOwn(opts, opt) ? opts[opt] : defaultOptions[opt];
  }
  if (options.ecmaVersion === "latest") {
    options.ecmaVersion = 1e8;
  } else if (options.ecmaVersion == null) {
    if (!warnedAboutEcmaVersion && typeof console === "object" && console.warn) {
      warnedAboutEcmaVersion = true;
      console.warn("Since Acorn 8.0.0, options.ecmaVersion is required.\nDefaulting to 2020, but this will stop working in the future.");
    }
    options.ecmaVersion = 11;
  } else if (options.ecmaVersion >= 2015) {
    options.ecmaVersion -= 2009;
  }
  if (options.allowReserved == null) {
    options.allowReserved = options.ecmaVersion < 5;
  }
  if (!opts || opts.allowHashBang == null) {
    options.allowHashBang = options.ecmaVersion >= 14;
  }
  if (isArray(options.onToken)) {
    var tokens = options.onToken;
    options.onToken = function(token) {
      return tokens.push(token);
    };
  }
  if (isArray(options.onComment)) {
    options.onComment = pushComment(options, options.onComment);
  }
  if (options.sourceType === "commonjs" && options.allowAwaitOutsideFunction) {
    throw new Error("Cannot use allowAwaitOutsideFunction with sourceType: commonjs");
  }
  return options;
}
function pushComment(options, array) {
  return function(block, text2, start, end, startLoc, endLoc) {
    var comment = {
      type: block ? "Block" : "Line",
      value: text2,
      start,
      end
    };
    if (options.locations) {
      comment.loc = new SourceLocation(this, startLoc, endLoc);
    }
    if (options.ranges) {
      comment.range = [start, end];
    }
    array.push(comment);
  };
}
function functionFlags(async, generator) {
  return SCOPE_FUNCTION | (async ? SCOPE_ASYNC : 0) | (generator ? SCOPE_GENERATOR : 0);
}
function isPrivateNameConflicted(privateNameMap, element) {
  var name = element.key.name;
  var curr = privateNameMap[name];
  var next = "true";
  if (element.type === "MethodDefinition" && (element.kind === "get" || element.kind === "set")) {
    next = (element.static ? "s" : "i") + element.kind;
  }
  if (curr === "iget" && next === "iset" || curr === "iset" && next === "iget" || curr === "sget" && next === "sset" || curr === "sset" && next === "sget") {
    privateNameMap[name] = "true";
    return false;
  } else if (!curr) {
    privateNameMap[name] = next;
    return false;
  } else {
    return true;
  }
}
function checkKeyName(node, name) {
  var computed = node.computed;
  var key = node.key;
  return !computed && (key.type === "Identifier" && key.name === name || key.type === "Literal" && key.value === name);
}
function isLocalVariableAccess(node) {
  return node.type === "Identifier" || node.type === "ParenthesizedExpression" && isLocalVariableAccess(node.expression);
}
function isPrivateFieldAccess(node) {
  return node.type === "MemberExpression" && node.property.type === "PrivateIdentifier" || node.type === "ChainExpression" && isPrivateFieldAccess(node.expression) || node.type === "ParenthesizedExpression" && isPrivateFieldAccess(node.expression);
}
function finishNodeAt(node, type2, pos, loc) {
  node.type = type2;
  node.end = pos;
  if (this.options.locations) {
    node.loc.end = loc;
  }
  if (this.options.ranges) {
    node.range[1] = pos;
  }
  return node;
}
function buildUnicodeData(ecmaVersion2) {
  var d = data[ecmaVersion2] = {
    binary: wordsRegexp(unicodeBinaryProperties[ecmaVersion2] + " " + unicodeGeneralCategoryValues),
    binaryOfStrings: wordsRegexp(unicodeBinaryPropertiesOfStrings[ecmaVersion2]),
    nonBinary: {
      General_Category: wordsRegexp(unicodeGeneralCategoryValues),
      Script: wordsRegexp(unicodeScriptValues[ecmaVersion2])
    }
  };
  d.nonBinary.Script_Extensions = d.nonBinary.Script;
  d.nonBinary.gc = d.nonBinary.General_Category;
  d.nonBinary.sc = d.nonBinary.Script;
  d.nonBinary.scx = d.nonBinary.Script_Extensions;
}
function hasProp(obj) {
  for (var _ in obj) {
    return true;
  }
  return false;
}
function isRegularExpressionModifier(ch) {
  return ch === 105 || ch === 109 || ch === 115;
}
function isSyntaxCharacter(ch) {
  return ch === 36 || ch >= 40 && ch <= 43 || ch === 46 || ch === 63 || ch >= 91 && ch <= 94 || ch >= 123 && ch <= 125;
}
function isRegExpIdentifierStart(ch) {
  return isIdentifierStart(ch, true) || ch === 36 || ch === 95;
}
function isRegExpIdentifierPart(ch) {
  return isIdentifierChar(ch, true) || ch === 36 || ch === 95 || ch === 8204 || ch === 8205;
}
function isControlLetter(ch) {
  return ch >= 65 && ch <= 90 || ch >= 97 && ch <= 122;
}
function isValidUnicode(ch) {
  return ch >= 0 && ch <= 1114111;
}
function isCharacterClassEscape(ch) {
  return ch === 100 || ch === 68 || ch === 115 || ch === 83 || ch === 119 || ch === 87;
}
function isUnicodePropertyNameCharacter(ch) {
  return isControlLetter(ch) || ch === 95;
}
function isUnicodePropertyValueCharacter(ch) {
  return isUnicodePropertyNameCharacter(ch) || isDecimalDigit(ch);
}
function isClassSetReservedDoublePunctuatorCharacter(ch) {
  return ch === 33 || ch >= 35 && ch <= 38 || ch >= 42 && ch <= 44 || ch === 46 || ch >= 58 && ch <= 64 || ch === 94 || ch === 96 || ch === 126;
}
function isClassSetSyntaxCharacter(ch) {
  return ch === 40 || ch === 41 || ch === 45 || ch === 47 || ch >= 91 && ch <= 93 || ch >= 123 && ch <= 125;
}
function isClassSetReservedPunctuator(ch) {
  return ch === 33 || ch === 35 || ch === 37 || ch === 38 || ch === 44 || ch === 45 || ch >= 58 && ch <= 62 || ch === 64 || ch === 96 || ch === 126;
}
function isDecimalDigit(ch) {
  return ch >= 48 && ch <= 57;
}
function isHexDigit(ch) {
  return ch >= 48 && ch <= 57 || ch >= 65 && ch <= 70 || ch >= 97 && ch <= 102;
}
function hexToInt(ch) {
  if (ch >= 65 && ch <= 70) {
    return 10 + (ch - 65);
  }
  if (ch >= 97 && ch <= 102) {
    return 10 + (ch - 97);
  }
  return ch - 48;
}
function isOctalDigit(ch) {
  return ch >= 48 && ch <= 55;
}
function stringToNumber(str2, isLegacyOctalNumericLiteral) {
  if (isLegacyOctalNumericLiteral) {
    return parseInt(str2, 8);
  }
  return parseFloat(str2.replace(/_/g, ""));
}
function stringToBigInt(str2) {
  if (typeof BigInt !== "function") {
    return null;
  }
  return BigInt(str2.replace(/_/g, ""));
}
var astralIdentifierCodes, astralIdentifierStartCodes, nonASCIIidentifierChars, nonASCIIidentifierStartChars, reservedWords, ecma5AndLessKeywords, keywords$1, keywordRelationalOperator, nonASCIIidentifierStart, nonASCIIidentifier, TokenType, beforeExpr, startsExpr, keywords, types$1, lineBreak, lineBreakG, nonASCIIwhitespace, skipWhiteSpace, ref, hasOwnProperty, toString, hasOwn, isArray, regexpCache, loneSurrogate, Position, SourceLocation, defaultOptions, warnedAboutEcmaVersion, SCOPE_TOP, SCOPE_FUNCTION, SCOPE_ASYNC, SCOPE_GENERATOR, SCOPE_ARROW, SCOPE_SIMPLE_CATCH, SCOPE_SUPER, SCOPE_DIRECT_SUPER, SCOPE_CLASS_STATIC_BLOCK, SCOPE_CLASS_FIELD_INIT, SCOPE_SWITCH, SCOPE_VAR, BIND_NONE, BIND_VAR, BIND_LEXICAL, BIND_FUNCTION, BIND_SIMPLE_CATCH, BIND_OUTSIDE, Parser, prototypeAccessors, pp$9, literal, DestructuringErrors, pp$8, loopLabel, switchLabel, empty$1, FUNC_STATEMENT, FUNC_HANGING_STATEMENT, FUNC_NULLABLE_ID, pp$7, TokContext, types2, pp$6, pp$5, empty, pp$4, pp$3, Scope, Node, pp$2, scriptValuesAddedInUnicode, ecma9BinaryProperties, ecma10BinaryProperties, ecma11BinaryProperties, ecma12BinaryProperties, ecma13BinaryProperties, ecma14BinaryProperties, unicodeBinaryProperties, ecma14BinaryPropertiesOfStrings, unicodeBinaryPropertiesOfStrings, unicodeGeneralCategoryValues, ecma9ScriptValues, ecma10ScriptValues, ecma11ScriptValues, ecma12ScriptValues, ecma13ScriptValues, ecma14ScriptValues, unicodeScriptValues, data, ecmaVersion, i, list, pp$1, BranchID, RegExpValidationState, CharSetNone, CharSetOk, CharSetString, Token, pp, INVALID_TEMPLATE_ESCAPE_ERROR, version;
var init_acorn = __esm({
  "node_modules/acorn/dist/acorn.mjs"() {
    astralIdentifierCodes = [509, 0, 227, 0, 150, 4, 294, 9, 1368, 2, 2, 1, 6, 3, 41, 2, 5, 0, 166, 1, 574, 3, 9, 9, 7, 9, 32, 4, 318, 1, 78, 5, 71, 10, 50, 3, 123, 2, 54, 14, 32, 10, 3, 1, 11, 3, 46, 10, 8, 0, 46, 9, 7, 2, 37, 13, 2, 9, 6, 1, 45, 0, 13, 2, 49, 13, 9, 3, 2, 11, 83, 11, 7, 0, 3, 0, 158, 11, 6, 9, 7, 3, 56, 1, 2, 6, 3, 1, 3, 2, 10, 0, 11, 1, 3, 6, 4, 4, 68, 8, 2, 0, 3, 0, 2, 3, 2, 4, 2, 0, 15, 1, 83, 17, 10, 9, 5, 0, 82, 19, 13, 9, 214, 6, 3, 8, 28, 1, 83, 16, 16, 9, 82, 12, 9, 9, 7, 19, 58, 14, 5, 9, 243, 14, 166, 9, 71, 5, 2, 1, 3, 3, 2, 0, 2, 1, 13, 9, 120, 6, 3, 6, 4, 0, 29, 9, 41, 6, 2, 3, 9, 0, 10, 10, 47, 15, 199, 7, 137, 9, 54, 7, 2, 7, 17, 9, 57, 21, 2, 13, 123, 5, 4, 0, 2, 1, 2, 6, 2, 0, 9, 9, 49, 4, 2, 1, 2, 4, 9, 9, 55, 9, 266, 3, 10, 1, 2, 0, 49, 6, 4, 4, 14, 10, 5350, 0, 7, 14, 11465, 27, 2343, 9, 87, 9, 39, 4, 60, 6, 26, 9, 535, 9, 470, 0, 2, 54, 8, 3, 82, 0, 12, 1, 19628, 1, 4178, 9, 519, 45, 3, 22, 543, 4, 4, 5, 9, 7, 3, 6, 31, 3, 149, 2, 1418, 49, 513, 54, 5, 49, 9, 0, 15, 0, 23, 4, 2, 14, 1361, 6, 2, 16, 3, 6, 2, 1, 2, 4, 101, 0, 161, 6, 10, 9, 357, 0, 62, 13, 499, 13, 245, 1, 2, 9, 233, 0, 3, 0, 8, 1, 6, 0, 475, 6, 110, 6, 6, 9, 4759, 9, 787719, 239];
    astralIdentifierStartCodes = [0, 11, 2, 25, 2, 18, 2, 1, 2, 14, 3, 13, 35, 122, 70, 52, 268, 28, 4, 48, 48, 31, 14, 29, 6, 37, 11, 29, 3, 35, 5, 7, 2, 4, 43, 157, 19, 35, 5, 35, 5, 39, 9, 51, 13, 10, 2, 14, 2, 6, 2, 1, 2, 10, 2, 14, 2, 6, 2, 1, 4, 51, 13, 310, 10, 21, 11, 7, 25, 5, 2, 41, 2, 8, 70, 5, 3, 0, 2, 43, 2, 1, 4, 0, 3, 22, 11, 22, 10, 30, 66, 18, 2, 1, 11, 21, 11, 25, 7, 25, 39, 55, 7, 1, 65, 0, 16, 3, 2, 2, 2, 28, 43, 28, 4, 28, 36, 7, 2, 27, 28, 53, 11, 21, 11, 18, 14, 17, 111, 72, 56, 50, 14, 50, 14, 35, 39, 27, 10, 22, 251, 41, 7, 1, 17, 5, 57, 28, 11, 0, 9, 21, 43, 17, 47, 20, 28, 22, 13, 52, 58, 1, 3, 0, 14, 44, 33, 24, 27, 35, 30, 0, 3, 0, 9, 34, 4, 0, 13, 47, 15, 3, 22, 0, 2, 0, 36, 17, 2, 24, 20, 1, 64, 6, 2, 0, 2, 3, 2, 14, 2, 9, 8, 46, 39, 7, 3, 1, 3, 21, 2, 6, 2, 1, 2, 4, 4, 0, 19, 0, 13, 4, 31, 9, 2, 0, 3, 0, 2, 37, 2, 0, 26, 0, 2, 0, 45, 52, 19, 3, 21, 2, 31, 47, 21, 1, 2, 0, 185, 46, 42, 3, 37, 47, 21, 0, 60, 42, 14, 0, 72, 26, 38, 6, 186, 43, 117, 63, 32, 7, 3, 0, 3, 7, 2, 1, 2, 23, 16, 0, 2, 0, 95, 7, 3, 38, 17, 0, 2, 0, 29, 0, 11, 39, 8, 0, 22, 0, 12, 45, 20, 0, 19, 72, 200, 32, 32, 8, 2, 36, 18, 0, 50, 29, 113, 6, 2, 1, 2, 37, 22, 0, 26, 5, 2, 1, 2, 31, 15, 0, 24, 43, 261, 18, 16, 0, 2, 12, 2, 33, 125, 0, 80, 921, 103, 110, 18, 195, 2637, 96, 16, 1071, 18, 5, 26, 3994, 6, 582, 6842, 29, 1763, 568, 8, 30, 18, 78, 18, 29, 19, 47, 17, 3, 32, 20, 6, 18, 433, 44, 212, 63, 33, 24, 3, 24, 45, 74, 6, 0, 67, 12, 65, 1, 2, 0, 15, 4, 10, 7381, 42, 31, 98, 114, 8702, 3, 2, 6, 2, 1, 2, 290, 16, 0, 30, 2, 3, 0, 15, 3, 9, 395, 2309, 106, 6, 12, 4, 8, 8, 9, 5991, 84, 2, 70, 2, 1, 3, 0, 3, 1, 3, 3, 2, 11, 2, 0, 2, 6, 2, 64, 2, 3, 3, 7, 2, 6, 2, 27, 2, 3, 2, 4, 2, 0, 4, 6, 2, 339, 3, 24, 2, 24, 2, 30, 2, 24, 2, 30, 2, 24, 2, 30, 2, 24, 2, 30, 2, 24, 2, 7, 1845, 30, 7, 5, 262, 61, 147, 44, 11, 6, 17, 0, 322, 29, 19, 43, 485, 27, 229, 29, 3, 0, 208, 30, 2, 2, 2, 1, 2, 6, 3, 4, 10, 1, 225, 6, 2, 3, 2, 1, 2, 14, 2, 196, 60, 67, 8, 0, 1205, 3, 2, 26, 2, 1, 2, 0, 3, 0, 2, 9, 2, 3, 2, 0, 2, 0, 7, 0, 5, 0, 2, 0, 2, 0, 2, 2, 2, 1, 2, 0, 3, 0, 2, 0, 2, 0, 2, 0, 2, 0, 2, 1, 2, 0, 3, 3, 2, 6, 2, 3, 2, 3, 2, 0, 2, 9, 2, 16, 6, 2, 2, 4, 2, 16, 4421, 42719, 33, 4381, 3, 5773, 3, 7472, 16, 621, 2467, 541, 1507, 4938, 6, 8489];
    nonASCIIidentifierChars = "‌‍·̀-ͯ·҃-֑҇-ׇֽֿׁׂׅׄؐ-ًؚ-٩ٰۖ-ۜ۟-۪ۤۧۨ-ۭ۰-۹ܑܰ-݊ަ-ް߀-߉߫-߽߳ࠖ-࠙ࠛ-ࠣࠥ-ࠧࠩ-࡙࠭-࡛ࢗ-࢟࣊-ࣣ࣡-ःऺ-़ा-ॏ॑-ॗॢॣ०-९ঁ-ঃ়া-ৄেৈো-্ৗৢৣ০-৯৾ਁ-ਃ਼ਾ-ੂੇੈੋ-੍ੑ੦-ੱੵઁ-ઃ઼ા-ૅે-ૉો-્ૢૣ૦-૯ૺ-૿ଁ-ଃ଼ା-ୄେୈୋ-୍୕-ୗୢୣ୦-୯ஂா-ூெ-ைொ-்ௗ௦-௯ఀ-ఄ఼ా-ౄె-ైొ-్ౕౖౢౣ౦-౯ಁ-ಃ಼ಾ-ೄೆ-ೈೊ-್ೕೖೢೣ೦-೯ೳഀ-ഃ഻഼ാ-ൄെ-ൈൊ-്ൗൢൣ൦-൯ඁ-ඃ්ා-ුූෘ-ෟ෦-෯ෲෳัิ-ฺ็-๎๐-๙ັິ-ຼ່-໎໐-໙༘༙༠-༩༹༵༷༾༿ཱ-྄྆྇ྍ-ྗྙ-ྼ࿆ါ-ှ၀-၉ၖ-ၙၞ-ၠၢ-ၤၧ-ၭၱ-ၴႂ-ႍႏ-ႝ፝-፟፩-፱ᜒ-᜕ᜲ-᜴ᝒᝓᝲᝳ឴-៓៝០-៩᠋-᠍᠏-᠙ᢩᤠ-ᤫᤰ-᤻᥆-᥏᧐-᧚ᨗ-ᨛᩕ-ᩞ᩠-᩿᩼-᪉᪐-᪙᪰-᪽ᪿ-᫝᫠-᫫ᬀ-ᬄ᬴-᭄᭐-᭙᭫-᭳ᮀ-ᮂᮡ-ᮭ᮰-᮹᯦-᯳ᰤ-᰷᱀-᱉᱐-᱙᳐-᳔᳒-᳨᳭᳴᳷-᳹᷀-᷿‌‍‿⁀⁔⃐-⃥⃜⃡-⃰⳯-⵿⳱ⷠ-〪ⷿ-゙゚〯・꘠-꘩꙯ꙴ-꙽ꚞꚟ꛰꛱ꠂ꠆ꠋꠣ-ꠧ꠬ꢀꢁꢴ-ꣅ꣐-꣙꣠-꣱ꣿ-꤉ꤦ-꤭ꥇ-꥓ꦀ-ꦃ꦳-꧀꧐-꧙ꧥ꧰-꧹ꨩ-ꨶꩃꩌꩍ꩐-꩙ꩻ-ꩽꪰꪲ-ꪴꪷꪸꪾ꪿꫁ꫫ-ꫯꫵ꫶ꯣ-ꯪ꯬꯭꯰-꯹ﬞ︀-️︠-︯︳︴﹍-﹏０-９＿･";
    nonASCIIidentifierStartChars = "ªµºÀ-ÖØ-öø-ˁˆ-ˑˠ-ˤˬˮͰ-ʹͶͷͺ-ͽͿΆΈ-ΊΌΎ-ΡΣ-ϵϷ-ҁҊ-ԯԱ-Ֆՙՠ-ֈא-תׯ-ײؠ-يٮٯٱ-ۓەۥۦۮۯۺ-ۼۿܐܒ-ܯݍ-ޥޱߊ-ߪߴߵߺࠀ-ࠕࠚࠤࠨࡀ-ࡘࡠ-ࡪࡰ-ࢇࢉ-࢏ࢠ-ࣉऄ-हऽॐक़-ॡॱ-ঀঅ-ঌএঐও-নপ-রলশ-হঽৎড়ঢ়য়-ৡৰৱৼਅ-ਊਏਐਓ-ਨਪ-ਰਲਲ਼ਵਸ਼ਸਹਖ਼-ੜਫ਼ੲ-ੴઅ-ઍએ-ઑઓ-નપ-રલળવ-હઽૐૠૡૹଅ-ଌଏଐଓ-ନପ-ରଲଳଵ-ହଽଡ଼ଢ଼ୟ-ୡୱஃஅ-ஊஎ-ஐஒ-கஙசஜஞடணதந-பம-ஹௐఅ-ఌఎ-ఐఒ-నప-హఽౘ-ౚ౜ౝౠౡಀಅ-ಌಎ-ಐಒ-ನಪ-ಳವ-ಹಽ೜-ೞೠೡೱೲഄ-ഌഎ-ഐഒ-ഺഽൎൔ-ൖൟ-ൡൺ-ൿඅ-ඖක-නඳ-රලව-ෆก-ะาำเ-ๆກຂຄຆ-ຊຌ-ຣລວ-ະາຳຽເ-ໄໆໜ-ໟༀཀ-ཇཉ-ཬྈ-ྌက-ဪဿၐ-ၕၚ-ၝၡၥၦၮ-ၰၵ-ႁႎႠ-ჅჇჍა-ჺჼ-ቈቊ-ቍቐ-ቖቘቚ-ቝበ-ኈኊ-ኍነ-ኰኲ-ኵኸ-ኾዀዂ-ዅወ-ዖዘ-ጐጒ-ጕጘ-ፚᎀ-ᎏᎠ-Ᏽᏸ-ᏽᐁ-ᙬᙯ-ᙿᚁ-ᚚᚠ-ᛪᛮ-ᛸᜀ-ᜑᜟ-ᜱᝀ-ᝑᝠ-ᝬᝮ-ᝰក-ឳៗៜᠠ-ᡸᢀ-ᢨᢪᢰ-ᣵᤀ-ᤞᥐ-ᥭᥰ-ᥴᦀ-ᦫᦰ-ᧉᨀ-ᨖᨠ-ᩔᪧᬅ-ᬳᭅ-ᭌᮃ-ᮠᮮᮯᮺ-ᯥᰀ-ᰣᱍ-ᱏᱚ-ᱽᲀ-ᲊᲐ-ᲺᲽ-Ჿᳩ-ᳬᳮ-ᳳᳵᳶᳺᴀ-ᶿḀ-ἕἘ-Ἕἠ-ὅὈ-Ὅὐ-ὗὙὛὝὟ-ώᾀ-ᾴᾶ-ᾼιῂ-ῄῆ-ῌῐ-ΐῖ-Ίῠ-Ῥῲ-ῴῶ-ῼⁱⁿₐ-ₜℂℇℊ-ℓℕ℘-ℝℤΩℨK-ℹℼ-ℿⅅ-ⅉⅎⅠ-ↈⰀ-ⳤⳫ-ⳮⳲⳳⴀ-ⴥⴧⴭⴰ-ⵧⵯⶀ-ⶖⶠ-ⶦⶨ-ⶮⶰ-ⶶⶸ-ⶾⷀ-ⷆⷈ-ⷎⷐ-ⷖⷘ-ⷞ々-〇〡-〩〱-〵〸-〼ぁ-ゖ゛-ゟァ-ヺー-ヿㄅ-ㄯㄱ-ㆎㆠ-ㆿㇰ-ㇿ㐀-䶿一-ꒌꓐ-ꓽꔀ-ꘌꘐ-ꘟꘪꘫꙀ-ꙮꙿ-ꚝꚠ-ꛯꜗ-ꜟꜢ-ꞈꞋ-Ƛ꟱-ꠁꠃ-ꠅꠇ-ꠊꠌ-ꠢꡀ-ꡳꢂ-ꢳꣲ-ꣷꣻꣽꣾꤊ-ꤥꤰ-ꥆꥠ-ꥼꦄ-ꦲꧏꧠ-ꧤꧦ-ꧯꧺ-ꧾꨀ-ꨨꩀ-ꩂꩄ-ꩋꩠ-ꩶꩺꩾ-ꪯꪱꪵꪶꪹ-ꪽꫀꫂꫛ-ꫝꫠ-ꫪꫲ-ꫴꬁ-ꬆꬉ-ꬎꬑ-ꬖꬠ-ꬦꬨ-ꬮꬰ-ꭚꭜ-ꭩꭰ-ꯢ가-힣ힰ-ퟆퟋ-ퟻ豈-舘並-龎ﬀ-ﬆﬓ-ﬗיִײַ-ﬨשׁ-זּטּ-לּמּנּסּףּפּצּ-ﮱﯓ-ﴽﵐ-ﶏﶒ-ﷇﷰ-ﷻﹰ-ﹴﹶ-ﻼＡ-Ｚａ-ｚｦ-ﾾￂ-ￇￊ-ￏￒ-ￗￚ-ￜ";
    reservedWords = {
      3: "abstract boolean byte char class double enum export extends final float goto implements import int interface long native package private protected public short static super synchronized throws transient volatile",
      5: "class enum extends super const export import",
      6: "enum",
      strict: "implements interface let package private protected public static yield",
      strictBind: "eval arguments"
    };
    ecma5AndLessKeywords = "break case catch continue debugger default do else finally for function if return switch throw try var while with null true false instanceof typeof void delete new in this";
    keywords$1 = {
      5: ecma5AndLessKeywords,
      "5module": ecma5AndLessKeywords + " export import",
      6: ecma5AndLessKeywords + " const class extends export import super"
    };
    keywordRelationalOperator = /^in(stanceof)?$/;
    nonASCIIidentifierStart = new RegExp("[" + nonASCIIidentifierStartChars + "]");
    nonASCIIidentifier = new RegExp("[" + nonASCIIidentifierStartChars + nonASCIIidentifierChars + "]");
    TokenType = function TokenType2(label, conf) {
      if (conf === void 0) conf = {};
      this.label = label;
      this.keyword = conf.keyword;
      this.beforeExpr = !!conf.beforeExpr;
      this.startsExpr = !!conf.startsExpr;
      this.isLoop = !!conf.isLoop;
      this.isAssign = !!conf.isAssign;
      this.prefix = !!conf.prefix;
      this.postfix = !!conf.postfix;
      this.binop = conf.binop || null;
      this.updateContext = null;
    };
    beforeExpr = { beforeExpr: true };
    startsExpr = { startsExpr: true };
    keywords = {};
    types$1 = {
      num: new TokenType("num", startsExpr),
      regexp: new TokenType("regexp", startsExpr),
      string: new TokenType("string", startsExpr),
      name: new TokenType("name", startsExpr),
      privateId: new TokenType("privateId", startsExpr),
      eof: new TokenType("eof"),
      // Punctuation token types.
      bracketL: new TokenType("[", { beforeExpr: true, startsExpr: true }),
      bracketR: new TokenType("]"),
      braceL: new TokenType("{", { beforeExpr: true, startsExpr: true }),
      braceR: new TokenType("}"),
      parenL: new TokenType("(", { beforeExpr: true, startsExpr: true }),
      parenR: new TokenType(")"),
      comma: new TokenType(",", beforeExpr),
      semi: new TokenType(";", beforeExpr),
      colon: new TokenType(":", beforeExpr),
      dot: new TokenType("."),
      question: new TokenType("?", beforeExpr),
      questionDot: new TokenType("?."),
      arrow: new TokenType("=>", beforeExpr),
      template: new TokenType("template"),
      invalidTemplate: new TokenType("invalidTemplate"),
      ellipsis: new TokenType("...", beforeExpr),
      backQuote: new TokenType("`", startsExpr),
      dollarBraceL: new TokenType("${", { beforeExpr: true, startsExpr: true }),
      // Operators. These carry several kinds of properties to help the
      // parser use them properly (the presence of these properties is
      // what categorizes them as operators).
      //
      // `binop`, when present, specifies that this operator is a binary
      // operator, and will refer to its precedence.
      //
      // `prefix` and `postfix` mark the operator as a prefix or postfix
      // unary operator.
      //
      // `isAssign` marks all of `=`, `+=`, `-=` etcetera, which act as
      // binary operators with a very low precedence, that should result
      // in AssignmentExpression nodes.
      eq: new TokenType("=", { beforeExpr: true, isAssign: true }),
      assign: new TokenType("_=", { beforeExpr: true, isAssign: true }),
      incDec: new TokenType("++/--", { prefix: true, postfix: true, startsExpr: true }),
      prefix: new TokenType("!/~", { beforeExpr: true, prefix: true, startsExpr: true }),
      logicalOR: binop("||", 1),
      logicalAND: binop("&&", 2),
      bitwiseOR: binop("|", 3),
      bitwiseXOR: binop("^", 4),
      bitwiseAND: binop("&", 5),
      equality: binop("==/!=/===/!==", 6),
      relational: binop("</>/<=/>=", 7),
      bitShift: binop("<</>>/>>>", 8),
      plusMin: new TokenType("+/-", { beforeExpr: true, binop: 9, prefix: true, startsExpr: true }),
      modulo: binop("%", 10),
      star: binop("*", 10),
      slash: binop("/", 10),
      starstar: new TokenType("**", { beforeExpr: true }),
      coalesce: binop("??", 1),
      // Keyword token types.
      _break: kw("break"),
      _case: kw("case", beforeExpr),
      _catch: kw("catch"),
      _continue: kw("continue"),
      _debugger: kw("debugger"),
      _default: kw("default", beforeExpr),
      _do: kw("do", { isLoop: true, beforeExpr: true }),
      _else: kw("else", beforeExpr),
      _finally: kw("finally"),
      _for: kw("for", { isLoop: true }),
      _function: kw("function", startsExpr),
      _if: kw("if"),
      _return: kw("return", beforeExpr),
      _switch: kw("switch"),
      _throw: kw("throw", beforeExpr),
      _try: kw("try"),
      _var: kw("var"),
      _const: kw("const"),
      _while: kw("while", { isLoop: true }),
      _with: kw("with"),
      _new: kw("new", { beforeExpr: true, startsExpr: true }),
      _this: kw("this", startsExpr),
      _super: kw("super", startsExpr),
      _class: kw("class", startsExpr),
      _extends: kw("extends", beforeExpr),
      _export: kw("export"),
      _import: kw("import", startsExpr),
      _null: kw("null", startsExpr),
      _true: kw("true", startsExpr),
      _false: kw("false", startsExpr),
      _in: kw("in", { beforeExpr: true, binop: 7 }),
      _instanceof: kw("instanceof", { beforeExpr: true, binop: 7 }),
      _typeof: kw("typeof", { beforeExpr: true, prefix: true, startsExpr: true }),
      _void: kw("void", { beforeExpr: true, prefix: true, startsExpr: true }),
      _delete: kw("delete", { beforeExpr: true, prefix: true, startsExpr: true })
    };
    lineBreak = /\r\n?|\n|\u2028|\u2029/;
    lineBreakG = new RegExp(lineBreak.source, "g");
    nonASCIIwhitespace = /[\u1680\u2000-\u200a\u202f\u205f\u3000\ufeff]/;
    skipWhiteSpace = /(?:\s|\/\/.*|\/\*[^]*?\*\/)*/g;
    ref = Object.prototype;
    hasOwnProperty = ref.hasOwnProperty;
    toString = ref.toString;
    hasOwn = Object.hasOwn || (function(obj, propName) {
      return hasOwnProperty.call(obj, propName);
    });
    isArray = Array.isArray || (function(obj) {
      return toString.call(obj) === "[object Array]";
    });
    regexpCache = /* @__PURE__ */ Object.create(null);
    loneSurrogate = /(?:[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?:[^\uD800-\uDBFF]|^)[\uDC00-\uDFFF])/;
    Position = function Position2(line, col) {
      this.line = line;
      this.column = col;
    };
    Position.prototype.offset = function offset(n) {
      return new Position(this.line, this.column + n);
    };
    SourceLocation = function SourceLocation2(p, start, end) {
      this.start = start;
      this.end = end;
      if (p.sourceFile !== null) {
        this.source = p.sourceFile;
      }
    };
    defaultOptions = {
      // `ecmaVersion` indicates the ECMAScript version to parse. Must be
      // either 3, 5, 6 (or 2015), 7 (2016), 8 (2017), 9 (2018), 10
      // (2019), 11 (2020), 12 (2021), 13 (2022), 14 (2023), or `"latest"`
      // (the latest version the library supports). This influences
      // support for strict mode, the set of reserved words, and support
      // for new syntax features.
      ecmaVersion: null,
      // `sourceType` indicates the mode the code should be parsed in.
      // Can be either `"script"`, `"module"` or `"commonjs"`. This influences global
      // strict mode and parsing of `import` and `export` declarations.
      sourceType: "script",
      // When set to true, enable strict parsing mode even if `sourceType`
      // is `"script"`.
      strict: false,
      // `onInsertedSemicolon` can be a callback that will be called when
      // a semicolon is automatically inserted. It will be passed the
      // position of the inserted semicolon as an offset, and if
      // `locations` is enabled, it is given the location as a `{line,
      // column}` object as second argument.
      onInsertedSemicolon: null,
      // `onTrailingComma` is similar to `onInsertedSemicolon`, but for
      // trailing commas.
      onTrailingComma: null,
      // By default, reserved words are only enforced if ecmaVersion >= 5.
      // Set `allowReserved` to a boolean value to explicitly turn this on
      // an off. When this option has the value "never", reserved words
      // and keywords can also not be used as property names.
      allowReserved: null,
      // When enabled, a return at the top level is not considered an
      // error.
      allowReturnOutsideFunction: false,
      // When enabled, import/export statements are not constrained to
      // appearing at the top of the program, and an import.meta expression
      // in a script isn't considered an error.
      allowImportExportEverywhere: false,
      // By default, await identifiers are allowed to appear at the top-level scope only if ecmaVersion >= 2022.
      // When enabled, await identifiers are allowed to appear at the top-level scope,
      // but they are still not allowed in non-async functions.
      allowAwaitOutsideFunction: null,
      // When enabled, super identifiers are not constrained to
      // appearing in methods and do not raise an error when they appear elsewhere.
      allowSuperOutsideMethod: null,
      // When enabled, hashbang directive in the beginning of file is
      // allowed and treated as a line comment. Enabled by default when
      // `ecmaVersion` >= 2023.
      allowHashBang: false,
      // By default, the parser will verify that private properties are
      // only used in places where they are valid and have been declared.
      // Set this to false to turn such checks off.
      checkPrivateFields: true,
      // When `locations` is on, `loc` properties holding objects with
      // `start` and `end` properties in `{line, column}` form (with
      // line being 1-based and column 0-based) will be attached to the
      // nodes.
      locations: false,
      // Pass an optional `{line, column}` object to use for the start of
      // the parse. This is mostly useful when using `parseExpressionAt`
      // with `locations: true`, to prevent the parser from having to
      // determine the line position at the start position.
      startLocation: null,
      // A function can be passed as `onToken` option, which will
      // cause Acorn to call that function with object in the same
      // format as tokens returned from `tokenizer().getToken()`. Note
      // that you are not allowed to call the parser from the
      // callback—that will corrupt its internal state.
      onToken: null,
      // A function can be passed as `onComment` option, which will
      // cause Acorn to call that function with `(block, text, start,
      // end)` parameters whenever a comment is skipped. `block` is a
      // boolean indicating whether this is a block (`/* */`) comment,
      // `text` is the content of the comment, and `start` and `end` are
      // character offsets that denote the start and end of the comment.
      // When the `locations` option is on, two more parameters are
      // passed, the full `{line, column}` locations of the start and
      // end of the comments. Note that you are not allowed to call the
      // parser from the callback—that will corrupt its internal state.
      // When this option has an array as value, objects representing the
      // comments are pushed to it.
      onComment: null,
      // Nodes have their start and end characters offsets recorded in
      // `start` and `end` properties (directly on the node, rather than
      // the `loc` object, which holds line/column data. To also add a
      // [semi-standardized][range] `range` property holding a `[start,
      // end]` array with the same numbers, set the `ranges` option to
      // `true`.
      //
      // [range]: https://bugzilla.mozilla.org/show_bug.cgi?id=745678
      ranges: false,
      // It is possible to parse multiple files into a single AST by
      // passing the tree produced by parsing the first file as
      // `program` option in subsequent parses. This will add the
      // toplevel forms of the parsed file to the `Program` (top) node
      // of an existing parse tree.
      program: null,
      // When `locations` is on, you can pass this to record the source
      // file in every node's `loc` object.
      sourceFile: null,
      // This value, if given, is stored in every node, whether
      // `locations` is on or off.
      directSourceFile: null,
      // When enabled, parenthesized expressions are represented by
      // (non-standard) ParenthesizedExpression nodes
      preserveParens: false
    };
    warnedAboutEcmaVersion = false;
    SCOPE_TOP = 1;
    SCOPE_FUNCTION = 2;
    SCOPE_ASYNC = 4;
    SCOPE_GENERATOR = 8;
    SCOPE_ARROW = 16;
    SCOPE_SIMPLE_CATCH = 32;
    SCOPE_SUPER = 64;
    SCOPE_DIRECT_SUPER = 128;
    SCOPE_CLASS_STATIC_BLOCK = 256;
    SCOPE_CLASS_FIELD_INIT = 512;
    SCOPE_SWITCH = 1024;
    SCOPE_VAR = SCOPE_TOP | SCOPE_FUNCTION | SCOPE_CLASS_STATIC_BLOCK;
    BIND_NONE = 0;
    BIND_VAR = 1;
    BIND_LEXICAL = 2;
    BIND_FUNCTION = 3;
    BIND_SIMPLE_CATCH = 4;
    BIND_OUTSIDE = 5;
    Parser = function Parser2(options, input, startPos) {
      this.options = options = getOptions(options);
      this.sourceFile = options.sourceFile;
      this.keywords = wordsRegexp(keywords$1[options.ecmaVersion >= 6 ? 6 : options.sourceType === "module" ? "5module" : 5]);
      var reserved = "";
      if (options.allowReserved !== true) {
        reserved = reservedWords[options.ecmaVersion >= 6 ? 6 : options.ecmaVersion === 5 ? 5 : 3];
        if (options.sourceType === "module") {
          reserved += " await";
        }
      }
      this.reservedWords = wordsRegexp(reserved);
      var reservedStrict = (reserved ? reserved + " " : "") + reservedWords.strict;
      this.reservedWordsStrict = wordsRegexp(reservedStrict);
      this.reservedWordsStrictBind = wordsRegexp(reservedStrict + " " + reservedWords.strictBind);
      this.input = String(input);
      this.containsEsc = false;
      this.pos = startPos || 0;
      this.curLine = 1;
      if (options.startLocation) {
        this.lineStart = this.pos - options.startLocation.column;
        this.curLine = options.startLocation.line;
      } else if (startPos) {
        this.lineStart = this.input.lastIndexOf("\n", startPos - 1) + 1;
        if (this.options.locations) {
          this.curLine = this.input.slice(0, this.lineStart).split(lineBreak).length;
        }
      } else {
        this.lineStart = 0;
      }
      this.type = types$1.eof;
      this.value = null;
      this.start = this.end = this.pos;
      this.startLoc = this.endLoc = this.curPosition();
      this.lastTokEndLoc = this.lastTokStartLoc = null;
      this.lastTokStart = this.lastTokEnd = this.pos;
      this.context = this.initialContext();
      this.exprAllowed = true;
      this.inModule = options.sourceType === "module";
      this.strict = this.inModule || options.strict === true || this.strictDirective(this.pos);
      this.potentialArrowAt = -1;
      this.potentialArrowInForAwait = false;
      this.yieldPos = this.awaitPos = this.awaitIdentPos = 0;
      this.labels = [];
      this.undefinedExports = /* @__PURE__ */ Object.create(null);
      if (this.pos === 0 && options.allowHashBang && this.input.slice(0, 2) === "#!") {
        this.skipLineComment(2);
      }
      this.scopeStack = [];
      this.enterScope(
        this.options.sourceType === "commonjs" ? SCOPE_FUNCTION : SCOPE_TOP
      );
      this.regexpState = null;
      this.privateNameStack = [];
    };
    prototypeAccessors = { inFunction: { configurable: true }, inGenerator: { configurable: true }, inAsync: { configurable: true }, canAwait: { configurable: true }, allowReturn: { configurable: true }, allowSuper: { configurable: true }, allowDirectSuper: { configurable: true }, treatFunctionsAsVar: { configurable: true }, allowNewDotTarget: { configurable: true }, allowUsing: { configurable: true }, inClassStaticBlock: { configurable: true } };
    Parser.prototype.parse = function parse() {
      var this$1$1 = this;
      var node = this.options.program || this.startNode();
      this.nextToken();
      return this.catchStackOverflow(function() {
        return this$1$1.parseTopLevel(node);
      });
    };
    prototypeAccessors.inFunction.get = function() {
      return (this.currentVarScope().flags & SCOPE_FUNCTION) > 0;
    };
    prototypeAccessors.inGenerator.get = function() {
      return (this.currentVarScope().flags & SCOPE_GENERATOR) > 0;
    };
    prototypeAccessors.inAsync.get = function() {
      return (this.currentVarScope().flags & SCOPE_ASYNC) > 0;
    };
    prototypeAccessors.canAwait.get = function() {
      for (var i2 = this.scopeStack.length - 1; i2 >= 0; i2--) {
        var ref2 = this.scopeStack[i2];
        var flags = ref2.flags;
        if (flags & (SCOPE_CLASS_STATIC_BLOCK | SCOPE_CLASS_FIELD_INIT)) {
          return false;
        }
        if (flags & SCOPE_FUNCTION) {
          return (flags & SCOPE_ASYNC) > 0;
        }
      }
      return this.inModule && this.options.ecmaVersion >= 13 || this.options.allowAwaitOutsideFunction;
    };
    prototypeAccessors.allowReturn.get = function() {
      if (this.inFunction) {
        return true;
      }
      if (this.options.allowReturnOutsideFunction && this.currentVarScope().flags & SCOPE_TOP) {
        return true;
      }
      return false;
    };
    prototypeAccessors.allowSuper.get = function() {
      var ref2 = this.currentThisScope();
      var flags = ref2.flags;
      return (flags & SCOPE_SUPER) > 0 || this.options.allowSuperOutsideMethod;
    };
    prototypeAccessors.allowDirectSuper.get = function() {
      return (this.currentThisScope().flags & SCOPE_DIRECT_SUPER) > 0;
    };
    prototypeAccessors.treatFunctionsAsVar.get = function() {
      return this.treatFunctionsAsVarInScope(this.currentScope());
    };
    prototypeAccessors.allowNewDotTarget.get = function() {
      for (var i2 = this.scopeStack.length - 1; i2 >= 0; i2--) {
        var ref2 = this.scopeStack[i2];
        var flags = ref2.flags;
        if (flags & (SCOPE_CLASS_STATIC_BLOCK | SCOPE_CLASS_FIELD_INIT) || flags & SCOPE_FUNCTION && !(flags & SCOPE_ARROW)) {
          return true;
        }
      }
      return false;
    };
    prototypeAccessors.allowUsing.get = function() {
      var ref2 = this.currentScope();
      var flags = ref2.flags;
      if (flags & SCOPE_SWITCH) {
        return false;
      }
      if (!this.inModule && flags & SCOPE_TOP) {
        return false;
      }
      return true;
    };
    prototypeAccessors.inClassStaticBlock.get = function() {
      return (this.currentVarScope().flags & SCOPE_CLASS_STATIC_BLOCK) > 0;
    };
    Parser.extend = function extend() {
      var plugins = [], len = arguments.length;
      while (len--) plugins[len] = arguments[len];
      var cls = this;
      for (var i2 = 0; i2 < plugins.length; i2++) {
        cls = plugins[i2](cls);
      }
      return cls;
    };
    Parser.parse = function parse2(input, options) {
      return new this(options, input).parse();
    };
    Parser.parseExpressionAt = function parseExpressionAt(input, pos, options) {
      var parser = new this(options, input, pos);
      parser.nextToken();
      return parser.parseExpression();
    };
    Parser.tokenizer = function tokenizer(input, options) {
      return new this(options, input);
    };
    Object.defineProperties(Parser.prototype, prototypeAccessors);
    pp$9 = Parser.prototype;
    literal = /^(?:'((?:\\[^]|[^'\\])*?)'|"((?:\\[^]|[^"\\])*?)")/;
    pp$9.strictDirective = function(start) {
      if (this.options.ecmaVersion < 5) {
        return false;
      }
      for (; ; ) {
        skipWhiteSpace.lastIndex = start;
        start += skipWhiteSpace.exec(this.input)[0].length;
        var match = literal.exec(this.input.slice(start));
        if (!match) {
          return false;
        }
        if ((match[1] || match[2]) === "use strict") {
          skipWhiteSpace.lastIndex = start + match[0].length;
          var spaceAfter = skipWhiteSpace.exec(this.input), end = spaceAfter.index + spaceAfter[0].length;
          var next = this.input.charAt(end);
          return next === ";" || next === "}" || lineBreak.test(spaceAfter[0]) && !(/[(`.[+\-/*%<>=,?^&]/.test(next) || next === "!" && this.input.charAt(end + 1) === "=");
        }
        start += match[0].length;
        skipWhiteSpace.lastIndex = start;
        start += skipWhiteSpace.exec(this.input)[0].length;
        if (this.input[start] === ";") {
          start++;
        }
      }
    };
    pp$9.eat = function(type2) {
      if (this.type === type2) {
        this.next();
        return true;
      } else {
        return false;
      }
    };
    pp$9.isContextual = function(name) {
      return this.type === types$1.name && this.value === name && !this.containsEsc;
    };
    pp$9.eatContextual = function(name) {
      if (!this.isContextual(name)) {
        return false;
      }
      this.next();
      return true;
    };
    pp$9.catchStackOverflow = function(f) {
      try {
        return f();
      } catch (e) {
        if (e instanceof Error && (/\bstack\b.*\b(exceeded|overflow)\b/i.test(e.message) || /\btoo much recursion\b/i.test(e.message))) {
          this.raise(this.start, "Not enough stack space to parse input");
        } else {
          throw e;
        }
      }
    };
    pp$9.expectContextual = function(name) {
      if (!this.eatContextual(name)) {
        this.unexpected();
      }
    };
    pp$9.canInsertSemicolon = function() {
      return this.type === types$1.eof || this.type === types$1.braceR || lineBreak.test(this.input.slice(this.lastTokEnd, this.start));
    };
    pp$9.insertSemicolon = function() {
      if (this.canInsertSemicolon()) {
        if (this.options.onInsertedSemicolon) {
          this.options.onInsertedSemicolon(this.lastTokEnd, this.lastTokEndLoc);
        }
        return true;
      }
    };
    pp$9.semicolon = function() {
      if (!this.eat(types$1.semi) && !this.insertSemicolon()) {
        this.unexpected();
      }
    };
    pp$9.afterTrailingComma = function(tokType, notNext) {
      if (this.type === tokType) {
        if (this.options.onTrailingComma) {
          this.options.onTrailingComma(this.lastTokStart, this.lastTokStartLoc);
        }
        if (!notNext) {
          this.next();
        }
        return true;
      }
    };
    pp$9.expect = function(type2) {
      this.eat(type2) || this.unexpected();
    };
    pp$9.unexpected = function(pos) {
      this.raise(pos != null ? pos : this.start, "Unexpected token");
    };
    DestructuringErrors = function DestructuringErrors2() {
      this.shorthandAssign = this.trailingComma = this.parenthesizedAssign = this.parenthesizedBind = this.doubleProto = -1;
    };
    pp$9.checkPatternErrors = function(refDestructuringErrors, isAssign) {
      if (!refDestructuringErrors) {
        return;
      }
      if (refDestructuringErrors.trailingComma > -1) {
        this.raiseRecoverable(refDestructuringErrors.trailingComma, "Comma is not permitted after the rest element");
      }
      var parens = isAssign ? refDestructuringErrors.parenthesizedAssign : refDestructuringErrors.parenthesizedBind;
      if (parens > -1) {
        this.raiseRecoverable(parens, isAssign ? "Assigning to rvalue" : "Parenthesized pattern");
      }
    };
    pp$9.checkExpressionErrors = function(refDestructuringErrors, andThrow) {
      if (!refDestructuringErrors) {
        return false;
      }
      var shorthandAssign = refDestructuringErrors.shorthandAssign;
      var doubleProto = refDestructuringErrors.doubleProto;
      if (!andThrow) {
        return shorthandAssign >= 0 || doubleProto >= 0;
      }
      if (shorthandAssign >= 0) {
        this.raise(shorthandAssign, "Shorthand property assignments are valid only in destructuring patterns");
      }
      if (doubleProto >= 0) {
        this.raiseRecoverable(doubleProto, "Redefinition of __proto__ property");
      }
    };
    pp$9.checkYieldAwaitInDefaultParams = function() {
      if (this.yieldPos && (!this.awaitPos || this.yieldPos < this.awaitPos)) {
        this.raise(this.yieldPos, "Yield expression cannot be a default value");
      }
      if (this.awaitPos) {
        this.raise(this.awaitPos, "Await expression cannot be a default value");
      }
    };
    pp$9.isSimpleAssignTarget = function(expr) {
      if (expr.type === "ParenthesizedExpression") {
        return this.isSimpleAssignTarget(expr.expression);
      }
      return expr.type === "Identifier" || expr.type === "MemberExpression";
    };
    pp$8 = Parser.prototype;
    pp$8.parseTopLevel = function(node) {
      var exports$1 = /* @__PURE__ */ Object.create(null);
      if (!node.body) {
        node.body = [];
      }
      while (this.type !== types$1.eof) {
        var stmt = this.parseStatement(null, true, exports$1);
        node.body.push(stmt);
      }
      if (this.inModule) {
        for (var i2 = 0, list2 = Object.keys(this.undefinedExports); i2 < list2.length; i2 += 1) {
          var name = list2[i2];
          this.raiseRecoverable(this.undefinedExports[name].start, "Export '" + name + "' is not defined");
        }
      }
      this.adaptDirectivePrologue(node.body);
      this.next();
      node.sourceType = this.options.sourceType === "commonjs" ? "script" : this.options.sourceType;
      return this.finishNode(node, "Program");
    };
    loopLabel = { kind: "loop" };
    switchLabel = { kind: "switch" };
    pp$8.isLet = function(context) {
      if (this.options.ecmaVersion < 6 || !this.isContextual("let")) {
        return false;
      }
      skipWhiteSpace.lastIndex = this.pos;
      var skip = skipWhiteSpace.exec(this.input);
      var next = this.pos + skip[0].length, nextCh = this.fullCharCodeAt(next);
      if (nextCh === 91 || nextCh === 92) {
        return true;
      }
      if (context) {
        return false;
      }
      if (nextCh === 123) {
        return true;
      }
      if (isIdentifierStart(nextCh)) {
        var start = next;
        do {
          next += nextCh <= 65535 ? 1 : 2;
        } while (isIdentifierChar(nextCh = this.fullCharCodeAt(next)));
        if (nextCh === 92) {
          return true;
        }
        var ident = this.input.slice(start, next);
        if (!keywordRelationalOperator.test(ident)) {
          return true;
        }
      }
      return false;
    };
    pp$8.isAsyncFunction = function() {
      if (this.options.ecmaVersion < 8 || !this.isContextual("async")) {
        return false;
      }
      skipWhiteSpace.lastIndex = this.pos;
      var skip = skipWhiteSpace.exec(this.input);
      var next = this.pos + skip[0].length, after;
      return !lineBreak.test(this.input.slice(this.pos, next)) && this.input.slice(next, next + 8) === "function" && (next + 8 === this.input.length || !(isIdentifierChar(after = this.fullCharCodeAt(next + 8)) || after === 92));
    };
    pp$8.isUsingKeyword = function(isAwaitUsing, isFor) {
      if (this.options.ecmaVersion < 17 || !this.isContextual(isAwaitUsing ? "await" : "using")) {
        return false;
      }
      skipWhiteSpace.lastIndex = this.pos;
      var skip = skipWhiteSpace.exec(this.input);
      var next = this.pos + skip[0].length;
      if (lineBreak.test(this.input.slice(this.pos, next))) {
        return false;
      }
      if (isAwaitUsing) {
        var usingEndPos = next + 5, after;
        if (this.input.slice(next, usingEndPos) !== "using" || usingEndPos === this.input.length || isIdentifierChar(after = this.fullCharCodeAt(usingEndPos)) || after === 92) {
          return false;
        }
        skipWhiteSpace.lastIndex = usingEndPos;
        var skipAfterUsing = skipWhiteSpace.exec(this.input);
        next = usingEndPos + skipAfterUsing[0].length;
        if (skipAfterUsing && lineBreak.test(this.input.slice(usingEndPos, next))) {
          return false;
        }
      }
      var ch = this.fullCharCodeAt(next);
      if (!isIdentifierStart(ch) && ch !== 92) {
        return false;
      }
      var idStart = next;
      do {
        next += ch <= 65535 ? 1 : 2;
      } while (isIdentifierChar(ch = this.fullCharCodeAt(next)));
      if (ch === 92) {
        return true;
      }
      var id = this.input.slice(idStart, next);
      if (keywordRelationalOperator.test(id)) {
        return false;
      }
      if (isFor && !isAwaitUsing && id === "of") {
        skipWhiteSpace.lastIndex = next;
        var skipAfterOf = skipWhiteSpace.exec(this.input);
        next = next + skipAfterOf[0].length;
        if (this.input.charCodeAt(next) !== 61 || // Check for ==, === and => operators
        (ch = this.input.charCodeAt(next + 1)) === 61 || ch === 62) {
          return false;
        }
      }
      return true;
    };
    pp$8.isAwaitUsing = function(isFor) {
      return this.isUsingKeyword(true, isFor);
    };
    pp$8.isUsing = function(isFor) {
      return this.isUsingKeyword(false, isFor);
    };
    pp$8.parseStatement = function(context, topLevel, exports$1) {
      var starttype = this.type, node = this.startNode(), kind;
      if (this.isLet(context)) {
        starttype = types$1._var;
        kind = "let";
      }
      switch (starttype) {
        case types$1._break:
        case types$1._continue:
          return this.parseBreakContinueStatement(node, starttype.keyword);
        case types$1._debugger:
          return this.parseDebuggerStatement(node);
        case types$1._do:
          return this.parseDoStatement(node);
        case types$1._for:
          return this.parseForStatement(node);
        case types$1._function:
          if (context && (this.strict || context !== "if" && context !== "label") && this.options.ecmaVersion >= 6) {
            this.unexpected();
          }
          return this.parseFunctionStatement(node, false, !context);
        case types$1._class:
          if (context) {
            this.unexpected();
          }
          return this.parseClass(node, true);
        case types$1._if:
          return this.parseIfStatement(node);
        case types$1._return:
          return this.parseReturnStatement(node);
        case types$1._switch:
          return this.parseSwitchStatement(node);
        case types$1._throw:
          return this.parseThrowStatement(node);
        case types$1._try:
          return this.parseTryStatement(node);
        case types$1._const:
        case types$1._var:
          kind = kind || this.value;
          if (context && kind !== "var") {
            this.unexpected();
          }
          return this.parseVarStatement(node, kind);
        case types$1._while:
          return this.parseWhileStatement(node);
        case types$1._with:
          return this.parseWithStatement(node);
        case types$1.braceL:
          return this.parseBlock(true, node);
        case types$1.semi:
          return this.parseEmptyStatement(node);
        case types$1._export:
        case types$1._import:
          if (this.options.ecmaVersion > 10 && starttype === types$1._import) {
            skipWhiteSpace.lastIndex = this.pos;
            var skip = skipWhiteSpace.exec(this.input);
            var next = this.pos + skip[0].length, nextCh = this.input.charCodeAt(next);
            if (nextCh === 40 || nextCh === 46) {
              return this.parseExpressionStatement(node, this.parseExpression());
            }
          }
          if (!this.options.allowImportExportEverywhere) {
            if (!topLevel) {
              this.raise(this.start, "'import' and 'export' may only appear at the top level");
            }
            if (!this.inModule) {
              this.raise(this.start, "'import' and 'export' may appear only with 'sourceType: module'");
            }
          }
          return starttype === types$1._import ? this.parseImport(node) : this.parseExport(node, exports$1);
        // If the statement does not start with a statement keyword or a
        // brace, it's an ExpressionStatement or LabeledStatement. We
        // simply start parsing an expression, and afterwards, if the
        // next token is a colon and the expression was a simple
        // Identifier node, we switch to interpreting it as a label.
        default:
          if (this.isAsyncFunction()) {
            if (context) {
              this.unexpected();
            }
            this.next();
            return this.parseFunctionStatement(node, true, !context);
          }
          var usingKind = this.isAwaitUsing(false) ? "await using" : this.isUsing(false) ? "using" : null;
          if (usingKind) {
            if (!this.allowUsing) {
              this.raise(this.start, "Using declaration cannot appear in the top level when source type is `script` or in the bare case statement");
            }
            if (context) {
              this.raise(this.start, "Using declaration is not allowed in single-statement positions");
            }
            if (usingKind === "await using") {
              if (!this.canAwait) {
                this.raise(this.start, "Await using cannot appear outside of async function");
              }
              this.next();
            }
            this.next();
            this.parseVar(node, false, usingKind);
            this.semicolon();
            return this.finishNode(node, "VariableDeclaration");
          }
          var maybeName = this.value, expr = this.parseExpression();
          if (starttype === types$1.name && expr.type === "Identifier" && this.eat(types$1.colon)) {
            return this.parseLabeledStatement(node, maybeName, expr, context);
          } else {
            return this.parseExpressionStatement(node, expr);
          }
      }
    };
    pp$8.parseBreakContinueStatement = function(node, keyword) {
      var isBreak = keyword === "break";
      this.next();
      if (this.eat(types$1.semi) || this.insertSemicolon()) {
        node.label = null;
      } else if (this.type !== types$1.name) {
        this.unexpected();
      } else {
        node.label = this.parseIdent();
        this.semicolon();
      }
      var i2 = 0;
      for (; i2 < this.labels.length; ++i2) {
        var lab = this.labels[i2];
        if (node.label == null || lab.name === node.label.name) {
          if (lab.kind != null && (isBreak || lab.kind === "loop")) {
            break;
          }
          if (node.label && isBreak) {
            break;
          }
        }
      }
      if (i2 === this.labels.length) {
        this.raise(node.start, "Unsyntactic " + keyword);
      }
      return this.finishNode(node, isBreak ? "BreakStatement" : "ContinueStatement");
    };
    pp$8.parseDebuggerStatement = function(node) {
      this.next();
      this.semicolon();
      return this.finishNode(node, "DebuggerStatement");
    };
    pp$8.parseDoStatement = function(node) {
      this.next();
      this.labels.push(loopLabel);
      node.body = this.parseStatement("do");
      this.labels.pop();
      this.expect(types$1._while);
      node.test = this.parseParenExpression();
      if (this.options.ecmaVersion >= 6) {
        this.eat(types$1.semi);
      } else {
        this.semicolon();
      }
      return this.finishNode(node, "DoWhileStatement");
    };
    pp$8.parseForStatement = function(node) {
      this.next();
      var awaitAt = this.options.ecmaVersion >= 9 && this.canAwait && this.eatContextual("await") ? this.lastTokStart : -1;
      this.labels.push(loopLabel);
      this.enterScope(0);
      this.expect(types$1.parenL);
      if (this.type === types$1.semi) {
        if (awaitAt > -1) {
          this.unexpected(awaitAt);
        }
        return this.parseFor(node, null);
      }
      var isLet = this.isLet();
      if (this.type === types$1._var || this.type === types$1._const || isLet) {
        var init$1 = this.startNode(), kind = isLet ? "let" : this.value;
        this.next();
        this.parseVar(init$1, true, kind);
        this.finishNode(init$1, "VariableDeclaration");
        return this.parseForAfterInit(node, init$1, awaitAt);
      }
      var startsWithLet = this.isContextual("let"), isForOf = false;
      var usingKind = this.isUsing(true) ? "using" : this.isAwaitUsing(true) ? "await using" : null;
      if (usingKind) {
        var init$2 = this.startNode();
        this.next();
        if (usingKind === "await using") {
          if (!this.canAwait) {
            this.raise(this.start, "Await using cannot appear outside of async function");
          }
          this.next();
        }
        this.parseVar(init$2, true, usingKind);
        this.finishNode(init$2, "VariableDeclaration");
        return this.parseForAfterInit(node, init$2, awaitAt);
      }
      var containsEsc = this.containsEsc;
      var refDestructuringErrors = new DestructuringErrors();
      var initPos = this.start;
      var init = awaitAt > -1 ? this.parseExprSubscripts(refDestructuringErrors, "await") : this.parseExpression(true, refDestructuringErrors);
      if (this.type === types$1._in || (isForOf = this.options.ecmaVersion >= 6 && this.isContextual("of"))) {
        if (awaitAt > -1) {
          if (this.type === types$1._in) {
            this.unexpected(awaitAt);
          }
          node.await = true;
        } else if (isForOf && this.options.ecmaVersion >= 8) {
          if (init.start === initPos && !containsEsc && init.type === "Identifier" && init.name === "async") {
            this.unexpected();
          } else if (this.options.ecmaVersion >= 9) {
            node.await = false;
          }
        }
        if (startsWithLet && isForOf) {
          this.raise(init.start, "The left-hand side of a for-of loop may not start with 'let'.");
        }
        this.toAssignable(init, false, refDestructuringErrors);
        this.checkLValPattern(init);
        return this.parseForIn(node, init);
      } else {
        this.checkExpressionErrors(refDestructuringErrors, true);
      }
      if (awaitAt > -1) {
        this.unexpected(awaitAt);
      }
      return this.parseFor(node, init);
    };
    pp$8.parseForAfterInit = function(node, init, awaitAt) {
      if ((this.type === types$1._in || this.options.ecmaVersion >= 6 && this.isContextual("of")) && init.declarations.length === 1) {
        if (this.type === types$1._in) {
          if ((init.kind === "using" || init.kind === "await using") && !init.declarations[0].init) {
            this.raise(this.start, "Using declaration is not allowed in for-in loops");
          }
          if (this.options.ecmaVersion >= 9 && awaitAt > -1) {
            this.unexpected(awaitAt);
          }
        } else if (this.options.ecmaVersion >= 9) {
          node.await = awaitAt > -1;
        }
        return this.parseForIn(node, init);
      }
      if (awaitAt > -1) {
        this.unexpected(awaitAt);
      }
      return this.parseFor(node, init);
    };
    pp$8.parseFunctionStatement = function(node, isAsync, declarationPosition) {
      this.next();
      return this.parseFunction(node, FUNC_STATEMENT | (declarationPosition ? 0 : FUNC_HANGING_STATEMENT), false, isAsync);
    };
    pp$8.parseIfStatement = function(node) {
      this.next();
      node.test = this.parseParenExpression();
      node.consequent = this.parseStatement("if");
      node.alternate = this.eat(types$1._else) ? this.parseStatement("if") : null;
      return this.finishNode(node, "IfStatement");
    };
    pp$8.parseReturnStatement = function(node) {
      if (!this.allowReturn) {
        this.raise(this.start, "'return' outside of function");
      }
      this.next();
      if (this.eat(types$1.semi) || this.insertSemicolon()) {
        node.argument = null;
      } else {
        node.argument = this.parseExpression();
        this.semicolon();
      }
      return this.finishNode(node, "ReturnStatement");
    };
    pp$8.parseSwitchStatement = function(node) {
      this.next();
      node.discriminant = this.parseParenExpression();
      node.cases = [];
      this.expect(types$1.braceL);
      this.labels.push(switchLabel);
      this.enterScope(SCOPE_SWITCH);
      var cur;
      for (var sawDefault = false; this.type !== types$1.braceR; ) {
        if (this.type === types$1._case || this.type === types$1._default) {
          var isCase = this.type === types$1._case;
          if (cur) {
            this.finishNode(cur, "SwitchCase");
          }
          node.cases.push(cur = this.startNode());
          cur.consequent = [];
          this.next();
          if (isCase) {
            cur.test = this.parseExpression();
          } else {
            if (sawDefault) {
              this.raiseRecoverable(this.lastTokStart, "Multiple default clauses");
            }
            sawDefault = true;
            cur.test = null;
          }
          this.expect(types$1.colon);
        } else {
          if (!cur) {
            this.unexpected();
          }
          cur.consequent.push(this.parseStatement(null));
        }
      }
      this.exitScope();
      if (cur) {
        this.finishNode(cur, "SwitchCase");
      }
      this.next();
      this.labels.pop();
      return this.finishNode(node, "SwitchStatement");
    };
    pp$8.parseThrowStatement = function(node) {
      this.next();
      if (lineBreak.test(this.input.slice(this.lastTokEnd, this.start))) {
        this.raise(this.lastTokEnd, "Illegal newline after throw");
      }
      node.argument = this.parseExpression();
      this.semicolon();
      return this.finishNode(node, "ThrowStatement");
    };
    empty$1 = [];
    pp$8.parseCatchClauseParam = function() {
      var param = this.parseBindingAtom();
      var simple = param.type === "Identifier";
      this.enterScope(simple ? SCOPE_SIMPLE_CATCH : 0);
      this.checkLValPattern(param, simple ? BIND_SIMPLE_CATCH : BIND_LEXICAL);
      this.expect(types$1.parenR);
      return param;
    };
    pp$8.parseTryStatement = function(node) {
      this.next();
      node.block = this.parseBlock();
      node.handler = null;
      if (this.type === types$1._catch) {
        var clause = this.startNode();
        this.next();
        if (this.eat(types$1.parenL)) {
          clause.param = this.parseCatchClauseParam();
        } else {
          if (this.options.ecmaVersion < 10) {
            this.unexpected();
          }
          clause.param = null;
          this.enterScope(0);
        }
        clause.body = this.parseBlock(false);
        this.exitScope();
        node.handler = this.finishNode(clause, "CatchClause");
      }
      node.finalizer = this.eat(types$1._finally) ? this.parseBlock() : null;
      if (!node.handler && !node.finalizer) {
        this.raise(node.start, "Missing catch or finally clause");
      }
      return this.finishNode(node, "TryStatement");
    };
    pp$8.parseVarStatement = function(node, kind, allowMissingInitializer) {
      this.next();
      this.parseVar(node, false, kind, allowMissingInitializer);
      this.semicolon();
      return this.finishNode(node, "VariableDeclaration");
    };
    pp$8.parseWhileStatement = function(node) {
      this.next();
      node.test = this.parseParenExpression();
      this.labels.push(loopLabel);
      node.body = this.parseStatement("while");
      this.labels.pop();
      return this.finishNode(node, "WhileStatement");
    };
    pp$8.parseWithStatement = function(node) {
      if (this.strict) {
        this.raise(this.start, "'with' in strict mode");
      }
      this.next();
      node.object = this.parseParenExpression();
      node.body = this.parseStatement("with");
      return this.finishNode(node, "WithStatement");
    };
    pp$8.parseEmptyStatement = function(node) {
      this.next();
      return this.finishNode(node, "EmptyStatement");
    };
    pp$8.parseLabeledStatement = function(node, maybeName, expr, context) {
      for (var i$1 = 0, list2 = this.labels; i$1 < list2.length; i$1 += 1) {
        var label = list2[i$1];
        if (label.name === maybeName) {
          this.raise(expr.start, "Label '" + maybeName + "' is already declared");
        }
      }
      var kind = this.type.isLoop ? "loop" : this.type === types$1._switch ? "switch" : null;
      for (var i2 = this.labels.length - 1; i2 >= 0; i2--) {
        var label$1 = this.labels[i2];
        if (label$1.statementStart === node.start) {
          label$1.statementStart = this.start;
          label$1.kind = kind;
        } else {
          break;
        }
      }
      this.labels.push({ name: maybeName, kind, statementStart: this.start });
      node.body = this.parseStatement(context ? context.indexOf("label") === -1 ? context + "label" : context : "label");
      this.labels.pop();
      node.label = expr;
      return this.finishNode(node, "LabeledStatement");
    };
    pp$8.parseExpressionStatement = function(node, expr) {
      node.expression = expr;
      this.semicolon();
      return this.finishNode(node, "ExpressionStatement");
    };
    pp$8.parseBlock = function(createNewLexicalScope, node, exitStrict) {
      if (createNewLexicalScope === void 0) createNewLexicalScope = true;
      if (node === void 0) node = this.startNode();
      node.body = [];
      this.expect(types$1.braceL);
      if (createNewLexicalScope) {
        this.enterScope(0);
      }
      while (this.type !== types$1.braceR) {
        var stmt = this.parseStatement(null);
        node.body.push(stmt);
      }
      if (exitStrict) {
        this.strict = false;
      }
      this.next();
      if (createNewLexicalScope) {
        this.exitScope();
      }
      return this.finishNode(node, "BlockStatement");
    };
    pp$8.parseFor = function(node, init) {
      node.init = init;
      this.expect(types$1.semi);
      node.test = this.type === types$1.semi ? null : this.parseExpression();
      this.expect(types$1.semi);
      node.update = this.type === types$1.parenR ? null : this.parseExpression();
      this.expect(types$1.parenR);
      node.body = this.parseStatement("for");
      this.exitScope();
      this.labels.pop();
      return this.finishNode(node, "ForStatement");
    };
    pp$8.parseForIn = function(node, init) {
      var isForIn = this.type === types$1._in;
      this.next();
      if (init.type === "VariableDeclaration" && init.declarations[0].init != null && (!isForIn || this.options.ecmaVersion < 8 || this.strict || init.kind !== "var" || init.declarations[0].id.type !== "Identifier")) {
        this.raise(
          init.start,
          (isForIn ? "for-in" : "for-of") + " loop variable declaration may not have an initializer"
        );
      }
      node.left = init;
      node.right = isForIn ? this.parseExpression() : this.parseMaybeAssign();
      this.expect(types$1.parenR);
      node.body = this.parseStatement("for");
      this.exitScope();
      this.labels.pop();
      return this.finishNode(node, isForIn ? "ForInStatement" : "ForOfStatement");
    };
    pp$8.parseVar = function(node, isFor, kind, allowMissingInitializer) {
      node.declarations = [];
      node.kind = kind;
      for (; ; ) {
        var decl = this.startNode();
        this.parseVarId(decl, kind);
        if (this.eat(types$1.eq)) {
          decl.init = this.parseMaybeAssign(isFor);
        } else if (!allowMissingInitializer && kind === "const" && !(this.type === types$1._in || this.options.ecmaVersion >= 6 && this.isContextual("of"))) {
          this.unexpected();
        } else if (!allowMissingInitializer && (kind === "using" || kind === "await using") && this.options.ecmaVersion >= 17 && this.type !== types$1._in && !this.isContextual("of")) {
          this.raise(this.lastTokEnd, "Missing initializer in " + kind + " declaration");
        } else if (!allowMissingInitializer && decl.id.type !== "Identifier" && !(isFor && (this.type === types$1._in || this.isContextual("of")))) {
          this.raise(this.lastTokEnd, "Complex binding patterns require an initialization value");
        } else {
          decl.init = null;
        }
        node.declarations.push(this.finishNode(decl, "VariableDeclarator"));
        if (!this.eat(types$1.comma)) {
          break;
        }
      }
      return node;
    };
    pp$8.parseVarId = function(decl, kind) {
      decl.id = kind === "using" || kind === "await using" ? this.parseIdent() : this.parseBindingAtom();
      this.checkLValPattern(decl.id, kind === "var" ? BIND_VAR : BIND_LEXICAL, false);
    };
    FUNC_STATEMENT = 1;
    FUNC_HANGING_STATEMENT = 2;
    FUNC_NULLABLE_ID = 4;
    pp$8.parseFunction = function(node, statement, allowExpressionBody, isAsync, forInit) {
      this.initFunction(node);
      if (this.options.ecmaVersion >= 9 || this.options.ecmaVersion >= 6 && !isAsync) {
        if (this.type === types$1.star && statement & FUNC_HANGING_STATEMENT) {
          this.unexpected();
        }
        node.generator = this.eat(types$1.star);
      }
      if (this.options.ecmaVersion >= 8) {
        node.async = !!isAsync;
      }
      if (statement & FUNC_STATEMENT) {
        node.id = statement & FUNC_NULLABLE_ID && this.type !== types$1.name ? null : this.parseIdent();
        if (node.id && !(statement & FUNC_HANGING_STATEMENT)) {
          this.checkLValSimple(node.id, this.strict || node.generator || node.async ? this.treatFunctionsAsVar ? BIND_VAR : BIND_LEXICAL : BIND_FUNCTION);
        }
      }
      var oldYieldPos = this.yieldPos, oldAwaitPos = this.awaitPos, oldAwaitIdentPos = this.awaitIdentPos;
      this.yieldPos = 0;
      this.awaitPos = 0;
      this.awaitIdentPos = 0;
      this.enterScope(functionFlags(node.async, node.generator));
      if (!(statement & FUNC_STATEMENT)) {
        node.id = this.type === types$1.name ? this.parseIdent() : null;
      }
      this.parseFunctionParams(node);
      this.parseFunctionBody(node, allowExpressionBody, false, forInit);
      this.yieldPos = oldYieldPos;
      this.awaitPos = oldAwaitPos;
      this.awaitIdentPos = oldAwaitIdentPos;
      return this.finishNode(node, statement & FUNC_STATEMENT ? "FunctionDeclaration" : "FunctionExpression");
    };
    pp$8.parseFunctionParams = function(node) {
      this.expect(types$1.parenL);
      node.params = this.parseBindingList(types$1.parenR, false, this.options.ecmaVersion >= 8);
      this.checkYieldAwaitInDefaultParams();
    };
    pp$8.parseClass = function(node, isStatement) {
      this.next();
      var oldStrict = this.strict;
      this.strict = true;
      this.parseClassId(node, isStatement);
      this.parseClassSuper(node);
      var privateNameMap = this.enterClassBody();
      var classBody = this.startNode();
      var hadConstructor = false;
      classBody.body = [];
      this.expect(types$1.braceL);
      while (this.type !== types$1.braceR) {
        var element = this.parseClassElement(node.superClass !== null);
        if (element) {
          classBody.body.push(element);
          if (element.type === "MethodDefinition" && element.kind === "constructor") {
            if (hadConstructor) {
              this.raiseRecoverable(element.start, "Duplicate constructor in the same class");
            }
            hadConstructor = true;
          } else if (element.key && element.key.type === "PrivateIdentifier" && isPrivateNameConflicted(privateNameMap, element)) {
            this.raiseRecoverable(element.key.start, "Identifier '#" + element.key.name + "' has already been declared");
          }
        }
      }
      this.strict = oldStrict;
      this.next();
      node.body = this.finishNode(classBody, "ClassBody");
      this.exitClassBody();
      return this.finishNode(node, isStatement ? "ClassDeclaration" : "ClassExpression");
    };
    pp$8.parseClassElement = function(constructorAllowsSuper) {
      if (this.eat(types$1.semi)) {
        return null;
      }
      var ecmaVersion2 = this.options.ecmaVersion;
      var node = this.startNode();
      var keyName = "";
      var isGenerator = false;
      var isAsync = false;
      var kind = "method";
      var isStatic = false;
      if (this.eatContextual("static")) {
        if (ecmaVersion2 >= 13 && this.eat(types$1.braceL)) {
          this.parseClassStaticBlock(node);
          return node;
        }
        if (this.isClassElementNameStart() || this.type === types$1.star) {
          isStatic = true;
        } else {
          keyName = "static";
        }
      }
      node.static = isStatic;
      if (!keyName && ecmaVersion2 >= 8 && this.eatContextual("async")) {
        if ((this.isClassElementNameStart() || this.type === types$1.star) && !this.canInsertSemicolon()) {
          isAsync = true;
        } else {
          keyName = "async";
        }
      }
      if (!keyName && (ecmaVersion2 >= 9 || !isAsync) && this.eat(types$1.star)) {
        isGenerator = true;
      }
      if (!keyName && !isAsync && !isGenerator) {
        var lastValue = this.value;
        if (this.eatContextual("get") || this.eatContextual("set")) {
          if (this.isClassElementNameStart()) {
            kind = lastValue;
          } else {
            keyName = lastValue;
          }
        }
      }
      if (keyName) {
        node.computed = false;
        node.key = this.startNodeAt(this.lastTokStart, this.lastTokStartLoc);
        node.key.name = keyName;
        this.finishNode(node.key, "Identifier");
      } else {
        this.parseClassElementName(node);
      }
      if (ecmaVersion2 < 13 || this.type === types$1.parenL || kind !== "method" || isGenerator || isAsync) {
        var isConstructor2 = !node.static && checkKeyName(node, "constructor");
        var allowsDirectSuper = isConstructor2 && constructorAllowsSuper;
        if (isConstructor2 && kind !== "method") {
          this.raise(node.key.start, "Constructor can't have get/set modifier");
        }
        node.kind = isConstructor2 ? "constructor" : kind;
        this.parseClassMethod(node, isGenerator, isAsync, allowsDirectSuper);
      } else {
        this.parseClassField(node);
      }
      return node;
    };
    pp$8.isClassElementNameStart = function() {
      return this.type === types$1.name || this.type === types$1.privateId || this.type === types$1.num || this.type === types$1.string || this.type === types$1.bracketL || this.type.keyword;
    };
    pp$8.parseClassElementName = function(element) {
      if (this.type === types$1.privateId) {
        if (this.value === "constructor") {
          this.raise(this.start, "Classes can't have an element named '#constructor'");
        }
        element.computed = false;
        element.key = this.parsePrivateIdent();
      } else {
        this.parsePropertyName(element);
      }
    };
    pp$8.parseClassMethod = function(method, isGenerator, isAsync, allowsDirectSuper) {
      var key = method.key;
      if (method.kind === "constructor") {
        if (isGenerator) {
          this.raise(key.start, "Constructor can't be a generator");
        }
        if (isAsync) {
          this.raise(key.start, "Constructor can't be an async method");
        }
      } else if (method.static && checkKeyName(method, "prototype")) {
        this.raise(key.start, "Classes may not have a static property named prototype");
      }
      var value = method.value = this.parseMethod(isGenerator, isAsync, allowsDirectSuper);
      if (method.kind === "get" && value.params.length !== 0) {
        this.raiseRecoverable(value.start, "getter should have no params");
      }
      if (method.kind === "set" && value.params.length !== 1) {
        this.raiseRecoverable(value.start, "setter should have exactly one param");
      }
      if (method.kind === "set" && value.params[0].type === "RestElement") {
        this.raiseRecoverable(value.params[0].start, "Setter cannot use rest params");
      }
      return this.finishNode(method, "MethodDefinition");
    };
    pp$8.parseClassField = function(field) {
      if (checkKeyName(field, "constructor")) {
        this.raise(field.key.start, "Classes can't have a field named 'constructor'");
      } else if (field.static && checkKeyName(field, "prototype")) {
        this.raise(field.key.start, "Classes can't have a static field named 'prototype'");
      }
      if (this.eat(types$1.eq)) {
        this.enterScope(SCOPE_CLASS_FIELD_INIT | SCOPE_SUPER);
        field.value = this.parseMaybeAssign();
        this.exitScope();
      } else {
        field.value = null;
      }
      this.semicolon();
      return this.finishNode(field, "PropertyDefinition");
    };
    pp$8.parseClassStaticBlock = function(node) {
      node.body = [];
      var oldLabels = this.labels;
      this.labels = [];
      this.enterScope(SCOPE_CLASS_STATIC_BLOCK | SCOPE_SUPER);
      while (this.type !== types$1.braceR) {
        var stmt = this.parseStatement(null);
        node.body.push(stmt);
      }
      this.next();
      this.exitScope();
      this.labels = oldLabels;
      return this.finishNode(node, "StaticBlock");
    };
    pp$8.parseClassId = function(node, isStatement) {
      if (this.type === types$1.name) {
        node.id = this.parseIdent();
        if (isStatement) {
          this.checkLValSimple(node.id, BIND_LEXICAL, false);
        }
      } else {
        if (isStatement === true) {
          this.unexpected();
        }
        node.id = null;
      }
    };
    pp$8.parseClassSuper = function(node) {
      node.superClass = this.eat(types$1._extends) ? this.parseExprSubscripts(null, false) : null;
    };
    pp$8.enterClassBody = function() {
      var element = { declared: /* @__PURE__ */ Object.create(null), used: [] };
      this.privateNameStack.push(element);
      return element.declared;
    };
    pp$8.exitClassBody = function() {
      var ref2 = this.privateNameStack.pop();
      var declared = ref2.declared;
      var used = ref2.used;
      if (!this.options.checkPrivateFields) {
        return;
      }
      var len = this.privateNameStack.length;
      var parent = len === 0 ? null : this.privateNameStack[len - 1];
      for (var i2 = 0; i2 < used.length; ++i2) {
        var id = used[i2];
        if (!hasOwn(declared, id.name)) {
          if (parent) {
            parent.used.push(id);
          } else {
            this.raiseRecoverable(id.start, "Private field '#" + id.name + "' must be declared in an enclosing class");
          }
        }
      }
    };
    pp$8.parseExportAllDeclaration = function(node, exports$1) {
      if (this.options.ecmaVersion >= 11) {
        if (this.eatContextual("as")) {
          node.exported = this.parseModuleExportName();
          this.checkExport(exports$1, node.exported, this.lastTokStart);
        } else {
          node.exported = null;
        }
      }
      this.expectContextual("from");
      if (this.type !== types$1.string) {
        this.unexpected();
      }
      node.source = this.parseExprAtom();
      if (this.options.ecmaVersion >= 16) {
        node.attributes = this.parseWithClause();
      }
      this.semicolon();
      return this.finishNode(node, "ExportAllDeclaration");
    };
    pp$8.parseExport = function(node, exports$1) {
      this.next();
      if (this.eat(types$1.star)) {
        return this.parseExportAllDeclaration(node, exports$1);
      }
      if (this.eat(types$1._default)) {
        this.checkExport(exports$1, "default", this.lastTokStart);
        node.declaration = this.parseExportDefaultDeclaration();
        return this.finishNode(node, "ExportDefaultDeclaration");
      }
      if (this.shouldParseExportStatement()) {
        node.declaration = this.parseExportDeclaration(node);
        if (node.declaration.type === "VariableDeclaration") {
          this.checkVariableExport(exports$1, node.declaration.declarations);
        } else {
          this.checkExport(exports$1, node.declaration.id, node.declaration.id.start);
        }
        node.specifiers = [];
        node.source = null;
        if (this.options.ecmaVersion >= 16) {
          node.attributes = [];
        }
      } else {
        node.declaration = null;
        node.specifiers = this.parseExportSpecifiers(exports$1);
        if (this.eatContextual("from")) {
          if (this.type !== types$1.string) {
            this.unexpected();
          }
          node.source = this.parseExprAtom();
          if (this.options.ecmaVersion >= 16) {
            node.attributes = this.parseWithClause();
          }
        } else {
          for (var i2 = 0, list2 = node.specifiers; i2 < list2.length; i2 += 1) {
            var spec = list2[i2];
            this.checkUnreserved(spec.local);
            this.checkLocalExport(spec.local);
            if (spec.local.type === "Literal") {
              this.raise(spec.local.start, "A string literal cannot be used as an exported binding without `from`.");
            }
          }
          node.source = null;
          if (this.options.ecmaVersion >= 16) {
            node.attributes = [];
          }
        }
        this.semicolon();
      }
      return this.finishNode(node, "ExportNamedDeclaration");
    };
    pp$8.parseExportDeclaration = function(node) {
      return this.parseStatement(null);
    };
    pp$8.parseExportDefaultDeclaration = function() {
      var isAsync;
      if (this.type === types$1._function || (isAsync = this.isAsyncFunction())) {
        var fNode = this.startNode();
        this.next();
        if (isAsync) {
          this.next();
        }
        return this.parseFunction(fNode, FUNC_STATEMENT | FUNC_NULLABLE_ID, false, isAsync);
      } else if (this.type === types$1._class) {
        var cNode = this.startNode();
        return this.parseClass(cNode, "nullableID");
      } else {
        var declaration = this.parseMaybeAssign();
        this.semicolon();
        return declaration;
      }
    };
    pp$8.checkExport = function(exports$1, name, pos) {
      if (!exports$1) {
        return;
      }
      if (typeof name !== "string") {
        name = name.type === "Identifier" ? name.name : name.value;
      }
      if (hasOwn(exports$1, name)) {
        this.raiseRecoverable(pos, "Duplicate export '" + name + "'");
      }
      exports$1[name] = true;
    };
    pp$8.checkPatternExport = function(exports$1, pat) {
      var type2 = pat.type;
      if (type2 === "Identifier") {
        this.checkExport(exports$1, pat, pat.start);
      } else if (type2 === "ObjectPattern") {
        for (var i2 = 0, list2 = pat.properties; i2 < list2.length; i2 += 1) {
          var prop = list2[i2];
          this.checkPatternExport(exports$1, prop);
        }
      } else if (type2 === "ArrayPattern") {
        for (var i$1 = 0, list$1 = pat.elements; i$1 < list$1.length; i$1 += 1) {
          var elt = list$1[i$1];
          if (elt) {
            this.checkPatternExport(exports$1, elt);
          }
        }
      } else if (type2 === "Property") {
        this.checkPatternExport(exports$1, pat.value);
      } else if (type2 === "AssignmentPattern") {
        this.checkPatternExport(exports$1, pat.left);
      } else if (type2 === "RestElement") {
        this.checkPatternExport(exports$1, pat.argument);
      }
    };
    pp$8.checkVariableExport = function(exports$1, decls) {
      if (!exports$1) {
        return;
      }
      for (var i2 = 0, list2 = decls; i2 < list2.length; i2 += 1) {
        var decl = list2[i2];
        this.checkPatternExport(exports$1, decl.id);
      }
    };
    pp$8.shouldParseExportStatement = function() {
      return this.type.keyword === "var" || this.type.keyword === "const" || this.type.keyword === "class" || this.type.keyword === "function" || this.isLet() || this.isAsyncFunction();
    };
    pp$8.parseExportSpecifier = function(exports$1) {
      var node = this.startNode();
      node.local = this.parseModuleExportName();
      node.exported = this.eatContextual("as") ? this.parseModuleExportName() : node.local;
      this.checkExport(
        exports$1,
        node.exported,
        node.exported.start
      );
      return this.finishNode(node, "ExportSpecifier");
    };
    pp$8.parseExportSpecifiers = function(exports$1) {
      var nodes = [], first = true;
      this.expect(types$1.braceL);
      while (!this.eat(types$1.braceR)) {
        if (!first) {
          this.expect(types$1.comma);
          if (this.afterTrailingComma(types$1.braceR)) {
            break;
          }
        } else {
          first = false;
        }
        nodes.push(this.parseExportSpecifier(exports$1));
      }
      return nodes;
    };
    pp$8.parseImport = function(node) {
      this.next();
      if (this.type === types$1.string) {
        node.specifiers = empty$1;
        node.source = this.parseExprAtom();
      } else {
        node.specifiers = this.parseImportSpecifiers();
        this.expectContextual("from");
        node.source = this.type === types$1.string ? this.parseExprAtom() : this.unexpected();
      }
      if (this.options.ecmaVersion >= 16) {
        node.attributes = this.parseWithClause();
      }
      this.semicolon();
      return this.finishNode(node, "ImportDeclaration");
    };
    pp$8.parseImportSpecifier = function() {
      var node = this.startNode();
      node.imported = this.parseModuleExportName();
      if (this.eatContextual("as")) {
        node.local = this.parseIdent();
      } else {
        this.checkUnreserved(node.imported);
        node.local = node.imported;
      }
      this.checkLValSimple(node.local, BIND_LEXICAL);
      return this.finishNode(node, "ImportSpecifier");
    };
    pp$8.parseImportDefaultSpecifier = function() {
      var node = this.startNode();
      node.local = this.parseIdent();
      this.checkLValSimple(node.local, BIND_LEXICAL);
      return this.finishNode(node, "ImportDefaultSpecifier");
    };
    pp$8.parseImportNamespaceSpecifier = function() {
      var node = this.startNode();
      this.next();
      this.expectContextual("as");
      node.local = this.parseIdent();
      this.checkLValSimple(node.local, BIND_LEXICAL);
      return this.finishNode(node, "ImportNamespaceSpecifier");
    };
    pp$8.parseImportSpecifiers = function() {
      var nodes = [], first = true;
      if (this.type === types$1.name) {
        nodes.push(this.parseImportDefaultSpecifier());
        if (!this.eat(types$1.comma)) {
          return nodes;
        }
      }
      if (this.type === types$1.star) {
        nodes.push(this.parseImportNamespaceSpecifier());
        return nodes;
      }
      this.expect(types$1.braceL);
      while (!this.eat(types$1.braceR)) {
        if (!first) {
          this.expect(types$1.comma);
          if (this.afterTrailingComma(types$1.braceR)) {
            break;
          }
        } else {
          first = false;
        }
        nodes.push(this.parseImportSpecifier());
      }
      return nodes;
    };
    pp$8.parseWithClause = function() {
      var nodes = [];
      if (!this.eat(types$1._with)) {
        return nodes;
      }
      this.expect(types$1.braceL);
      var attributeKeys = {};
      var first = true;
      while (!this.eat(types$1.braceR)) {
        if (!first) {
          this.expect(types$1.comma);
          if (this.afterTrailingComma(types$1.braceR)) {
            break;
          }
        } else {
          first = false;
        }
        var attr = this.parseImportAttribute();
        var keyName = attr.key.type === "Identifier" ? attr.key.name : attr.key.value;
        if (hasOwn(attributeKeys, keyName)) {
          this.raiseRecoverable(attr.key.start, "Duplicate attribute key '" + keyName + "'");
        }
        attributeKeys[keyName] = true;
        nodes.push(attr);
      }
      return nodes;
    };
    pp$8.parseImportAttribute = function() {
      var node = this.startNode();
      node.key = this.type === types$1.string ? this.parseExprAtom() : this.parseIdent(this.options.allowReserved !== "never");
      this.expect(types$1.colon);
      if (this.type !== types$1.string) {
        this.unexpected();
      }
      node.value = this.parseExprAtom();
      return this.finishNode(node, "ImportAttribute");
    };
    pp$8.parseModuleExportName = function() {
      if (this.options.ecmaVersion >= 13 && this.type === types$1.string) {
        var stringLiteral = this.parseLiteral(this.value);
        if (loneSurrogate.test(stringLiteral.value)) {
          this.raise(stringLiteral.start, "An export name cannot include a lone surrogate.");
        }
        return stringLiteral;
      }
      return this.parseIdent(true);
    };
    pp$8.adaptDirectivePrologue = function(statements) {
      for (var i2 = 0; i2 < statements.length && this.isDirectiveCandidate(statements[i2]); ++i2) {
        statements[i2].directive = statements[i2].expression.raw.slice(1, -1);
      }
    };
    pp$8.isDirectiveCandidate = function(statement) {
      return this.options.ecmaVersion >= 5 && statement.type === "ExpressionStatement" && statement.expression.type === "Literal" && typeof statement.expression.value === "string" && // Reject parenthesized strings.
      (this.input[statement.start] === '"' || this.input[statement.start] === "'");
    };
    pp$7 = Parser.prototype;
    pp$7.toAssignable = function(node, isBinding, refDestructuringErrors) {
      if (this.options.ecmaVersion >= 6 && node) {
        switch (node.type) {
          case "Identifier":
            if (this.inAsync && node.name === "await") {
              this.raise(node.start, "Cannot use 'await' as identifier inside an async function");
            }
            break;
          case "ObjectPattern":
          case "ArrayPattern":
          case "AssignmentPattern":
          case "RestElement":
            break;
          case "ObjectExpression":
            node.type = "ObjectPattern";
            if (refDestructuringErrors) {
              this.checkPatternErrors(refDestructuringErrors, true);
            }
            for (var i2 = 0, list2 = node.properties; i2 < list2.length; i2 += 1) {
              var prop = list2[i2];
              this.toAssignable(prop, isBinding);
              if (prop.type === "RestElement" && (prop.argument.type === "ArrayPattern" || prop.argument.type === "ObjectPattern")) {
                this.raise(prop.argument.start, "Unexpected token");
              }
            }
            break;
          case "Property":
            if (node.kind !== "init") {
              this.raise(node.key.start, "Object pattern can't contain getter or setter");
            }
            this.toAssignable(node.value, isBinding);
            break;
          case "ArrayExpression":
            node.type = "ArrayPattern";
            if (refDestructuringErrors) {
              this.checkPatternErrors(refDestructuringErrors, true);
            }
            this.toAssignableList(node.elements, isBinding);
            break;
          case "SpreadElement":
            node.type = "RestElement";
            this.toAssignable(node.argument, isBinding);
            if (node.argument.type === "AssignmentPattern") {
              this.raise(node.argument.start, "Rest elements cannot have a default value");
            }
            break;
          case "AssignmentExpression":
            if (node.operator !== "=") {
              this.raise(node.left.end, "Only '=' operator can be used for specifying default value.");
            }
            node.type = "AssignmentPattern";
            delete node.operator;
            this.toAssignable(node.left, isBinding);
            break;
          case "ParenthesizedExpression":
            this.toAssignable(node.expression, isBinding, refDestructuringErrors);
            break;
          case "ChainExpression":
            this.raiseRecoverable(node.start, "Optional chaining cannot appear in left-hand side");
            break;
          case "MemberExpression":
            if (!isBinding) {
              break;
            }
          default:
            this.raise(node.start, "Assigning to rvalue");
        }
      } else if (refDestructuringErrors) {
        this.checkPatternErrors(refDestructuringErrors, true);
      }
      return node;
    };
    pp$7.toAssignableList = function(exprList, isBinding) {
      var end = exprList.length;
      for (var i2 = 0; i2 < end; i2++) {
        var elt = exprList[i2];
        if (elt) {
          this.toAssignable(elt, isBinding);
        }
      }
      if (end) {
        var last = exprList[end - 1];
        if (this.options.ecmaVersion === 6 && isBinding && last && last.type === "RestElement" && last.argument.type !== "Identifier") {
          this.unexpected(last.argument.start);
        }
      }
      return exprList;
    };
    pp$7.parseSpread = function(refDestructuringErrors) {
      var node = this.startNode();
      this.next();
      node.argument = this.parseMaybeAssign(false, refDestructuringErrors);
      return this.finishNode(node, "SpreadElement");
    };
    pp$7.parseRestBinding = function() {
      var node = this.startNode();
      this.next();
      if (this.options.ecmaVersion === 6 && this.type !== types$1.name) {
        this.unexpected();
      }
      node.argument = this.parseBindingAtom();
      return this.finishNode(node, "RestElement");
    };
    pp$7.parseBindingAtom = function() {
      if (this.options.ecmaVersion >= 6) {
        switch (this.type) {
          case types$1.bracketL:
            var node = this.startNode();
            this.next();
            node.elements = this.parseBindingList(types$1.bracketR, true, true);
            return this.finishNode(node, "ArrayPattern");
          case types$1.braceL:
            return this.parseObj(true);
        }
      }
      return this.parseIdent();
    };
    pp$7.parseBindingList = function(close, allowEmpty, allowTrailingComma, allowModifiers) {
      var elts = [], first = true;
      while (!this.eat(close)) {
        if (first) {
          first = false;
        } else {
          this.expect(types$1.comma);
        }
        if (allowEmpty && this.type === types$1.comma) {
          elts.push(null);
        } else if (allowTrailingComma && this.afterTrailingComma(close)) {
          break;
        } else if (this.type === types$1.ellipsis) {
          var rest = this.parseRestBinding();
          this.parseBindingListItem(rest);
          elts.push(rest);
          if (this.type === types$1.comma) {
            this.raiseRecoverable(this.start, "Comma is not permitted after the rest element");
          }
          this.expect(close);
          break;
        } else {
          elts.push(this.parseAssignableListItem(allowModifiers));
        }
      }
      return elts;
    };
    pp$7.parseAssignableListItem = function(allowModifiers) {
      var elem = this.parseMaybeDefault(this.start, this.startLoc);
      this.parseBindingListItem(elem);
      return elem;
    };
    pp$7.parseBindingListItem = function(param) {
      return param;
    };
    pp$7.parseMaybeDefault = function(startPos, startLoc, left) {
      left = left || this.parseBindingAtom();
      if (this.options.ecmaVersion < 6 || !this.eat(types$1.eq)) {
        return left;
      }
      var node = this.startNodeAt(startPos, startLoc);
      node.left = left;
      node.right = this.parseMaybeAssign();
      return this.finishNode(node, "AssignmentPattern");
    };
    pp$7.checkLValSimple = function(expr, bindingType, checkClashes) {
      if (bindingType === void 0) bindingType = BIND_NONE;
      var isBind = bindingType !== BIND_NONE;
      switch (expr.type) {
        case "Identifier":
          if (this.strict && this.reservedWordsStrictBind.test(expr.name)) {
            this.raiseRecoverable(expr.start, (isBind ? "Binding " : "Assigning to ") + expr.name + " in strict mode");
          }
          if (isBind) {
            if (bindingType === BIND_LEXICAL && expr.name === "let") {
              this.raiseRecoverable(expr.start, "let is disallowed as a lexically bound name");
            }
            if (checkClashes) {
              if (hasOwn(checkClashes, expr.name)) {
                this.raiseRecoverable(expr.start, "Argument name clash");
              }
              checkClashes[expr.name] = true;
            }
            if (bindingType !== BIND_OUTSIDE) {
              this.declareName(expr.name, bindingType, expr.start);
            }
          }
          break;
        case "ChainExpression":
          this.raiseRecoverable(expr.start, "Optional chaining cannot appear in left-hand side");
          break;
        case "MemberExpression":
          if (isBind) {
            this.raiseRecoverable(expr.start, "Binding member expression");
          }
          break;
        case "ParenthesizedExpression":
          if (isBind) {
            this.raiseRecoverable(expr.start, "Binding parenthesized expression");
          }
          return this.checkLValSimple(expr.expression, bindingType, checkClashes);
        default:
          this.raise(expr.start, (isBind ? "Binding" : "Assigning to") + " rvalue");
      }
    };
    pp$7.checkLValPattern = function(expr, bindingType, checkClashes) {
      if (bindingType === void 0) bindingType = BIND_NONE;
      switch (expr.type) {
        case "ObjectPattern":
          for (var i2 = 0, list2 = expr.properties; i2 < list2.length; i2 += 1) {
            var prop = list2[i2];
            this.checkLValInnerPattern(prop, bindingType, checkClashes);
          }
          break;
        case "ArrayPattern":
          for (var i$1 = 0, list$1 = expr.elements; i$1 < list$1.length; i$1 += 1) {
            var elem = list$1[i$1];
            if (elem) {
              this.checkLValInnerPattern(elem, bindingType, checkClashes);
            }
          }
          break;
        default:
          this.checkLValSimple(expr, bindingType, checkClashes);
      }
    };
    pp$7.checkLValInnerPattern = function(expr, bindingType, checkClashes) {
      if (bindingType === void 0) bindingType = BIND_NONE;
      switch (expr.type) {
        case "Property":
          this.checkLValInnerPattern(expr.value, bindingType, checkClashes);
          break;
        case "AssignmentPattern":
          this.checkLValPattern(expr.left, bindingType, checkClashes);
          break;
        case "RestElement":
          this.checkLValPattern(expr.argument, bindingType, checkClashes);
          break;
        default:
          this.checkLValPattern(expr, bindingType, checkClashes);
      }
    };
    TokContext = function TokContext2(token, isExpr, preserveSpace, override, generator) {
      this.token = token;
      this.isExpr = !!isExpr;
      this.preserveSpace = !!preserveSpace;
      this.override = override;
      this.generator = !!generator;
    };
    types2 = {
      b_stat: new TokContext("{", false),
      b_expr: new TokContext("{", true),
      b_tmpl: new TokContext("${", false),
      p_stat: new TokContext("(", false),
      p_expr: new TokContext("(", true),
      q_tmpl: new TokContext("`", true, true, function(p) {
        return p.tryReadTemplateToken();
      }),
      f_stat: new TokContext("function", false),
      f_expr: new TokContext("function", true),
      f_expr_gen: new TokContext("function", true, false, null, true),
      f_gen: new TokContext("function", false, false, null, true)
    };
    pp$6 = Parser.prototype;
    pp$6.initialContext = function() {
      return [types2.b_stat];
    };
    pp$6.curContext = function() {
      return this.context[this.context.length - 1];
    };
    pp$6.braceIsBlock = function(prevType) {
      var parent = this.curContext();
      if (parent === types2.f_expr || parent === types2.f_stat) {
        return true;
      }
      if (prevType === types$1.colon && (parent === types2.b_stat || parent === types2.b_expr)) {
        return !parent.isExpr;
      }
      if (prevType === types$1._return || prevType === types$1.name && this.exprAllowed) {
        return lineBreak.test(this.input.slice(this.lastTokEnd, this.start));
      }
      if (prevType === types$1._else || prevType === types$1.semi || prevType === types$1.eof || prevType === types$1.parenR || prevType === types$1.arrow) {
        return true;
      }
      if (prevType === types$1.braceL) {
        return parent === types2.b_stat;
      }
      if (prevType === types$1._var || prevType === types$1._const || prevType === types$1.name) {
        return false;
      }
      return !this.exprAllowed;
    };
    pp$6.inGeneratorContext = function() {
      for (var i2 = this.context.length - 1; i2 >= 1; i2--) {
        var context = this.context[i2];
        if (context.token === "function") {
          return context.generator;
        }
      }
      return false;
    };
    pp$6.updateContext = function(prevType) {
      var update, type2 = this.type;
      if (type2.keyword && prevType === types$1.dot) {
        this.exprAllowed = false;
      } else if (update = type2.updateContext) {
        update.call(this, prevType);
      } else {
        this.exprAllowed = type2.beforeExpr;
      }
    };
    pp$6.overrideContext = function(tokenCtx) {
      if (this.curContext() !== tokenCtx) {
        this.context[this.context.length - 1] = tokenCtx;
      }
    };
    types$1.parenR.updateContext = types$1.braceR.updateContext = function() {
      if (this.context.length === 1) {
        this.exprAllowed = true;
        return;
      }
      var out = this.context.pop();
      if (out === types2.b_stat && this.curContext().token === "function") {
        out = this.context.pop();
      }
      this.exprAllowed = !out.isExpr;
    };
    types$1.braceL.updateContext = function(prevType) {
      this.context.push(this.braceIsBlock(prevType) ? types2.b_stat : types2.b_expr);
      this.exprAllowed = true;
    };
    types$1.dollarBraceL.updateContext = function() {
      this.context.push(types2.b_tmpl);
      this.exprAllowed = true;
    };
    types$1.parenL.updateContext = function(prevType) {
      var statementParens = prevType === types$1._if || prevType === types$1._for || prevType === types$1._with || prevType === types$1._while;
      this.context.push(statementParens ? types2.p_stat : types2.p_expr);
      this.exprAllowed = true;
    };
    types$1.incDec.updateContext = function() {
    };
    types$1._function.updateContext = types$1._class.updateContext = function(prevType) {
      if (prevType.beforeExpr && prevType !== types$1._else && !(prevType === types$1.semi && this.curContext() !== types2.p_stat) && !(prevType === types$1._return && lineBreak.test(this.input.slice(this.lastTokEnd, this.start))) && !((prevType === types$1.colon || prevType === types$1.braceL) && this.curContext() === types2.b_stat)) {
        this.context.push(types2.f_expr);
      } else {
        this.context.push(types2.f_stat);
      }
      this.exprAllowed = false;
    };
    types$1.colon.updateContext = function() {
      if (this.curContext().token === "function") {
        this.context.pop();
      }
      this.exprAllowed = true;
    };
    types$1.backQuote.updateContext = function() {
      if (this.curContext() === types2.q_tmpl) {
        this.context.pop();
      } else {
        this.context.push(types2.q_tmpl);
      }
      this.exprAllowed = false;
    };
    types$1.star.updateContext = function(prevType) {
      if (prevType === types$1._function) {
        var index = this.context.length - 1;
        if (this.context[index] === types2.f_expr) {
          this.context[index] = types2.f_expr_gen;
        } else {
          this.context[index] = types2.f_gen;
        }
      }
      this.exprAllowed = true;
    };
    types$1.name.updateContext = function(prevType) {
      var allowed = false;
      if (this.options.ecmaVersion >= 6 && prevType !== types$1.dot) {
        if (this.value === "of" && !this.exprAllowed || this.value === "yield" && this.inGeneratorContext()) {
          allowed = true;
        }
      }
      this.exprAllowed = allowed;
    };
    pp$5 = Parser.prototype;
    pp$5.checkPropClash = function(prop, propHash, refDestructuringErrors) {
      if (this.options.ecmaVersion >= 9 && prop.type === "SpreadElement") {
        return;
      }
      if (this.options.ecmaVersion >= 6 && (prop.computed || prop.method || prop.shorthand)) {
        return;
      }
      var key = prop.key;
      var name;
      switch (key.type) {
        case "Identifier":
          name = key.name;
          break;
        case "Literal":
          name = String(key.value);
          break;
        default:
          return;
      }
      var kind = prop.kind;
      if (this.options.ecmaVersion >= 6) {
        if (name === "__proto__" && kind === "init") {
          if (propHash.proto) {
            if (refDestructuringErrors) {
              if (refDestructuringErrors.doubleProto < 0) {
                refDestructuringErrors.doubleProto = key.start;
              }
            } else {
              this.raiseRecoverable(key.start, "Redefinition of __proto__ property");
            }
          }
          propHash.proto = true;
        }
        return;
      }
      name = "$" + name;
      var other = propHash[name];
      if (other) {
        var redefinition;
        if (kind === "init") {
          redefinition = this.strict && other.init || other.get || other.set;
        } else {
          redefinition = other.init || other[kind];
        }
        if (redefinition) {
          this.raiseRecoverable(key.start, "Redefinition of property");
        }
      } else {
        other = propHash[name] = {
          init: false,
          get: false,
          set: false
        };
      }
      other[kind] = true;
    };
    pp$5.parseExpression = function(forInit, refDestructuringErrors) {
      var this$1$1 = this;
      return this.catchStackOverflow(function() {
        var startPos = this$1$1.start, startLoc = this$1$1.startLoc;
        var expr = this$1$1.parseMaybeAssign(forInit, refDestructuringErrors);
        if (this$1$1.type === types$1.comma) {
          var node = this$1$1.startNodeAt(startPos, startLoc);
          node.expressions = [expr];
          while (this$1$1.eat(types$1.comma)) {
            node.expressions.push(this$1$1.parseMaybeAssign(forInit, refDestructuringErrors));
          }
          return this$1$1.finishNode(node, "SequenceExpression");
        }
        return expr;
      });
    };
    pp$5.parseMaybeAssign = function(forInit, refDestructuringErrors, afterLeftParse) {
      if (this.isContextual("yield")) {
        if (this.inGenerator) {
          return this.parseYield(forInit);
        } else {
          this.exprAllowed = false;
        }
      }
      var ownDestructuringErrors = false, oldParenAssign = -1, oldTrailingComma = -1, oldDoubleProto = -1;
      if (refDestructuringErrors) {
        oldParenAssign = refDestructuringErrors.parenthesizedAssign;
        oldTrailingComma = refDestructuringErrors.trailingComma;
        oldDoubleProto = refDestructuringErrors.doubleProto;
        refDestructuringErrors.parenthesizedAssign = refDestructuringErrors.trailingComma = -1;
      } else {
        refDestructuringErrors = new DestructuringErrors();
        ownDestructuringErrors = true;
      }
      var startPos = this.start, startLoc = this.startLoc;
      if (this.type === types$1.parenL || this.type === types$1.name) {
        this.potentialArrowAt = this.start;
        this.potentialArrowInForAwait = forInit === "await";
      }
      var left = this.parseMaybeConditional(forInit, refDestructuringErrors);
      if (afterLeftParse) {
        left = afterLeftParse.call(this, left, startPos, startLoc);
      }
      if (this.type.isAssign) {
        var node = this.startNodeAt(startPos, startLoc);
        node.operator = this.value;
        if (this.type === types$1.eq) {
          left = this.toAssignable(left, false, refDestructuringErrors);
        }
        if (!ownDestructuringErrors) {
          refDestructuringErrors.parenthesizedAssign = refDestructuringErrors.trailingComma = refDestructuringErrors.doubleProto = -1;
        }
        if (refDestructuringErrors.shorthandAssign >= left.start) {
          refDestructuringErrors.shorthandAssign = -1;
        }
        if (this.type === types$1.eq) {
          this.checkLValPattern(left);
        } else {
          this.checkLValSimple(left);
        }
        node.left = left;
        this.next();
        node.right = this.parseMaybeAssign(forInit);
        if (oldDoubleProto > -1) {
          refDestructuringErrors.doubleProto = oldDoubleProto;
        }
        return this.finishNode(node, "AssignmentExpression");
      } else {
        if (ownDestructuringErrors) {
          this.checkExpressionErrors(refDestructuringErrors, true);
        }
      }
      if (oldParenAssign > -1) {
        refDestructuringErrors.parenthesizedAssign = oldParenAssign;
      }
      if (oldTrailingComma > -1) {
        refDestructuringErrors.trailingComma = oldTrailingComma;
      }
      return left;
    };
    pp$5.parseMaybeConditional = function(forInit, refDestructuringErrors) {
      var startPos = this.start, startLoc = this.startLoc;
      var expr = this.parseExprOps(forInit, refDestructuringErrors);
      if (this.checkExpressionErrors(refDestructuringErrors)) {
        return expr;
      }
      if (!(expr.type === "ArrowFunctionExpression" && expr.start === startPos) && this.eat(types$1.question)) {
        var node = this.startNodeAt(startPos, startLoc);
        node.test = expr;
        node.consequent = this.parseMaybeAssign();
        this.expect(types$1.colon);
        node.alternate = this.parseMaybeAssign(forInit);
        return this.finishNode(node, "ConditionalExpression");
      }
      return expr;
    };
    pp$5.parseExprOps = function(forInit, refDestructuringErrors) {
      var startPos = this.start, startLoc = this.startLoc;
      var expr = this.parseMaybeUnary(refDestructuringErrors, false, false, forInit);
      if (this.checkExpressionErrors(refDestructuringErrors)) {
        return expr;
      }
      return expr.start === startPos && expr.type === "ArrowFunctionExpression" ? expr : this.parseExprOp(expr, startPos, startLoc, -1, forInit);
    };
    pp$5.parseExprOp = function(left, leftStartPos, leftStartLoc, minPrec, forInit) {
      var prec = this.type.binop;
      if (prec != null && (!forInit || this.type !== types$1._in)) {
        if (prec > minPrec) {
          var logical = this.type === types$1.logicalOR || this.type === types$1.logicalAND;
          var coalesce = this.type === types$1.coalesce;
          if (coalesce) {
            prec = types$1.logicalAND.binop;
          }
          var op = this.value;
          this.next();
          var startPos = this.start, startLoc = this.startLoc;
          var right = this.parseExprOp(this.parseMaybeUnary(null, false, false, forInit), startPos, startLoc, prec, forInit);
          var node = this.buildBinary(leftStartPos, leftStartLoc, left, right, op, logical || coalesce);
          if (logical && this.type === types$1.coalesce || coalesce && (this.type === types$1.logicalOR || this.type === types$1.logicalAND)) {
            this.raiseRecoverable(this.start, "Logical expressions and coalesce expressions cannot be mixed. Wrap either by parentheses");
          }
          return this.parseExprOp(node, leftStartPos, leftStartLoc, minPrec, forInit);
        }
      }
      return left;
    };
    pp$5.buildBinary = function(startPos, startLoc, left, right, op, logical) {
      if (right.type === "PrivateIdentifier") {
        this.raise(right.start, "Private identifier can only be left side of binary expression");
      }
      var node = this.startNodeAt(startPos, startLoc);
      node.left = left;
      node.operator = op;
      node.right = right;
      return this.finishNode(node, logical ? "LogicalExpression" : "BinaryExpression");
    };
    pp$5.parseMaybeUnary = function(refDestructuringErrors, sawUnary, incDec, forInit) {
      var startPos = this.start, startLoc = this.startLoc, expr;
      if (this.isContextual("await") && this.canAwait) {
        expr = this.parseAwait(forInit);
        sawUnary = true;
      } else if (this.type.prefix) {
        var node = this.startNode(), update = this.type === types$1.incDec;
        node.operator = this.value;
        node.prefix = true;
        this.next();
        node.argument = this.parseMaybeUnary(null, true, update, forInit);
        this.checkExpressionErrors(refDestructuringErrors, true);
        if (update) {
          this.checkLValSimple(node.argument);
        } else if (this.strict && node.operator === "delete" && isLocalVariableAccess(node.argument)) {
          this.raiseRecoverable(node.start, "Deleting local variable in strict mode");
        } else if (node.operator === "delete" && isPrivateFieldAccess(node.argument)) {
          this.raiseRecoverable(node.start, "Private fields can not be deleted");
        } else {
          sawUnary = true;
        }
        expr = this.finishNode(node, update ? "UpdateExpression" : "UnaryExpression");
      } else if (!sawUnary && this.type === types$1.privateId) {
        if ((forInit || this.privateNameStack.length === 0) && this.options.checkPrivateFields) {
          this.unexpected();
        }
        expr = this.parsePrivateIdent();
        if (this.type !== types$1._in) {
          this.unexpected();
        }
      } else {
        expr = this.parseExprSubscripts(refDestructuringErrors, forInit);
        if (this.checkExpressionErrors(refDestructuringErrors)) {
          return expr;
        }
        while (this.type.postfix && !this.canInsertSemicolon()) {
          var node$1 = this.startNodeAt(startPos, startLoc);
          node$1.operator = this.value;
          node$1.prefix = false;
          node$1.argument = expr;
          this.checkLValSimple(expr);
          this.next();
          expr = this.finishNode(node$1, "UpdateExpression");
        }
      }
      if (!incDec && !(expr.type === "ArrowFunctionExpression" && expr.start === startPos) && this.eat(types$1.starstar)) {
        if (sawUnary) {
          this.unexpected(this.lastTokStart);
        } else {
          return this.buildBinary(startPos, startLoc, expr, this.parseMaybeUnary(null, false, false, forInit), "**", false);
        }
      } else {
        return expr;
      }
    };
    pp$5.parseExprSubscripts = function(refDestructuringErrors, forInit) {
      var startPos = this.start, startLoc = this.startLoc;
      var expr = this.parseExprAtom(refDestructuringErrors, forInit);
      if (expr.type === "ArrowFunctionExpression" && this.input.slice(this.lastTokStart, this.lastTokEnd) !== ")") {
        return expr;
      }
      var result = this.parseSubscripts(expr, startPos, startLoc, false, forInit);
      if (refDestructuringErrors && result.type === "MemberExpression") {
        if (refDestructuringErrors.parenthesizedAssign >= result.start) {
          refDestructuringErrors.parenthesizedAssign = -1;
        }
        if (refDestructuringErrors.parenthesizedBind >= result.start) {
          refDestructuringErrors.parenthesizedBind = -1;
        }
        if (refDestructuringErrors.trailingComma >= result.start) {
          refDestructuringErrors.trailingComma = -1;
        }
      }
      return result;
    };
    pp$5.parseSubscripts = function(base, startPos, startLoc, noCalls, forInit) {
      var maybeAsyncArrow = this.options.ecmaVersion >= 8 && base.type === "Identifier" && base.name === "async" && this.lastTokEnd === base.end && !this.canInsertSemicolon() && base.end - base.start === 5 && this.potentialArrowAt === base.start;
      var optionalChained = false;
      while (true) {
        var element = this.parseSubscript(base, startPos, startLoc, noCalls, maybeAsyncArrow, optionalChained, forInit);
        if (element.optional) {
          optionalChained = true;
        }
        if (element === base || element.type === "ArrowFunctionExpression") {
          if (optionalChained) {
            var chainNode = this.startNodeAt(startPos, startLoc);
            chainNode.expression = element;
            element = this.finishNode(chainNode, "ChainExpression");
          }
          return element;
        }
        base = element;
      }
    };
    pp$5.shouldParseAsyncArrow = function() {
      return !this.canInsertSemicolon() && this.eat(types$1.arrow);
    };
    pp$5.parseSubscriptAsyncArrow = function(startPos, startLoc, exprList, forInit) {
      return this.parseArrowExpression(this.startNodeAt(startPos, startLoc), exprList, true, forInit);
    };
    pp$5.parseSubscript = function(base, startPos, startLoc, noCalls, maybeAsyncArrow, optionalChained, forInit) {
      var optionalSupported = this.options.ecmaVersion >= 11;
      var optional = optionalSupported && this.eat(types$1.questionDot);
      if (noCalls && optional) {
        this.raise(this.lastTokStart, "Optional chaining cannot appear in the callee of new expressions");
      }
      var computed = this.eat(types$1.bracketL);
      if (computed || optional && this.type !== types$1.parenL && this.type !== types$1.backQuote || this.eat(types$1.dot)) {
        var node = this.startNodeAt(startPos, startLoc);
        node.object = base;
        if (computed) {
          node.property = this.parseExpression();
          this.expect(types$1.bracketR);
        } else if (this.type === types$1.privateId && base.type !== "Super") {
          node.property = this.parsePrivateIdent();
        } else {
          node.property = this.parseIdent(this.options.allowReserved !== "never");
        }
        node.computed = !!computed;
        if (optionalSupported) {
          node.optional = optional;
        }
        base = this.finishNode(node, "MemberExpression");
      } else if (!noCalls && this.eat(types$1.parenL)) {
        var refDestructuringErrors = new DestructuringErrors(), oldYieldPos = this.yieldPos, oldAwaitPos = this.awaitPos, oldAwaitIdentPos = this.awaitIdentPos;
        this.yieldPos = 0;
        this.awaitPos = 0;
        this.awaitIdentPos = 0;
        var exprList = this.parseExprList(types$1.parenR, this.options.ecmaVersion >= 8, false, refDestructuringErrors);
        if (maybeAsyncArrow && !optional && this.shouldParseAsyncArrow()) {
          this.checkPatternErrors(refDestructuringErrors, false);
          this.checkYieldAwaitInDefaultParams();
          if (this.awaitIdentPos > 0) {
            this.raise(this.awaitIdentPos, "Cannot use 'await' as identifier inside an async function");
          }
          this.yieldPos = oldYieldPos;
          this.awaitPos = oldAwaitPos;
          this.awaitIdentPos = oldAwaitIdentPos;
          return this.parseSubscriptAsyncArrow(startPos, startLoc, exprList, forInit);
        }
        this.checkExpressionErrors(refDestructuringErrors, true);
        this.yieldPos = oldYieldPos || this.yieldPos;
        this.awaitPos = oldAwaitPos || this.awaitPos;
        this.awaitIdentPos = oldAwaitIdentPos || this.awaitIdentPos;
        var node$1 = this.startNodeAt(startPos, startLoc);
        node$1.callee = base;
        node$1.arguments = exprList;
        if (optionalSupported) {
          node$1.optional = optional;
        }
        base = this.finishNode(node$1, "CallExpression");
      } else if (this.type === types$1.backQuote) {
        if (optional || optionalChained) {
          this.raise(this.start, "Optional chaining cannot appear in the tag of tagged template expressions");
        }
        var node$2 = this.startNodeAt(startPos, startLoc);
        node$2.tag = base;
        node$2.quasi = this.parseTemplate({ isTagged: true });
        base = this.finishNode(node$2, "TaggedTemplateExpression");
      }
      return base;
    };
    pp$5.parseExprAtom = function(refDestructuringErrors, forInit, forNew) {
      if (this.type === types$1.slash) {
        this.readRegexp();
      }
      var node, canBeArrow = this.potentialArrowAt === this.start;
      switch (this.type) {
        case types$1._super:
          if (!this.allowSuper) {
            this.raise(this.start, "'super' keyword outside a method");
          }
          node = this.startNode();
          this.next();
          if (this.type === types$1.parenL && !this.allowDirectSuper) {
            this.raise(node.start, "super() call outside constructor of a subclass");
          }
          if (this.type !== types$1.dot && this.type !== types$1.bracketL && this.type !== types$1.parenL) {
            this.unexpected();
          }
          return this.finishNode(node, "Super");
        case types$1._this:
          node = this.startNode();
          this.next();
          return this.finishNode(node, "ThisExpression");
        case types$1.name:
          var startPos = this.start, startLoc = this.startLoc, containsEsc = this.containsEsc;
          var id = this.parseIdent(false);
          if (this.options.ecmaVersion >= 8 && !containsEsc && id.name === "async" && !this.canInsertSemicolon() && this.eat(types$1._function)) {
            this.overrideContext(types2.f_expr);
            return this.parseFunction(this.startNodeAt(startPos, startLoc), 0, false, true, forInit);
          }
          if (canBeArrow && !this.canInsertSemicolon()) {
            if (this.eat(types$1.arrow)) {
              return this.parseArrowExpression(this.startNodeAt(startPos, startLoc), [id], false, forInit);
            }
            if (this.options.ecmaVersion >= 8 && id.name === "async" && this.type === types$1.name && !containsEsc && (!this.potentialArrowInForAwait || this.value !== "of" || this.containsEsc)) {
              id = this.parseIdent(false);
              if (this.canInsertSemicolon() || !this.eat(types$1.arrow)) {
                this.unexpected();
              }
              return this.parseArrowExpression(this.startNodeAt(startPos, startLoc), [id], true, forInit);
            }
          }
          return id;
        case types$1.regexp:
          var value = this.value;
          node = this.parseLiteral(value.value);
          node.regex = { pattern: value.pattern, flags: value.flags };
          return node;
        case types$1.num:
        case types$1.string:
          return this.parseLiteral(this.value);
        case types$1._null:
        case types$1._true:
        case types$1._false:
          node = this.startNode();
          node.value = this.type === types$1._null ? null : this.type === types$1._true;
          node.raw = this.type.keyword;
          this.next();
          return this.finishNode(node, "Literal");
        case types$1.parenL:
          var start = this.start, expr = this.parseParenAndDistinguishExpression(canBeArrow, forInit);
          if (refDestructuringErrors) {
            if (refDestructuringErrors.parenthesizedAssign < 0 && !this.isSimpleAssignTarget(expr)) {
              refDestructuringErrors.parenthesizedAssign = start;
            }
            if (refDestructuringErrors.parenthesizedBind < 0) {
              refDestructuringErrors.parenthesizedBind = start;
            }
          }
          return expr;
        case types$1.bracketL:
          node = this.startNode();
          this.next();
          node.elements = this.parseExprList(types$1.bracketR, true, true, refDestructuringErrors);
          return this.finishNode(node, "ArrayExpression");
        case types$1.braceL:
          this.overrideContext(types2.b_expr);
          return this.parseObj(false, refDestructuringErrors);
        case types$1._function:
          node = this.startNode();
          this.next();
          return this.parseFunction(node, 0);
        case types$1._class:
          return this.parseClass(this.startNode(), false);
        case types$1._new:
          return this.parseNew();
        case types$1.backQuote:
          return this.parseTemplate();
        case types$1._import:
          if (this.options.ecmaVersion >= 11) {
            return this.parseExprImport(forNew);
          } else {
            return this.unexpected();
          }
        default:
          return this.parseExprAtomDefault();
      }
    };
    pp$5.parseExprAtomDefault = function() {
      this.unexpected();
    };
    pp$5.parseExprImport = function(forNew) {
      var node = this.startNode();
      if (this.containsEsc) {
        this.raiseRecoverable(this.start, "Escape sequence in keyword import");
      }
      this.next();
      if (this.type === types$1.parenL && !forNew) {
        return this.parseDynamicImport(node);
      } else if (this.type === types$1.dot) {
        var meta = this.startNodeAt(node.start, node.loc && node.loc.start);
        meta.name = "import";
        node.meta = this.finishNode(meta, "Identifier");
        return this.parseImportMeta(node);
      } else {
        this.unexpected();
      }
    };
    pp$5.parseDynamicImport = function(node) {
      this.next();
      node.source = this.parseMaybeAssign();
      if (this.options.ecmaVersion >= 16) {
        if (!this.eat(types$1.parenR)) {
          this.expect(types$1.comma);
          if (!this.afterTrailingComma(types$1.parenR)) {
            node.options = this.parseMaybeAssign();
            if (!this.eat(types$1.parenR)) {
              this.expect(types$1.comma);
              if (!this.afterTrailingComma(types$1.parenR)) {
                this.unexpected();
              }
            }
          } else {
            node.options = null;
          }
        } else {
          node.options = null;
        }
      } else {
        if (!this.eat(types$1.parenR)) {
          var errorPos = this.start;
          if (this.eat(types$1.comma) && this.eat(types$1.parenR)) {
            this.raiseRecoverable(errorPos, "Trailing comma is not allowed in import()");
          } else {
            this.unexpected(errorPos);
          }
        }
      }
      return this.finishNode(node, "ImportExpression");
    };
    pp$5.parseImportMeta = function(node) {
      this.next();
      var containsEsc = this.containsEsc;
      node.property = this.parseIdent(true);
      if (node.property.name !== "meta") {
        this.raiseRecoverable(node.property.start, "The only valid meta property for import is 'import.meta'");
      }
      if (containsEsc) {
        this.raiseRecoverable(node.start, "'import.meta' must not contain escaped characters");
      }
      if (this.options.sourceType !== "module" && !this.options.allowImportExportEverywhere) {
        this.raiseRecoverable(node.start, "Cannot use 'import.meta' outside a module");
      }
      return this.finishNode(node, "MetaProperty");
    };
    pp$5.parseLiteral = function(value) {
      var node = this.startNode();
      node.value = value;
      node.raw = this.input.slice(this.start, this.end);
      if (node.raw.charCodeAt(node.raw.length - 1) === 110) {
        node.bigint = node.value != null ? node.value.toString() : node.raw.slice(0, -1).replace(/_/g, "");
      }
      this.next();
      return this.finishNode(node, "Literal");
    };
    pp$5.parseParenExpression = function() {
      this.expect(types$1.parenL);
      var val = this.parseExpression();
      this.expect(types$1.parenR);
      return val;
    };
    pp$5.shouldParseArrow = function(exprList) {
      return !this.canInsertSemicolon();
    };
    pp$5.parseParenAndDistinguishExpression = function(canBeArrow, forInit) {
      var startPos = this.start, startLoc = this.startLoc, val, allowTrailingComma = this.options.ecmaVersion >= 8;
      if (this.options.ecmaVersion >= 6) {
        this.next();
        var innerStartPos = this.start, innerStartLoc = this.startLoc;
        var exprList = [], first = true, lastIsComma = false;
        var refDestructuringErrors = new DestructuringErrors(), oldYieldPos = this.yieldPos, oldAwaitPos = this.awaitPos, spreadStart;
        this.yieldPos = 0;
        this.awaitPos = 0;
        while (this.type !== types$1.parenR) {
          first ? first = false : this.expect(types$1.comma);
          if (allowTrailingComma && this.afterTrailingComma(types$1.parenR, true)) {
            lastIsComma = true;
            break;
          } else if (this.type === types$1.ellipsis) {
            spreadStart = this.start;
            exprList.push(this.parseParenItem(this.parseRestBinding()));
            if (this.type === types$1.comma) {
              this.raiseRecoverable(
                this.start,
                "Comma is not permitted after the rest element"
              );
            }
            break;
          } else {
            exprList.push(this.parseMaybeAssign(false, refDestructuringErrors, this.parseParenItem));
          }
        }
        var innerEndPos = this.lastTokEnd, innerEndLoc = this.lastTokEndLoc;
        this.expect(types$1.parenR);
        if (canBeArrow && this.shouldParseArrow(exprList) && this.eat(types$1.arrow)) {
          this.checkPatternErrors(refDestructuringErrors, false);
          this.checkYieldAwaitInDefaultParams();
          this.yieldPos = oldYieldPos;
          this.awaitPos = oldAwaitPos;
          return this.parseParenArrowList(startPos, startLoc, exprList, forInit);
        }
        if (!exprList.length || lastIsComma) {
          this.unexpected(this.lastTokStart);
        }
        if (spreadStart) {
          this.unexpected(spreadStart);
        }
        this.checkExpressionErrors(refDestructuringErrors, true);
        this.yieldPos = oldYieldPos || this.yieldPos;
        this.awaitPos = oldAwaitPos || this.awaitPos;
        if (exprList.length > 1) {
          val = this.startNodeAt(innerStartPos, innerStartLoc);
          val.expressions = exprList;
          this.finishNodeAt(val, "SequenceExpression", innerEndPos, innerEndLoc);
        } else {
          val = exprList[0];
        }
      } else {
        val = this.parseParenExpression();
      }
      if (this.options.preserveParens) {
        var par = this.startNodeAt(startPos, startLoc);
        par.expression = val;
        return this.finishNode(par, "ParenthesizedExpression");
      } else {
        return val;
      }
    };
    pp$5.parseParenItem = function(item) {
      return item;
    };
    pp$5.parseParenArrowList = function(startPos, startLoc, exprList, forInit) {
      return this.parseArrowExpression(this.startNodeAt(startPos, startLoc), exprList, false, forInit);
    };
    empty = [];
    pp$5.parseNew = function() {
      if (this.containsEsc) {
        this.raiseRecoverable(this.start, "Escape sequence in keyword new");
      }
      var node = this.startNode();
      this.next();
      if (this.options.ecmaVersion >= 6 && this.type === types$1.dot) {
        var meta = this.startNodeAt(node.start, node.loc && node.loc.start);
        meta.name = "new";
        node.meta = this.finishNode(meta, "Identifier");
        this.next();
        var containsEsc = this.containsEsc;
        node.property = this.parseIdent(true);
        if (node.property.name !== "target") {
          this.raiseRecoverable(node.property.start, "The only valid meta property for new is 'new.target'");
        }
        if (containsEsc) {
          this.raiseRecoverable(node.start, "'new.target' must not contain escaped characters");
        }
        if (!this.allowNewDotTarget) {
          this.raiseRecoverable(node.start, "'new.target' can only be used in functions and class static block");
        }
        return this.finishNode(node, "MetaProperty");
      }
      var startPos = this.start, startLoc = this.startLoc;
      node.callee = this.parseSubscripts(this.parseExprAtom(null, false, true), startPos, startLoc, true, false);
      if (node.callee.type === "Super") {
        this.raiseRecoverable(startPos, "Invalid use of 'super'");
      }
      if (this.eat(types$1.parenL)) {
        node.arguments = this.parseExprList(types$1.parenR, this.options.ecmaVersion >= 8, false);
      } else {
        node.arguments = empty;
      }
      return this.finishNode(node, "NewExpression");
    };
    pp$5.parseTemplateElement = function(ref2) {
      var isTagged = ref2.isTagged;
      var elem = this.startNode();
      if (this.type === types$1.invalidTemplate) {
        if (!isTagged) {
          this.raiseRecoverable(this.start, "Bad escape sequence in untagged template literal");
        }
        elem.value = {
          raw: this.value.replace(/\r\n?/g, "\n"),
          cooked: null
        };
      } else {
        elem.value = {
          raw: this.input.slice(this.start, this.end).replace(/\r\n?/g, "\n"),
          cooked: this.value
        };
      }
      this.next();
      elem.tail = this.type === types$1.backQuote;
      return this.finishNode(elem, "TemplateElement");
    };
    pp$5.parseTemplate = function(ref2) {
      if (ref2 === void 0) ref2 = {};
      var isTagged = ref2.isTagged;
      if (isTagged === void 0) isTagged = false;
      var node = this.startNode();
      this.next();
      node.expressions = [];
      var curElt = this.parseTemplateElement({ isTagged });
      node.quasis = [curElt];
      while (!curElt.tail) {
        if (this.type === types$1.eof) {
          this.raise(this.pos, "Unterminated template literal");
        }
        this.expect(types$1.dollarBraceL);
        node.expressions.push(this.parseExpression());
        this.expect(types$1.braceR);
        node.quasis.push(curElt = this.parseTemplateElement({ isTagged }));
      }
      this.next();
      return this.finishNode(node, "TemplateLiteral");
    };
    pp$5.isAsyncProp = function(prop) {
      return !prop.computed && prop.key.type === "Identifier" && prop.key.name === "async" && (this.type === types$1.name || this.type === types$1.num || this.type === types$1.string || this.type === types$1.bracketL || this.type.keyword || this.options.ecmaVersion >= 9 && this.type === types$1.star) && !lineBreak.test(this.input.slice(this.lastTokEnd, this.start));
    };
    pp$5.parseObj = function(isPattern, refDestructuringErrors) {
      var node = this.startNode(), first = true, propHash = {};
      node.properties = [];
      this.next();
      while (!this.eat(types$1.braceR)) {
        if (!first) {
          this.expect(types$1.comma);
          if (this.options.ecmaVersion >= 5 && this.afterTrailingComma(types$1.braceR)) {
            break;
          }
        } else {
          first = false;
        }
        var prop = this.parseProperty(isPattern, refDestructuringErrors);
        if (!isPattern) {
          this.checkPropClash(prop, propHash, refDestructuringErrors);
        }
        node.properties.push(prop);
      }
      return this.finishNode(node, isPattern ? "ObjectPattern" : "ObjectExpression");
    };
    pp$5.parseProperty = function(isPattern, refDestructuringErrors) {
      var prop = this.startNode(), isGenerator, isAsync, startPos, startLoc;
      if (this.options.ecmaVersion >= 9 && this.eat(types$1.ellipsis)) {
        if (isPattern) {
          prop.argument = this.parseIdent(false);
          if (this.type === types$1.comma) {
            this.raiseRecoverable(this.start, "Comma is not permitted after the rest element");
          }
          return this.finishNode(prop, "RestElement");
        }
        prop.argument = this.parseMaybeAssign(false, refDestructuringErrors);
        if (this.type === types$1.comma && refDestructuringErrors && refDestructuringErrors.trailingComma < 0) {
          refDestructuringErrors.trailingComma = this.start;
        }
        return this.finishNode(prop, "SpreadElement");
      }
      if (this.options.ecmaVersion >= 6) {
        prop.method = false;
        prop.shorthand = false;
        if (isPattern || refDestructuringErrors) {
          startPos = this.start;
          startLoc = this.startLoc;
        }
        if (!isPattern) {
          isGenerator = this.eat(types$1.star);
        }
      }
      var containsEsc = this.containsEsc;
      this.parsePropertyName(prop);
      if (!isPattern && !containsEsc && this.options.ecmaVersion >= 8 && !isGenerator && this.isAsyncProp(prop)) {
        isAsync = true;
        isGenerator = this.options.ecmaVersion >= 9 && this.eat(types$1.star);
        this.parsePropertyName(prop);
      } else {
        isAsync = false;
      }
      this.parsePropertyValue(prop, isPattern, isGenerator, isAsync, startPos, startLoc, refDestructuringErrors, containsEsc);
      return this.finishNode(prop, "Property");
    };
    pp$5.parseGetterSetter = function(prop) {
      var kind = prop.key.name;
      this.parsePropertyName(prop);
      prop.value = this.parseMethod(false);
      prop.kind = kind;
      var paramCount = prop.kind === "get" ? 0 : 1;
      if (prop.value.params.length !== paramCount) {
        var start = prop.value.start;
        if (prop.kind === "get") {
          this.raiseRecoverable(start, "getter should have no params");
        } else {
          this.raiseRecoverable(start, "setter should have exactly one param");
        }
      } else {
        if (prop.kind === "set" && prop.value.params[0].type === "RestElement") {
          this.raiseRecoverable(prop.value.params[0].start, "Setter cannot use rest params");
        }
      }
    };
    pp$5.parsePropertyValue = function(prop, isPattern, isGenerator, isAsync, startPos, startLoc, refDestructuringErrors, containsEsc) {
      if ((isGenerator || isAsync) && this.type === types$1.colon) {
        this.unexpected();
      }
      if (this.eat(types$1.colon)) {
        prop.value = isPattern ? this.parseMaybeDefault(this.start, this.startLoc) : this.parseMaybeAssign(false, refDestructuringErrors);
        prop.kind = "init";
      } else if (this.options.ecmaVersion >= 6 && this.type === types$1.parenL) {
        if (isPattern) {
          this.unexpected();
        }
        prop.method = true;
        prop.value = this.parseMethod(isGenerator, isAsync);
        prop.kind = "init";
      } else if (!isPattern && !containsEsc && this.options.ecmaVersion >= 5 && !prop.computed && prop.key.type === "Identifier" && (prop.key.name === "get" || prop.key.name === "set") && (this.type !== types$1.comma && this.type !== types$1.braceR && this.type !== types$1.eq)) {
        if (isGenerator || isAsync) {
          this.unexpected();
        }
        this.parseGetterSetter(prop);
      } else if (this.options.ecmaVersion >= 6 && !prop.computed && prop.key.type === "Identifier") {
        if (isGenerator || isAsync) {
          this.unexpected();
        }
        this.checkUnreserved(prop.key);
        if (prop.key.name === "await" && !this.awaitIdentPos) {
          this.awaitIdentPos = startPos;
        }
        if (isPattern) {
          prop.value = this.parseMaybeDefault(startPos, startLoc, this.copyNode(prop.key));
        } else if (this.type === types$1.eq && refDestructuringErrors) {
          if (refDestructuringErrors.shorthandAssign < 0) {
            refDestructuringErrors.shorthandAssign = this.start;
          }
          prop.value = this.parseMaybeDefault(startPos, startLoc, this.copyNode(prop.key));
        } else {
          prop.value = this.copyNode(prop.key);
        }
        prop.kind = "init";
        prop.shorthand = true;
      } else {
        this.unexpected();
      }
    };
    pp$5.parsePropertyName = function(prop) {
      if (this.options.ecmaVersion >= 6) {
        if (this.eat(types$1.bracketL)) {
          prop.computed = true;
          prop.key = this.parseMaybeAssign();
          this.expect(types$1.bracketR);
          return prop.key;
        } else {
          prop.computed = false;
        }
      }
      return prop.key = this.type === types$1.num || this.type === types$1.string ? this.parseExprAtom() : this.parseIdent(this.options.allowReserved !== "never");
    };
    pp$5.initFunction = function(node) {
      node.id = null;
      if (this.options.ecmaVersion >= 6) {
        node.generator = node.expression = false;
      }
      if (this.options.ecmaVersion >= 8) {
        node.async = false;
      }
    };
    pp$5.parseMethod = function(isGenerator, isAsync, allowDirectSuper) {
      var node = this.startNode(), oldYieldPos = this.yieldPos, oldAwaitPos = this.awaitPos, oldAwaitIdentPos = this.awaitIdentPos;
      this.initFunction(node);
      if (this.options.ecmaVersion >= 6) {
        node.generator = isGenerator;
      }
      if (this.options.ecmaVersion >= 8) {
        node.async = !!isAsync;
      }
      this.yieldPos = 0;
      this.awaitPos = 0;
      this.awaitIdentPos = 0;
      this.enterScope(functionFlags(isAsync, node.generator) | SCOPE_SUPER | (allowDirectSuper ? SCOPE_DIRECT_SUPER : 0));
      this.expect(types$1.parenL);
      node.params = this.parseBindingList(types$1.parenR, false, this.options.ecmaVersion >= 8);
      this.checkYieldAwaitInDefaultParams();
      this.parseFunctionBody(node, false, true, false);
      this.yieldPos = oldYieldPos;
      this.awaitPos = oldAwaitPos;
      this.awaitIdentPos = oldAwaitIdentPos;
      return this.finishNode(node, "FunctionExpression");
    };
    pp$5.parseArrowExpression = function(node, params, isAsync, forInit) {
      var oldYieldPos = this.yieldPos, oldAwaitPos = this.awaitPos, oldAwaitIdentPos = this.awaitIdentPos;
      this.enterScope(functionFlags(isAsync, false) | SCOPE_ARROW);
      this.initFunction(node);
      if (this.options.ecmaVersion >= 8) {
        node.async = !!isAsync;
      }
      this.yieldPos = 0;
      this.awaitPos = 0;
      this.awaitIdentPos = 0;
      node.params = this.toAssignableList(params, true);
      this.parseFunctionBody(node, true, false, forInit);
      this.yieldPos = oldYieldPos;
      this.awaitPos = oldAwaitPos;
      this.awaitIdentPos = oldAwaitIdentPos;
      return this.finishNode(node, "ArrowFunctionExpression");
    };
    pp$5.parseFunctionBody = function(node, isArrowFunction, isMethod, forInit) {
      var isExpression = isArrowFunction && this.type !== types$1.braceL;
      var oldStrict = this.strict, useStrict = false;
      if (isExpression) {
        node.body = this.parseMaybeAssign(forInit);
        node.expression = true;
        this.checkParams(node, false);
      } else {
        var nonSimple = this.options.ecmaVersion >= 7 && !this.isSimpleParamList(node.params);
        if (!oldStrict || nonSimple) {
          useStrict = this.strictDirective(this.end);
          if (useStrict && nonSimple) {
            this.raiseRecoverable(node.start, "Illegal 'use strict' directive in function with non-simple parameter list");
          }
        }
        var oldLabels = this.labels;
        this.labels = [];
        if (useStrict) {
          this.strict = true;
        }
        this.checkParams(node, !oldStrict && !useStrict && !isArrowFunction && !isMethod && this.isSimpleParamList(node.params));
        if (this.strict && node.id) {
          this.checkLValSimple(node.id, BIND_OUTSIDE);
        }
        node.body = this.parseBlock(false, void 0, useStrict && !oldStrict);
        node.expression = false;
        this.adaptDirectivePrologue(node.body.body);
        this.labels = oldLabels;
      }
      this.exitScope();
    };
    pp$5.isSimpleParamList = function(params) {
      for (var i2 = 0, list2 = params; i2 < list2.length; i2 += 1) {
        var param = list2[i2];
        if (param.type !== "Identifier") {
          return false;
        }
      }
      return true;
    };
    pp$5.checkParams = function(node, allowDuplicates) {
      var nameHash = /* @__PURE__ */ Object.create(null);
      for (var i2 = 0, list2 = node.params; i2 < list2.length; i2 += 1) {
        var param = list2[i2];
        this.checkLValInnerPattern(param, BIND_VAR, allowDuplicates ? null : nameHash);
      }
    };
    pp$5.parseExprList = function(close, allowTrailingComma, allowEmpty, refDestructuringErrors) {
      var elts = [], first = true;
      while (!this.eat(close)) {
        if (!first) {
          this.expect(types$1.comma);
          if (allowTrailingComma && this.afterTrailingComma(close)) {
            break;
          }
        } else {
          first = false;
        }
        var elt = void 0;
        if (allowEmpty && this.type === types$1.comma) {
          elt = null;
        } else if (this.type === types$1.ellipsis) {
          elt = this.parseSpread(refDestructuringErrors);
          if (refDestructuringErrors && this.type === types$1.comma && refDestructuringErrors.trailingComma < 0) {
            refDestructuringErrors.trailingComma = this.start;
          }
        } else {
          elt = this.parseMaybeAssign(false, refDestructuringErrors);
        }
        elts.push(elt);
      }
      return elts;
    };
    pp$5.checkUnreserved = function(ref2) {
      var start = ref2.start;
      var end = ref2.end;
      var name = ref2.name;
      if (this.inGenerator && name === "yield") {
        this.raiseRecoverable(start, "Cannot use 'yield' as identifier inside a generator");
      }
      if (this.inAsync && name === "await") {
        this.raiseRecoverable(start, "Cannot use 'await' as identifier inside an async function");
      }
      if (!(this.currentThisScope().flags & SCOPE_VAR) && name === "arguments") {
        this.raiseRecoverable(start, "Cannot use 'arguments' in class field initializer");
      }
      if (this.inClassStaticBlock && (name === "arguments" || name === "await")) {
        this.raise(start, "Cannot use " + name + " in class static initialization block");
      }
      if (this.keywords.test(name)) {
        this.raise(start, "Unexpected keyword '" + name + "'");
      }
      if (this.options.ecmaVersion < 6 && this.input.slice(start, end).indexOf("\\") !== -1) {
        return;
      }
      var re = this.strict ? this.reservedWordsStrict : this.reservedWords;
      if (re.test(name)) {
        if (!this.inAsync && name === "await") {
          this.raiseRecoverable(start, "Cannot use keyword 'await' outside an async function");
        }
        this.raiseRecoverable(start, "The keyword '" + name + "' is reserved");
      }
    };
    pp$5.parseIdent = function(liberal) {
      var node = this.parseIdentNode();
      this.next(!!liberal);
      this.finishNode(node, "Identifier");
      if (!liberal) {
        this.checkUnreserved(node);
        if (node.name === "await" && !this.awaitIdentPos) {
          this.awaitIdentPos = node.start;
        }
      }
      return node;
    };
    pp$5.parseIdentNode = function() {
      var node = this.startNode();
      if (this.type === types$1.name) {
        node.name = this.value;
      } else if (this.type.keyword) {
        node.name = this.type.keyword;
        if ((node.name === "class" || node.name === "function") && (this.lastTokEnd !== this.lastTokStart + 1 || this.input.charCodeAt(this.lastTokStart) !== 46)) {
          this.context.pop();
        }
        this.type = types$1.name;
      } else {
        this.unexpected();
      }
      return node;
    };
    pp$5.parsePrivateIdent = function() {
      var node = this.startNode();
      if (this.type === types$1.privateId) {
        node.name = this.value;
      } else {
        this.unexpected();
      }
      this.next();
      this.finishNode(node, "PrivateIdentifier");
      if (this.options.checkPrivateFields) {
        if (this.privateNameStack.length === 0) {
          this.raise(node.start, "Private field '#" + node.name + "' must be declared in an enclosing class");
        } else {
          this.privateNameStack[this.privateNameStack.length - 1].used.push(node);
        }
      }
      return node;
    };
    pp$5.parseYield = function(forInit) {
      if (!this.yieldPos) {
        this.yieldPos = this.start;
      }
      var node = this.startNode();
      this.next();
      if (this.type === types$1.semi || this.canInsertSemicolon() || this.type !== types$1.star && !this.type.startsExpr) {
        node.delegate = false;
        node.argument = null;
      } else {
        node.delegate = this.eat(types$1.star);
        node.argument = this.parseMaybeAssign(forInit);
      }
      return this.finishNode(node, "YieldExpression");
    };
    pp$5.parseAwait = function(forInit) {
      if (!this.awaitPos) {
        this.awaitPos = this.start;
      }
      var node = this.startNode();
      this.next();
      node.argument = this.parseMaybeUnary(null, true, false, forInit);
      return this.finishNode(node, "AwaitExpression");
    };
    pp$4 = Parser.prototype;
    pp$4.raise = function(pos, message) {
      var loc = getLineInfo(this.input, pos);
      message += " (" + loc.line + ":" + loc.column + ")";
      if (this.sourceFile) {
        message += " in " + this.sourceFile;
      }
      var err = new SyntaxError(message);
      err.pos = pos;
      err.loc = loc;
      err.raisedAt = this.pos;
      throw err;
    };
    pp$4.raiseRecoverable = pp$4.raise;
    pp$4.curPosition = function() {
      if (this.options.locations) {
        return new Position(this.curLine, this.pos - this.lineStart);
      }
    };
    pp$3 = Parser.prototype;
    Scope = function Scope2(flags) {
      this.flags = flags;
      this.var = [];
      this.lexical = [];
      this.functions = [];
    };
    pp$3.enterScope = function(flags) {
      this.scopeStack.push(new Scope(flags));
    };
    pp$3.exitScope = function() {
      this.scopeStack.pop();
    };
    pp$3.treatFunctionsAsVarInScope = function(scope) {
      return scope.flags & SCOPE_FUNCTION || !this.inModule && scope.flags & SCOPE_TOP;
    };
    pp$3.declareName = function(name, bindingType, pos) {
      var redeclared = false;
      if (bindingType === BIND_LEXICAL) {
        var scope = this.currentScope();
        redeclared = scope.lexical.indexOf(name) > -1 || scope.functions.indexOf(name) > -1 || scope.var.indexOf(name) > -1;
        scope.lexical.push(name);
        if (this.inModule && scope.flags & SCOPE_TOP) {
          delete this.undefinedExports[name];
        }
      } else if (bindingType === BIND_SIMPLE_CATCH) {
        var scope$1 = this.currentScope();
        scope$1.lexical.push(name);
      } else if (bindingType === BIND_FUNCTION) {
        var scope$2 = this.currentScope();
        if (this.treatFunctionsAsVar) {
          redeclared = scope$2.lexical.indexOf(name) > -1;
        } else {
          redeclared = scope$2.lexical.indexOf(name) > -1 || scope$2.var.indexOf(name) > -1;
        }
        scope$2.functions.push(name);
      } else {
        for (var i2 = this.scopeStack.length - 1; i2 >= 0; --i2) {
          var scope$3 = this.scopeStack[i2];
          if (scope$3.lexical.indexOf(name) > -1 && !(scope$3.flags & SCOPE_SIMPLE_CATCH && scope$3.lexical[0] === name) || !this.treatFunctionsAsVarInScope(scope$3) && scope$3.functions.indexOf(name) > -1) {
            redeclared = true;
            break;
          }
          scope$3.var.push(name);
          if (this.inModule && scope$3.flags & SCOPE_TOP) {
            delete this.undefinedExports[name];
          }
          if (scope$3.flags & SCOPE_VAR) {
            break;
          }
        }
      }
      if (redeclared) {
        this.raiseRecoverable(pos, "Identifier '" + name + "' has already been declared");
      }
    };
    pp$3.checkLocalExport = function(id) {
      if (this.scopeStack[0].lexical.indexOf(id.name) === -1 && this.scopeStack[0].var.indexOf(id.name) === -1) {
        this.undefinedExports[id.name] = id;
      }
    };
    pp$3.currentScope = function() {
      return this.scopeStack[this.scopeStack.length - 1];
    };
    pp$3.currentVarScope = function() {
      for (var i2 = this.scopeStack.length - 1; ; i2--) {
        var scope = this.scopeStack[i2];
        if (scope.flags & (SCOPE_VAR | SCOPE_CLASS_FIELD_INIT | SCOPE_CLASS_STATIC_BLOCK)) {
          return scope;
        }
      }
    };
    pp$3.currentThisScope = function() {
      for (var i2 = this.scopeStack.length - 1; ; i2--) {
        var scope = this.scopeStack[i2];
        if (scope.flags & (SCOPE_VAR | SCOPE_CLASS_FIELD_INIT | SCOPE_CLASS_STATIC_BLOCK) && !(scope.flags & SCOPE_ARROW)) {
          return scope;
        }
      }
    };
    Node = function Node2(parser, pos, loc) {
      this.type = "";
      this.start = pos;
      this.end = 0;
      if (parser.options.locations) {
        this.loc = new SourceLocation(parser, loc);
      }
      if (parser.options.directSourceFile) {
        this.sourceFile = parser.options.directSourceFile;
      }
      if (parser.options.ranges) {
        this.range = [pos, 0];
      }
    };
    pp$2 = Parser.prototype;
    pp$2.startNode = function() {
      return new Node(this, this.start, this.startLoc);
    };
    pp$2.startNodeAt = function(pos, loc) {
      return new Node(this, pos, loc);
    };
    pp$2.finishNode = function(node, type2) {
      return finishNodeAt.call(this, node, type2, this.lastTokEnd, this.lastTokEndLoc);
    };
    pp$2.finishNodeAt = function(node, type2, pos, loc) {
      return finishNodeAt.call(this, node, type2, pos, loc);
    };
    pp$2.copyNode = function(node) {
      var newNode = new Node(this, node.start, this.startLoc);
      for (var prop in node) {
        newNode[prop] = node[prop];
      }
      return newNode;
    };
    scriptValuesAddedInUnicode = "Berf Beria_Erfe Gara Garay Gukh Gurung_Khema Hrkt Katakana_Or_Hiragana Kawi Kirat_Rai Krai Nag_Mundari Nagm Ol_Onal Onao Sidetic Sidt Sunu Sunuwar Tai_Yo Tayo Todhri Todr Tolong_Siki Tols Tulu_Tigalari Tutg Unknown Zzzz";
    ecma9BinaryProperties = "ASCII ASCII_Hex_Digit AHex Alphabetic Alpha Any Assigned Bidi_Control Bidi_C Bidi_Mirrored Bidi_M Case_Ignorable CI Cased Changes_When_Casefolded CWCF Changes_When_Casemapped CWCM Changes_When_Lowercased CWL Changes_When_NFKC_Casefolded CWKCF Changes_When_Titlecased CWT Changes_When_Uppercased CWU Dash Default_Ignorable_Code_Point DI Deprecated Dep Diacritic Dia Emoji Emoji_Component Emoji_Modifier Emoji_Modifier_Base Emoji_Presentation Extender Ext Grapheme_Base Gr_Base Grapheme_Extend Gr_Ext Hex_Digit Hex IDS_Binary_Operator IDSB IDS_Trinary_Operator IDST ID_Continue IDC ID_Start IDS Ideographic Ideo Join_Control Join_C Logical_Order_Exception LOE Lowercase Lower Math Noncharacter_Code_Point NChar Pattern_Syntax Pat_Syn Pattern_White_Space Pat_WS Quotation_Mark QMark Radical Regional_Indicator RI Sentence_Terminal STerm Soft_Dotted SD Terminal_Punctuation Term Unified_Ideograph UIdeo Uppercase Upper Variation_Selector VS White_Space space XID_Continue XIDC XID_Start XIDS";
    ecma10BinaryProperties = ecma9BinaryProperties + " Extended_Pictographic";
    ecma11BinaryProperties = ecma10BinaryProperties;
    ecma12BinaryProperties = ecma11BinaryProperties + " EBase EComp EMod EPres ExtPict";
    ecma13BinaryProperties = ecma12BinaryProperties;
    ecma14BinaryProperties = ecma13BinaryProperties;
    unicodeBinaryProperties = {
      9: ecma9BinaryProperties,
      10: ecma10BinaryProperties,
      11: ecma11BinaryProperties,
      12: ecma12BinaryProperties,
      13: ecma13BinaryProperties,
      14: ecma14BinaryProperties
    };
    ecma14BinaryPropertiesOfStrings = "Basic_Emoji Emoji_Keycap_Sequence RGI_Emoji_Modifier_Sequence RGI_Emoji_Flag_Sequence RGI_Emoji_Tag_Sequence RGI_Emoji_ZWJ_Sequence RGI_Emoji";
    unicodeBinaryPropertiesOfStrings = {
      9: "",
      10: "",
      11: "",
      12: "",
      13: "",
      14: ecma14BinaryPropertiesOfStrings
    };
    unicodeGeneralCategoryValues = "Cased_Letter LC Close_Punctuation Pe Connector_Punctuation Pc Control Cc cntrl Currency_Symbol Sc Dash_Punctuation Pd Decimal_Number Nd digit Enclosing_Mark Me Final_Punctuation Pf Format Cf Initial_Punctuation Pi Letter L Letter_Number Nl Line_Separator Zl Lowercase_Letter Ll Mark M Combining_Mark Math_Symbol Sm Modifier_Letter Lm Modifier_Symbol Sk Nonspacing_Mark Mn Number N Open_Punctuation Ps Other C Other_Letter Lo Other_Number No Other_Punctuation Po Other_Symbol So Paragraph_Separator Zp Private_Use Co Punctuation P punct Separator Z Space_Separator Zs Spacing_Mark Mc Surrogate Cs Symbol S Titlecase_Letter Lt Unassigned Cn Uppercase_Letter Lu";
    ecma9ScriptValues = "Adlam Adlm Ahom Anatolian_Hieroglyphs Hluw Arabic Arab Armenian Armn Avestan Avst Balinese Bali Bamum Bamu Bassa_Vah Bass Batak Batk Bengali Beng Bhaiksuki Bhks Bopomofo Bopo Brahmi Brah Braille Brai Buginese Bugi Buhid Buhd Canadian_Aboriginal Cans Carian Cari Caucasian_Albanian Aghb Chakma Cakm Cham Cham Cherokee Cher Common Zyyy Coptic Copt Qaac Cuneiform Xsux Cypriot Cprt Cyrillic Cyrl Deseret Dsrt Devanagari Deva Duployan Dupl Egyptian_Hieroglyphs Egyp Elbasan Elba Ethiopic Ethi Georgian Geor Glagolitic Glag Gothic Goth Grantha Gran Greek Grek Gujarati Gujr Gurmukhi Guru Han Hani Hangul Hang Hanunoo Hano Hatran Hatr Hebrew Hebr Hiragana Hira Imperial_Aramaic Armi Inherited Zinh Qaai Inscriptional_Pahlavi Phli Inscriptional_Parthian Prti Javanese Java Kaithi Kthi Kannada Knda Katakana Kana Kayah_Li Kali Kharoshthi Khar Khmer Khmr Khojki Khoj Khudawadi Sind Lao Laoo Latin Latn Lepcha Lepc Limbu Limb Linear_A Lina Linear_B Linb Lisu Lisu Lycian Lyci Lydian Lydi Mahajani Mahj Malayalam Mlym Mandaic Mand Manichaean Mani Marchen Marc Masaram_Gondi Gonm Meetei_Mayek Mtei Mende_Kikakui Mend Meroitic_Cursive Merc Meroitic_Hieroglyphs Mero Miao Plrd Modi Mongolian Mong Mro Mroo Multani Mult Myanmar Mymr Nabataean Nbat New_Tai_Lue Talu Newa Newa Nko Nkoo Nushu Nshu Ogham Ogam Ol_Chiki Olck Old_Hungarian Hung Old_Italic Ital Old_North_Arabian Narb Old_Permic Perm Old_Persian Xpeo Old_South_Arabian Sarb Old_Turkic Orkh Oriya Orya Osage Osge Osmanya Osma Pahawh_Hmong Hmng Palmyrene Palm Pau_Cin_Hau Pauc Phags_Pa Phag Phoenician Phnx Psalter_Pahlavi Phlp Rejang Rjng Runic Runr Samaritan Samr Saurashtra Saur Sharada Shrd Shavian Shaw Siddham Sidd SignWriting Sgnw Sinhala Sinh Sora_Sompeng Sora Soyombo Soyo Sundanese Sund Syloti_Nagri Sylo Syriac Syrc Tagalog Tglg Tagbanwa Tagb Tai_Le Tale Tai_Tham Lana Tai_Viet Tavt Takri Takr Tamil Taml Tangut Tang Telugu Telu Thaana Thaa Thai Thai Tibetan Tibt Tifinagh Tfng Tirhuta Tirh Ugaritic Ugar Vai Vaii Warang_Citi Wara Yi Yiii Zanabazar_Square Zanb";
    ecma10ScriptValues = ecma9ScriptValues + " Dogra Dogr Gunjala_Gondi Gong Hanifi_Rohingya Rohg Makasar Maka Medefaidrin Medf Old_Sogdian Sogo Sogdian Sogd";
    ecma11ScriptValues = ecma10ScriptValues + " Elymaic Elym Nandinagari Nand Nyiakeng_Puachue_Hmong Hmnp Wancho Wcho";
    ecma12ScriptValues = ecma11ScriptValues + " Chorasmian Chrs Diak Dives_Akuru Khitan_Small_Script Kits Yezi Yezidi";
    ecma13ScriptValues = ecma12ScriptValues + " Cypro_Minoan Cpmn Old_Uyghur Ougr Tangsa Tnsa Toto Vithkuqi Vith";
    ecma14ScriptValues = ecma13ScriptValues + " " + scriptValuesAddedInUnicode;
    unicodeScriptValues = {
      9: ecma9ScriptValues,
      10: ecma10ScriptValues,
      11: ecma11ScriptValues,
      12: ecma12ScriptValues,
      13: ecma13ScriptValues,
      14: ecma14ScriptValues
    };
    data = {};
    for (i = 0, list = [9, 10, 11, 12, 13, 14]; i < list.length; i += 1) {
      ecmaVersion = list[i];
      buildUnicodeData(ecmaVersion);
    }
    pp$1 = Parser.prototype;
    BranchID = function BranchID2(parent, base) {
      this.parent = parent;
      this.base = base || this;
    };
    BranchID.prototype.separatedFrom = function separatedFrom(alt) {
      for (var self = this; self; self = self.parent) {
        for (var other = alt; other; other = other.parent) {
          if (self.base === other.base && self !== other) {
            return true;
          }
        }
      }
      return false;
    };
    BranchID.prototype.sibling = function sibling() {
      return new BranchID(this.parent, this.base);
    };
    RegExpValidationState = function RegExpValidationState2(parser) {
      this.parser = parser;
      this.validFlags = "gim" + (parser.options.ecmaVersion >= 6 ? "uy" : "") + (parser.options.ecmaVersion >= 9 ? "s" : "") + (parser.options.ecmaVersion >= 13 ? "d" : "") + (parser.options.ecmaVersion >= 15 ? "v" : "");
      this.unicodeProperties = data[parser.options.ecmaVersion >= 14 ? 14 : parser.options.ecmaVersion];
      this.source = "";
      this.flags = "";
      this.start = 0;
      this.switchU = false;
      this.switchV = false;
      this.switchN = false;
      this.pos = 0;
      this.lastIntValue = 0;
      this.lastStringValue = "";
      this.lastAssertionIsQuantifiable = false;
      this.numCapturingParens = 0;
      this.maxBackReference = 0;
      this.groupNames = /* @__PURE__ */ Object.create(null);
      this.backReferenceNames = [];
      this.branchID = null;
    };
    RegExpValidationState.prototype.reset = function reset(start, pattern, flags) {
      var unicodeSets = flags.indexOf("v") !== -1;
      var unicode = flags.indexOf("u") !== -1;
      this.start = start | 0;
      this.source = pattern + "";
      this.flags = flags;
      if (unicodeSets && this.parser.options.ecmaVersion >= 15) {
        this.switchU = true;
        this.switchV = true;
        this.switchN = true;
      } else {
        this.switchU = unicode && this.parser.options.ecmaVersion >= 6;
        this.switchV = false;
        this.switchN = unicode && this.parser.options.ecmaVersion >= 9;
      }
    };
    RegExpValidationState.prototype.raise = function raise(message) {
      this.parser.raiseRecoverable(this.start, "Invalid regular expression: /" + this.source + "/: " + message);
    };
    RegExpValidationState.prototype.at = function at(i2, forceU) {
      if (forceU === void 0) forceU = false;
      var s = this.source;
      var l = s.length;
      if (i2 >= l) {
        return -1;
      }
      var c = s.charCodeAt(i2);
      if (!(forceU || this.switchU) || c <= 55295 || c >= 57344 || i2 + 1 >= l) {
        return c;
      }
      var next = s.charCodeAt(i2 + 1);
      return next >= 56320 && next <= 57343 ? (c << 10) + next - 56613888 : c;
    };
    RegExpValidationState.prototype.nextIndex = function nextIndex(i2, forceU) {
      if (forceU === void 0) forceU = false;
      var s = this.source;
      var l = s.length;
      if (i2 >= l) {
        return l;
      }
      var c = s.charCodeAt(i2), next;
      if (!(forceU || this.switchU) || c <= 55295 || c >= 57344 || i2 + 1 >= l || (next = s.charCodeAt(i2 + 1)) < 56320 || next > 57343) {
        return i2 + 1;
      }
      return i2 + 2;
    };
    RegExpValidationState.prototype.current = function current(forceU) {
      if (forceU === void 0) forceU = false;
      return this.at(this.pos, forceU);
    };
    RegExpValidationState.prototype.lookahead = function lookahead(forceU) {
      if (forceU === void 0) forceU = false;
      return this.at(this.nextIndex(this.pos, forceU), forceU);
    };
    RegExpValidationState.prototype.advance = function advance(forceU) {
      if (forceU === void 0) forceU = false;
      this.pos = this.nextIndex(this.pos, forceU);
    };
    RegExpValidationState.prototype.eat = function eat(ch, forceU) {
      if (forceU === void 0) forceU = false;
      if (this.current(forceU) === ch) {
        this.advance(forceU);
        return true;
      }
      return false;
    };
    RegExpValidationState.prototype.eatChars = function eatChars(chs, forceU) {
      if (forceU === void 0) forceU = false;
      var pos = this.pos;
      for (var i2 = 0, list2 = chs; i2 < list2.length; i2 += 1) {
        var ch = list2[i2];
        var current2 = this.at(pos, forceU);
        if (current2 === -1 || current2 !== ch) {
          return false;
        }
        pos = this.nextIndex(pos, forceU);
      }
      this.pos = pos;
      return true;
    };
    pp$1.validateRegExpFlags = function(state) {
      var validFlags = state.validFlags;
      var flags = state.flags;
      var u = false;
      var v = false;
      for (var i2 = 0; i2 < flags.length; i2++) {
        var flag = flags.charAt(i2);
        if (validFlags.indexOf(flag) === -1) {
          this.raise(state.start, "Invalid regular expression flag");
        }
        if (flags.indexOf(flag, i2 + 1) > -1) {
          this.raise(state.start, "Duplicate regular expression flag");
        }
        if (flag === "u") {
          u = true;
        }
        if (flag === "v") {
          v = true;
        }
      }
      if (this.options.ecmaVersion >= 15 && u && v) {
        this.raise(state.start, "Invalid regular expression flag");
      }
    };
    pp$1.validateRegExpPattern = function(state) {
      this.regexp_pattern(state);
      if (!state.switchN && this.options.ecmaVersion >= 9 && hasProp(state.groupNames)) {
        state.switchN = true;
        this.regexp_pattern(state);
      }
    };
    pp$1.regexp_pattern = function(state) {
      state.pos = 0;
      state.lastIntValue = 0;
      state.lastStringValue = "";
      state.lastAssertionIsQuantifiable = false;
      state.numCapturingParens = 0;
      state.maxBackReference = 0;
      state.groupNames = /* @__PURE__ */ Object.create(null);
      state.backReferenceNames.length = 0;
      state.branchID = null;
      this.regexp_disjunction(state);
      if (state.pos !== state.source.length) {
        if (state.eat(
          41
          /* ) */
        )) {
          state.raise("Unmatched ')'");
        }
        if (state.eat(
          93
          /* ] */
        ) || state.eat(
          125
          /* } */
        )) {
          state.raise("Lone quantifier brackets");
        }
      }
      if (state.maxBackReference > state.numCapturingParens) {
        state.raise("Invalid escape");
      }
      for (var i2 = 0, list2 = state.backReferenceNames; i2 < list2.length; i2 += 1) {
        var name = list2[i2];
        if (!state.groupNames[name]) {
          state.raise("Invalid named capture referenced");
        }
      }
    };
    pp$1.regexp_disjunction = function(state) {
      var trackDisjunction = this.options.ecmaVersion >= 16;
      if (trackDisjunction) {
        state.branchID = new BranchID(state.branchID, null);
      }
      this.regexp_alternative(state);
      while (state.eat(
        124
        /* | */
      )) {
        if (trackDisjunction) {
          state.branchID = state.branchID.sibling();
        }
        this.regexp_alternative(state);
      }
      if (trackDisjunction) {
        state.branchID = state.branchID.parent;
      }
      if (this.regexp_eatQuantifier(state, true)) {
        state.raise("Nothing to repeat");
      }
      if (state.eat(
        123
        /* { */
      )) {
        state.raise("Lone quantifier brackets");
      }
    };
    pp$1.regexp_alternative = function(state) {
      while (state.pos < state.source.length && this.regexp_eatTerm(state)) {
      }
    };
    pp$1.regexp_eatTerm = function(state) {
      if (this.regexp_eatAssertion(state)) {
        if (state.lastAssertionIsQuantifiable && this.regexp_eatQuantifier(state)) {
          if (state.switchU) {
            state.raise("Invalid quantifier");
          }
        }
        return true;
      }
      if (state.switchU ? this.regexp_eatAtom(state) : this.regexp_eatExtendedAtom(state)) {
        this.regexp_eatQuantifier(state);
        return true;
      }
      return false;
    };
    pp$1.regexp_eatAssertion = function(state) {
      var start = state.pos;
      state.lastAssertionIsQuantifiable = false;
      if (state.eat(
        94
        /* ^ */
      ) || state.eat(
        36
        /* $ */
      )) {
        return true;
      }
      if (state.eat(
        92
        /* \ */
      )) {
        if (state.eat(
          66
          /* B */
        ) || state.eat(
          98
          /* b */
        )) {
          return true;
        }
        state.pos = start;
      }
      if (state.eat(
        40
        /* ( */
      ) && state.eat(
        63
        /* ? */
      )) {
        var lookbehind = false;
        if (this.options.ecmaVersion >= 9) {
          lookbehind = state.eat(
            60
            /* < */
          );
        }
        if (state.eat(
          61
          /* = */
        ) || state.eat(
          33
          /* ! */
        )) {
          this.regexp_disjunction(state);
          if (!state.eat(
            41
            /* ) */
          )) {
            state.raise("Unterminated group");
          }
          state.lastAssertionIsQuantifiable = !lookbehind;
          return true;
        }
      }
      state.pos = start;
      return false;
    };
    pp$1.regexp_eatQuantifier = function(state, noError) {
      if (noError === void 0) noError = false;
      if (this.regexp_eatQuantifierPrefix(state, noError)) {
        state.eat(
          63
          /* ? */
        );
        return true;
      }
      return false;
    };
    pp$1.regexp_eatQuantifierPrefix = function(state, noError) {
      return state.eat(
        42
        /* * */
      ) || state.eat(
        43
        /* + */
      ) || state.eat(
        63
        /* ? */
      ) || this.regexp_eatBracedQuantifier(state, noError);
    };
    pp$1.regexp_eatBracedQuantifier = function(state, noError) {
      var start = state.pos;
      if (state.eat(
        123
        /* { */
      )) {
        var min = 0, max = -1;
        if (this.regexp_eatDecimalDigits(state)) {
          min = state.lastIntValue;
          if (state.eat(
            44
            /* , */
          ) && this.regexp_eatDecimalDigits(state)) {
            max = state.lastIntValue;
          }
          if (state.eat(
            125
            /* } */
          )) {
            if (max !== -1 && max < min && !noError) {
              state.raise("numbers out of order in {} quantifier");
            }
            return true;
          }
        }
        if (state.switchU && !noError) {
          state.raise("Incomplete quantifier");
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatAtom = function(state) {
      return this.regexp_eatPatternCharacters(state) || state.eat(
        46
        /* . */
      ) || this.regexp_eatReverseSolidusAtomEscape(state) || this.regexp_eatCharacterClass(state) || this.regexp_eatUncapturingGroup(state) || this.regexp_eatCapturingGroup(state);
    };
    pp$1.regexp_eatReverseSolidusAtomEscape = function(state) {
      var start = state.pos;
      if (state.eat(
        92
        /* \ */
      )) {
        if (this.regexp_eatAtomEscape(state)) {
          return true;
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatUncapturingGroup = function(state) {
      var start = state.pos;
      if (state.eat(
        40
        /* ( */
      )) {
        if (state.eat(
          63
          /* ? */
        )) {
          if (this.options.ecmaVersion >= 16) {
            var addModifiers = this.regexp_eatModifiers(state);
            var hasHyphen = state.eat(
              45
              /* - */
            );
            if (addModifiers || hasHyphen) {
              for (var i2 = 0; i2 < addModifiers.length; i2++) {
                var modifier = addModifiers.charAt(i2);
                if (addModifiers.indexOf(modifier, i2 + 1) > -1) {
                  state.raise("Duplicate regular expression modifiers");
                }
              }
              if (hasHyphen) {
                var removeModifiers = this.regexp_eatModifiers(state);
                if (!addModifiers && !removeModifiers && state.current() === 58) {
                  state.raise("Invalid regular expression modifiers");
                }
                for (var i$1 = 0; i$1 < removeModifiers.length; i$1++) {
                  var modifier$1 = removeModifiers.charAt(i$1);
                  if (removeModifiers.indexOf(modifier$1, i$1 + 1) > -1 || addModifiers.indexOf(modifier$1) > -1) {
                    state.raise("Duplicate regular expression modifiers");
                  }
                }
              }
            }
          }
          if (state.eat(
            58
            /* : */
          )) {
            this.regexp_disjunction(state);
            if (state.eat(
              41
              /* ) */
            )) {
              return true;
            }
            state.raise("Unterminated group");
          }
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatCapturingGroup = function(state) {
      if (state.eat(
        40
        /* ( */
      )) {
        if (this.options.ecmaVersion >= 9) {
          this.regexp_groupSpecifier(state);
        } else if (state.current() === 63) {
          state.raise("Invalid group");
        }
        this.regexp_disjunction(state);
        if (state.eat(
          41
          /* ) */
        )) {
          state.numCapturingParens += 1;
          return true;
        }
        state.raise("Unterminated group");
      }
      return false;
    };
    pp$1.regexp_eatModifiers = function(state) {
      var modifiers = "";
      var ch = 0;
      while ((ch = state.current()) !== -1 && isRegularExpressionModifier(ch)) {
        modifiers += codePointToString(ch);
        state.advance();
      }
      return modifiers;
    };
    pp$1.regexp_eatExtendedAtom = function(state) {
      return state.eat(
        46
        /* . */
      ) || this.regexp_eatReverseSolidusAtomEscape(state) || this.regexp_eatCharacterClass(state) || this.regexp_eatUncapturingGroup(state) || this.regexp_eatCapturingGroup(state) || this.regexp_eatInvalidBracedQuantifier(state) || this.regexp_eatExtendedPatternCharacter(state);
    };
    pp$1.regexp_eatInvalidBracedQuantifier = function(state) {
      if (this.regexp_eatBracedQuantifier(state, true)) {
        state.raise("Nothing to repeat");
      }
      return false;
    };
    pp$1.regexp_eatSyntaxCharacter = function(state) {
      var ch = state.current();
      if (isSyntaxCharacter(ch)) {
        state.lastIntValue = ch;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatPatternCharacters = function(state) {
      var start = state.pos;
      var ch = 0;
      while ((ch = state.current()) !== -1 && !isSyntaxCharacter(ch)) {
        state.advance();
      }
      return state.pos !== start;
    };
    pp$1.regexp_eatExtendedPatternCharacter = function(state) {
      var ch = state.current();
      if (ch !== -1 && ch !== 36 && !(ch >= 40 && ch <= 43) && ch !== 46 && ch !== 63 && ch !== 91 && ch !== 94 && ch !== 124) {
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_groupSpecifier = function(state) {
      if (state.eat(
        63
        /* ? */
      )) {
        if (!this.regexp_eatGroupName(state)) {
          state.raise("Invalid group");
        }
        var trackDisjunction = this.options.ecmaVersion >= 16;
        var known = state.groupNames[state.lastStringValue];
        if (known) {
          if (trackDisjunction) {
            for (var i2 = 0, list2 = known; i2 < list2.length; i2 += 1) {
              var altID = list2[i2];
              if (!altID.separatedFrom(state.branchID)) {
                state.raise("Duplicate capture group name");
              }
            }
          } else {
            state.raise("Duplicate capture group name");
          }
        }
        if (trackDisjunction) {
          (known || (state.groupNames[state.lastStringValue] = [])).push(state.branchID);
        } else {
          state.groupNames[state.lastStringValue] = true;
        }
      }
    };
    pp$1.regexp_eatGroupName = function(state) {
      state.lastStringValue = "";
      if (state.eat(
        60
        /* < */
      )) {
        if (this.regexp_eatRegExpIdentifierName(state) && state.eat(
          62
          /* > */
        )) {
          return true;
        }
        state.raise("Invalid capture group name");
      }
      return false;
    };
    pp$1.regexp_eatRegExpIdentifierName = function(state) {
      state.lastStringValue = "";
      if (this.regexp_eatRegExpIdentifierStart(state)) {
        state.lastStringValue += codePointToString(state.lastIntValue);
        while (this.regexp_eatRegExpIdentifierPart(state)) {
          state.lastStringValue += codePointToString(state.lastIntValue);
        }
        return true;
      }
      return false;
    };
    pp$1.regexp_eatRegExpIdentifierStart = function(state) {
      var start = state.pos;
      var forceU = this.options.ecmaVersion >= 11;
      var ch = state.current(forceU);
      state.advance(forceU);
      if (ch === 92 && this.regexp_eatRegExpUnicodeEscapeSequence(state, forceU)) {
        ch = state.lastIntValue;
      }
      if (isRegExpIdentifierStart(ch)) {
        state.lastIntValue = ch;
        return true;
      }
      state.pos = start;
      return false;
    };
    pp$1.regexp_eatRegExpIdentifierPart = function(state) {
      var start = state.pos;
      var forceU = this.options.ecmaVersion >= 11;
      var ch = state.current(forceU);
      state.advance(forceU);
      if (ch === 92 && this.regexp_eatRegExpUnicodeEscapeSequence(state, forceU)) {
        ch = state.lastIntValue;
      }
      if (isRegExpIdentifierPart(ch)) {
        state.lastIntValue = ch;
        return true;
      }
      state.pos = start;
      return false;
    };
    pp$1.regexp_eatAtomEscape = function(state) {
      if (this.regexp_eatBackReference(state) || this.regexp_eatCharacterClassEscape(state) || this.regexp_eatCharacterEscape(state) || state.switchN && this.regexp_eatKGroupName(state)) {
        return true;
      }
      if (state.switchU) {
        if (state.current() === 99) {
          state.raise("Invalid unicode escape");
        }
        state.raise("Invalid escape");
      }
      return false;
    };
    pp$1.regexp_eatBackReference = function(state) {
      var start = state.pos;
      if (this.regexp_eatDecimalEscape(state)) {
        var n = state.lastIntValue;
        if (state.switchU) {
          if (n > state.maxBackReference) {
            state.maxBackReference = n;
          }
          return true;
        }
        if (n <= state.numCapturingParens) {
          return true;
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatKGroupName = function(state) {
      if (state.eat(
        107
        /* k */
      )) {
        if (this.regexp_eatGroupName(state)) {
          state.backReferenceNames.push(state.lastStringValue);
          return true;
        }
        state.raise("Invalid named reference");
      }
      return false;
    };
    pp$1.regexp_eatCharacterEscape = function(state) {
      return this.regexp_eatControlEscape(state) || this.regexp_eatCControlLetter(state) || this.regexp_eatZero(state) || this.regexp_eatHexEscapeSequence(state) || this.regexp_eatRegExpUnicodeEscapeSequence(state, false) || !state.switchU && this.regexp_eatLegacyOctalEscapeSequence(state) || this.regexp_eatIdentityEscape(state);
    };
    pp$1.regexp_eatCControlLetter = function(state) {
      var start = state.pos;
      if (state.eat(
        99
        /* c */
      )) {
        if (this.regexp_eatControlLetter(state)) {
          return true;
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatZero = function(state) {
      if (state.current() === 48 && !isDecimalDigit(state.lookahead())) {
        state.lastIntValue = 0;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatControlEscape = function(state) {
      var ch = state.current();
      if (ch === 116) {
        state.lastIntValue = 9;
        state.advance();
        return true;
      }
      if (ch === 110) {
        state.lastIntValue = 10;
        state.advance();
        return true;
      }
      if (ch === 118) {
        state.lastIntValue = 11;
        state.advance();
        return true;
      }
      if (ch === 102) {
        state.lastIntValue = 12;
        state.advance();
        return true;
      }
      if (ch === 114) {
        state.lastIntValue = 13;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatControlLetter = function(state) {
      var ch = state.current();
      if (isControlLetter(ch)) {
        state.lastIntValue = ch % 32;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatRegExpUnicodeEscapeSequence = function(state, forceU) {
      if (forceU === void 0) forceU = false;
      var start = state.pos;
      var switchU = forceU || state.switchU;
      if (state.eat(
        117
        /* u */
      )) {
        if (this.regexp_eatFixedHexDigits(state, 4)) {
          var lead = state.lastIntValue;
          if (switchU && lead >= 55296 && lead <= 56319) {
            var leadSurrogateEnd = state.pos;
            if (state.eat(
              92
              /* \ */
            ) && state.eat(
              117
              /* u */
            ) && this.regexp_eatFixedHexDigits(state, 4)) {
              var trail = state.lastIntValue;
              if (trail >= 56320 && trail <= 57343) {
                state.lastIntValue = (lead - 55296) * 1024 + (trail - 56320) + 65536;
                return true;
              }
            }
            state.pos = leadSurrogateEnd;
            state.lastIntValue = lead;
          }
          return true;
        }
        if (switchU && state.eat(
          123
          /* { */
        ) && this.regexp_eatHexDigits(state) && state.eat(
          125
          /* } */
        ) && isValidUnicode(state.lastIntValue)) {
          return true;
        }
        if (switchU) {
          state.raise("Invalid unicode escape");
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatIdentityEscape = function(state) {
      if (state.switchU) {
        if (this.regexp_eatSyntaxCharacter(state)) {
          return true;
        }
        if (state.eat(
          47
          /* / */
        )) {
          state.lastIntValue = 47;
          return true;
        }
        return false;
      }
      var ch = state.current();
      if (ch !== 99 && (!state.switchN || ch !== 107)) {
        state.lastIntValue = ch;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatDecimalEscape = function(state) {
      state.lastIntValue = 0;
      var ch = state.current();
      if (ch >= 49 && ch <= 57) {
        do {
          state.lastIntValue = 10 * state.lastIntValue + (ch - 48);
          state.advance();
        } while ((ch = state.current()) >= 48 && ch <= 57);
        return true;
      }
      return false;
    };
    CharSetNone = 0;
    CharSetOk = 1;
    CharSetString = 2;
    pp$1.regexp_eatCharacterClassEscape = function(state) {
      var ch = state.current();
      if (isCharacterClassEscape(ch)) {
        state.lastIntValue = -1;
        state.advance();
        return CharSetOk;
      }
      var negate = false;
      if (state.switchU && this.options.ecmaVersion >= 9 && ((negate = ch === 80) || ch === 112)) {
        state.lastIntValue = -1;
        state.advance();
        var result;
        if (state.eat(
          123
          /* { */
        ) && (result = this.regexp_eatUnicodePropertyValueExpression(state)) && state.eat(
          125
          /* } */
        )) {
          if (negate && result === CharSetString) {
            state.raise("Invalid property name");
          }
          return result;
        }
        state.raise("Invalid property name");
      }
      return CharSetNone;
    };
    pp$1.regexp_eatUnicodePropertyValueExpression = function(state) {
      var start = state.pos;
      if (this.regexp_eatUnicodePropertyName(state) && state.eat(
        61
        /* = */
      )) {
        var name = state.lastStringValue;
        if (this.regexp_eatUnicodePropertyValue(state)) {
          var value = state.lastStringValue;
          this.regexp_validateUnicodePropertyNameAndValue(state, name, value);
          return CharSetOk;
        }
      }
      state.pos = start;
      if (this.regexp_eatLoneUnicodePropertyNameOrValue(state)) {
        var nameOrValue = state.lastStringValue;
        return this.regexp_validateUnicodePropertyNameOrValue(state, nameOrValue);
      }
      return CharSetNone;
    };
    pp$1.regexp_validateUnicodePropertyNameAndValue = function(state, name, value) {
      if (!hasOwn(state.unicodeProperties.nonBinary, name)) {
        state.raise("Invalid property name");
      }
      if (!state.unicodeProperties.nonBinary[name].test(value)) {
        state.raise("Invalid property value");
      }
    };
    pp$1.regexp_validateUnicodePropertyNameOrValue = function(state, nameOrValue) {
      if (state.unicodeProperties.binary.test(nameOrValue)) {
        return CharSetOk;
      }
      if (state.switchV && state.unicodeProperties.binaryOfStrings.test(nameOrValue)) {
        return CharSetString;
      }
      state.raise("Invalid property name");
    };
    pp$1.regexp_eatUnicodePropertyName = function(state) {
      var ch = 0;
      state.lastStringValue = "";
      while (isUnicodePropertyNameCharacter(ch = state.current())) {
        state.lastStringValue += codePointToString(ch);
        state.advance();
      }
      return state.lastStringValue !== "";
    };
    pp$1.regexp_eatUnicodePropertyValue = function(state) {
      var ch = 0;
      state.lastStringValue = "";
      while (isUnicodePropertyValueCharacter(ch = state.current())) {
        state.lastStringValue += codePointToString(ch);
        state.advance();
      }
      return state.lastStringValue !== "";
    };
    pp$1.regexp_eatLoneUnicodePropertyNameOrValue = function(state) {
      return this.regexp_eatUnicodePropertyValue(state);
    };
    pp$1.regexp_eatCharacterClass = function(state) {
      if (state.eat(
        91
        /* [ */
      )) {
        var negate = state.eat(
          94
          /* ^ */
        );
        var result = this.regexp_classContents(state);
        if (!state.eat(
          93
          /* ] */
        )) {
          state.raise("Unterminated character class");
        }
        if (negate && result === CharSetString) {
          state.raise("Negated character class may contain strings");
        }
        return true;
      }
      return false;
    };
    pp$1.regexp_classContents = function(state) {
      if (state.current() === 93) {
        return CharSetOk;
      }
      if (state.switchV) {
        return this.regexp_classSetExpression(state);
      }
      this.regexp_nonEmptyClassRanges(state);
      return CharSetOk;
    };
    pp$1.regexp_nonEmptyClassRanges = function(state) {
      while (this.regexp_eatClassAtom(state)) {
        var left = state.lastIntValue;
        if (state.eat(
          45
          /* - */
        ) && this.regexp_eatClassAtom(state)) {
          var right = state.lastIntValue;
          if (state.switchU && (left === -1 || right === -1)) {
            state.raise("Invalid character class");
          }
          if (left !== -1 && right !== -1 && left > right) {
            state.raise("Range out of order in character class");
          }
        }
      }
    };
    pp$1.regexp_eatClassAtom = function(state) {
      var start = state.pos;
      if (state.eat(
        92
        /* \ */
      )) {
        if (this.regexp_eatClassEscape(state)) {
          return true;
        }
        if (state.switchU) {
          var ch$1 = state.current();
          if (ch$1 === 99 || isOctalDigit(ch$1)) {
            state.raise("Invalid class escape");
          }
          state.raise("Invalid escape");
        }
        state.pos = start;
      }
      var ch = state.current();
      if (ch !== 93) {
        state.lastIntValue = ch;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatClassEscape = function(state) {
      var start = state.pos;
      if (state.eat(
        98
        /* b */
      )) {
        state.lastIntValue = 8;
        return true;
      }
      if (state.switchU && state.eat(
        45
        /* - */
      )) {
        state.lastIntValue = 45;
        return true;
      }
      if (!state.switchU && state.eat(
        99
        /* c */
      )) {
        if (this.regexp_eatClassControlLetter(state)) {
          return true;
        }
        state.pos = start;
      }
      return this.regexp_eatCharacterClassEscape(state) || this.regexp_eatCharacterEscape(state);
    };
    pp$1.regexp_classSetExpression = function(state) {
      var result = CharSetOk, subResult;
      if (this.regexp_eatClassSetRange(state)) ;
      else if (subResult = this.regexp_eatClassSetOperand(state)) {
        if (subResult === CharSetString) {
          result = CharSetString;
        }
        var start = state.pos;
        while (state.eatChars(
          [38, 38]
          /* && */
        )) {
          if (state.current() !== 38 && (subResult = this.regexp_eatClassSetOperand(state))) {
            if (subResult !== CharSetString) {
              result = CharSetOk;
            }
            continue;
          }
          state.raise("Invalid character in character class");
        }
        if (start !== state.pos) {
          return result;
        }
        while (state.eatChars(
          [45, 45]
          /* -- */
        )) {
          if (this.regexp_eatClassSetOperand(state)) {
            continue;
          }
          state.raise("Invalid character in character class");
        }
        if (start !== state.pos) {
          return result;
        }
      } else {
        state.raise("Invalid character in character class");
      }
      for (; ; ) {
        if (this.regexp_eatClassSetRange(state)) {
          continue;
        }
        subResult = this.regexp_eatClassSetOperand(state);
        if (!subResult) {
          return result;
        }
        if (subResult === CharSetString) {
          result = CharSetString;
        }
      }
    };
    pp$1.regexp_eatClassSetRange = function(state) {
      var start = state.pos;
      if (this.regexp_eatClassSetCharacter(state)) {
        var left = state.lastIntValue;
        if (state.eat(
          45
          /* - */
        ) && this.regexp_eatClassSetCharacter(state)) {
          var right = state.lastIntValue;
          if (left !== -1 && right !== -1 && left > right) {
            state.raise("Range out of order in character class");
          }
          return true;
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatClassSetOperand = function(state) {
      if (this.regexp_eatClassSetCharacter(state)) {
        return CharSetOk;
      }
      return this.regexp_eatClassStringDisjunction(state) || this.regexp_eatNestedClass(state);
    };
    pp$1.regexp_eatNestedClass = function(state) {
      var start = state.pos;
      if (state.eat(
        91
        /* [ */
      )) {
        var negate = state.eat(
          94
          /* ^ */
        );
        var result = this.regexp_classContents(state);
        if (state.eat(
          93
          /* ] */
        )) {
          if (negate && result === CharSetString) {
            state.raise("Negated character class may contain strings");
          }
          return result;
        }
        state.pos = start;
      }
      if (state.eat(
        92
        /* \ */
      )) {
        var result$1 = this.regexp_eatCharacterClassEscape(state);
        if (result$1) {
          return result$1;
        }
        state.pos = start;
      }
      return null;
    };
    pp$1.regexp_eatClassStringDisjunction = function(state) {
      var start = state.pos;
      if (state.eatChars(
        [92, 113]
        /* \q */
      )) {
        if (state.eat(
          123
          /* { */
        )) {
          var result = this.regexp_classStringDisjunctionContents(state);
          if (state.eat(
            125
            /* } */
          )) {
            return result;
          }
        } else {
          state.raise("Invalid escape");
        }
        state.pos = start;
      }
      return null;
    };
    pp$1.regexp_classStringDisjunctionContents = function(state) {
      var result = this.regexp_classString(state);
      while (state.eat(
        124
        /* | */
      )) {
        if (this.regexp_classString(state) === CharSetString) {
          result = CharSetString;
        }
      }
      return result;
    };
    pp$1.regexp_classString = function(state) {
      var count = 0;
      while (this.regexp_eatClassSetCharacter(state)) {
        count++;
      }
      return count === 1 ? CharSetOk : CharSetString;
    };
    pp$1.regexp_eatClassSetCharacter = function(state) {
      var start = state.pos;
      if (state.eat(
        92
        /* \ */
      )) {
        if (this.regexp_eatCharacterEscape(state) || this.regexp_eatClassSetReservedPunctuator(state)) {
          return true;
        }
        if (state.eat(
          98
          /* b */
        )) {
          state.lastIntValue = 8;
          return true;
        }
        state.pos = start;
        return false;
      }
      var ch = state.current();
      if (ch < 0 || ch === state.lookahead() && isClassSetReservedDoublePunctuatorCharacter(ch)) {
        return false;
      }
      if (isClassSetSyntaxCharacter(ch)) {
        return false;
      }
      state.advance();
      state.lastIntValue = ch;
      return true;
    };
    pp$1.regexp_eatClassSetReservedPunctuator = function(state) {
      var ch = state.current();
      if (isClassSetReservedPunctuator(ch)) {
        state.lastIntValue = ch;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatClassControlLetter = function(state) {
      var ch = state.current();
      if (isDecimalDigit(ch) || ch === 95) {
        state.lastIntValue = ch % 32;
        state.advance();
        return true;
      }
      return false;
    };
    pp$1.regexp_eatHexEscapeSequence = function(state) {
      var start = state.pos;
      if (state.eat(
        120
        /* x */
      )) {
        if (this.regexp_eatFixedHexDigits(state, 2)) {
          return true;
        }
        if (state.switchU) {
          state.raise("Invalid escape");
        }
        state.pos = start;
      }
      return false;
    };
    pp$1.regexp_eatDecimalDigits = function(state) {
      var start = state.pos;
      var ch = 0;
      state.lastIntValue = 0;
      while (isDecimalDigit(ch = state.current())) {
        state.lastIntValue = 10 * state.lastIntValue + (ch - 48);
        state.advance();
      }
      return state.pos !== start;
    };
    pp$1.regexp_eatHexDigits = function(state) {
      var start = state.pos;
      var ch = 0;
      state.lastIntValue = 0;
      while (isHexDigit(ch = state.current())) {
        state.lastIntValue = 16 * state.lastIntValue + hexToInt(ch);
        state.advance();
      }
      return state.pos !== start;
    };
    pp$1.regexp_eatLegacyOctalEscapeSequence = function(state) {
      if (this.regexp_eatOctalDigit(state)) {
        var n1 = state.lastIntValue;
        if (this.regexp_eatOctalDigit(state)) {
          var n2 = state.lastIntValue;
          if (n1 <= 3 && this.regexp_eatOctalDigit(state)) {
            state.lastIntValue = n1 * 64 + n2 * 8 + state.lastIntValue;
          } else {
            state.lastIntValue = n1 * 8 + n2;
          }
        } else {
          state.lastIntValue = n1;
        }
        return true;
      }
      return false;
    };
    pp$1.regexp_eatOctalDigit = function(state) {
      var ch = state.current();
      if (isOctalDigit(ch)) {
        state.lastIntValue = ch - 48;
        state.advance();
        return true;
      }
      state.lastIntValue = 0;
      return false;
    };
    pp$1.regexp_eatFixedHexDigits = function(state, length) {
      var start = state.pos;
      state.lastIntValue = 0;
      for (var i2 = 0; i2 < length; ++i2) {
        var ch = state.current();
        if (!isHexDigit(ch)) {
          state.pos = start;
          return false;
        }
        state.lastIntValue = 16 * state.lastIntValue + hexToInt(ch);
        state.advance();
      }
      return true;
    };
    Token = function Token2(p) {
      this.type = p.type;
      this.value = p.value;
      this.start = p.start;
      this.end = p.end;
      if (p.options.locations) {
        this.loc = new SourceLocation(p, p.startLoc, p.endLoc);
      }
      if (p.options.ranges) {
        this.range = [p.start, p.end];
      }
    };
    pp = Parser.prototype;
    pp.next = function(ignoreEscapeSequenceInKeyword) {
      if (!ignoreEscapeSequenceInKeyword && this.type.keyword && this.containsEsc) {
        this.raiseRecoverable(this.start, "Escape sequence in keyword " + this.type.keyword);
      }
      if (this.options.onToken) {
        this.options.onToken(new Token(this));
      }
      this.lastTokEnd = this.end;
      this.lastTokStart = this.start;
      this.lastTokEndLoc = this.endLoc;
      this.lastTokStartLoc = this.startLoc;
      this.nextToken();
    };
    pp.getToken = function() {
      this.next();
      return new Token(this);
    };
    if (typeof Symbol !== "undefined") {
      pp[Symbol.iterator] = function() {
        var this$1$1 = this;
        return {
          next: function() {
            var token = this$1$1.getToken();
            return {
              done: token.type === types$1.eof,
              value: token
            };
          }
        };
      };
    }
    pp.nextToken = function() {
      var curContext = this.curContext();
      if (!curContext || !curContext.preserveSpace) {
        this.skipSpace();
      }
      this.start = this.pos;
      if (this.options.locations) {
        this.startLoc = this.curPosition();
      }
      if (this.pos >= this.input.length) {
        return this.finishToken(types$1.eof);
      }
      if (curContext.override) {
        return curContext.override(this);
      } else {
        this.readToken(this.fullCharCodeAtPos());
      }
    };
    pp.readToken = function(code) {
      if (isIdentifierStart(code, this.options.ecmaVersion >= 6) || code === 92) {
        return this.readWord();
      }
      return this.getTokenFromCode(code);
    };
    pp.fullCharCodeAt = function(pos) {
      var code = this.input.charCodeAt(pos);
      if (code <= 55295 || code >= 56320) {
        return code;
      }
      var next = this.input.charCodeAt(pos + 1);
      return next <= 56319 || next >= 57344 ? code : (code << 10) + next - 56613888;
    };
    pp.fullCharCodeAtPos = function() {
      return this.fullCharCodeAt(this.pos);
    };
    pp.skipBlockComment = function() {
      var startLoc = this.options.onComment && this.curPosition();
      var start = this.pos, end = this.input.indexOf("*/", this.pos += 2);
      if (end === -1) {
        this.raise(this.pos - 2, "Unterminated comment");
      }
      this.pos = end + 2;
      if (this.options.locations) {
        for (var nextBreak = void 0, pos = start; (nextBreak = nextLineBreak(this.input, pos, this.pos)) > -1; ) {
          ++this.curLine;
          pos = this.lineStart = nextBreak;
        }
      }
      if (this.options.onComment) {
        this.options.onComment(
          true,
          this.input.slice(start + 2, end),
          start,
          this.pos,
          startLoc,
          this.curPosition()
        );
      }
    };
    pp.skipLineComment = function(startSkip) {
      var start = this.pos;
      var startLoc = this.options.onComment && this.curPosition();
      var ch = this.input.charCodeAt(this.pos += startSkip);
      while (this.pos < this.input.length && !isNewLine(ch)) {
        ch = this.input.charCodeAt(++this.pos);
      }
      if (this.options.onComment) {
        this.options.onComment(
          false,
          this.input.slice(start + startSkip, this.pos),
          start,
          this.pos,
          startLoc,
          this.curPosition()
        );
      }
    };
    pp.skipSpace = function() {
      loop: while (this.pos < this.input.length) {
        var ch = this.input.charCodeAt(this.pos);
        switch (ch) {
          case 32:
          case 160:
            ++this.pos;
            break;
          case 13:
            if (this.input.charCodeAt(this.pos + 1) === 10) {
              ++this.pos;
            }
          case 10:
          case 8232:
          case 8233:
            ++this.pos;
            if (this.options.locations) {
              ++this.curLine;
              this.lineStart = this.pos;
            }
            break;
          case 47:
            switch (this.input.charCodeAt(this.pos + 1)) {
              case 42:
                this.skipBlockComment();
                break;
              case 47:
                this.skipLineComment(2);
                break;
              default:
                break loop;
            }
            break;
          default:
            if (ch > 8 && ch < 14 || ch >= 5760 && nonASCIIwhitespace.test(String.fromCharCode(ch))) {
              ++this.pos;
            } else {
              break loop;
            }
        }
      }
    };
    pp.finishToken = function(type2, val) {
      this.end = this.pos;
      if (this.options.locations) {
        this.endLoc = this.curPosition();
      }
      var prevType = this.type;
      this.type = type2;
      this.value = val;
      this.updateContext(prevType);
    };
    pp.readToken_dot = function() {
      var next = this.input.charCodeAt(this.pos + 1);
      if (next >= 48 && next <= 57) {
        return this.readNumber(true);
      }
      var next2 = this.input.charCodeAt(this.pos + 2);
      if (this.options.ecmaVersion >= 6 && next === 46 && next2 === 46) {
        this.pos += 3;
        return this.finishToken(types$1.ellipsis);
      } else {
        ++this.pos;
        return this.finishToken(types$1.dot);
      }
    };
    pp.readToken_slash = function() {
      var next = this.input.charCodeAt(this.pos + 1);
      if (this.exprAllowed) {
        ++this.pos;
        return this.readRegexp();
      }
      if (next === 61) {
        return this.finishOp(types$1.assign, 2);
      }
      return this.finishOp(types$1.slash, 1);
    };
    pp.readToken_mult_modulo_exp = function(code) {
      var next = this.input.charCodeAt(this.pos + 1);
      var size = 1;
      var tokentype = code === 42 ? types$1.star : types$1.modulo;
      if (this.options.ecmaVersion >= 7 && code === 42 && next === 42) {
        ++size;
        tokentype = types$1.starstar;
        next = this.input.charCodeAt(this.pos + 2);
      }
      if (next === 61) {
        return this.finishOp(types$1.assign, size + 1);
      }
      return this.finishOp(tokentype, size);
    };
    pp.readToken_pipe_amp = function(code) {
      var next = this.input.charCodeAt(this.pos + 1);
      if (next === code) {
        if (this.options.ecmaVersion >= 12) {
          var next2 = this.input.charCodeAt(this.pos + 2);
          if (next2 === 61) {
            return this.finishOp(types$1.assign, 3);
          }
        }
        return this.finishOp(code === 124 ? types$1.logicalOR : types$1.logicalAND, 2);
      }
      if (next === 61) {
        return this.finishOp(types$1.assign, 2);
      }
      return this.finishOp(code === 124 ? types$1.bitwiseOR : types$1.bitwiseAND, 1);
    };
    pp.readToken_caret = function() {
      var next = this.input.charCodeAt(this.pos + 1);
      if (next === 61) {
        return this.finishOp(types$1.assign, 2);
      }
      return this.finishOp(types$1.bitwiseXOR, 1);
    };
    pp.readToken_plus_min = function(code) {
      var next = this.input.charCodeAt(this.pos + 1);
      if (next === code) {
        if (next === 45 && !this.inModule && this.input.charCodeAt(this.pos + 2) === 62 && (this.lastTokEnd === 0 || lineBreak.test(this.input.slice(this.lastTokEnd, this.pos)))) {
          this.skipLineComment(3);
          this.skipSpace();
          return this.nextToken();
        }
        return this.finishOp(types$1.incDec, 2);
      }
      if (next === 61) {
        return this.finishOp(types$1.assign, 2);
      }
      return this.finishOp(types$1.plusMin, 1);
    };
    pp.readToken_lt_gt = function(code) {
      var next = this.input.charCodeAt(this.pos + 1);
      var size = 1;
      if (next === code) {
        size = code === 62 && this.input.charCodeAt(this.pos + 2) === 62 ? 3 : 2;
        if (this.input.charCodeAt(this.pos + size) === 61) {
          return this.finishOp(types$1.assign, size + 1);
        }
        return this.finishOp(types$1.bitShift, size);
      }
      if (next === 33 && code === 60 && !this.inModule && this.input.charCodeAt(this.pos + 2) === 45 && this.input.charCodeAt(this.pos + 3) === 45) {
        this.skipLineComment(4);
        this.skipSpace();
        return this.nextToken();
      }
      if (next === 61) {
        size = 2;
      }
      return this.finishOp(types$1.relational, size);
    };
    pp.readToken_eq_excl = function(code) {
      var next = this.input.charCodeAt(this.pos + 1);
      if (next === 61) {
        return this.finishOp(types$1.equality, this.input.charCodeAt(this.pos + 2) === 61 ? 3 : 2);
      }
      if (code === 61 && next === 62 && this.options.ecmaVersion >= 6) {
        this.pos += 2;
        return this.finishToken(types$1.arrow);
      }
      return this.finishOp(code === 61 ? types$1.eq : types$1.prefix, 1);
    };
    pp.readToken_question = function() {
      var ecmaVersion2 = this.options.ecmaVersion;
      if (ecmaVersion2 >= 11) {
        var next = this.input.charCodeAt(this.pos + 1);
        if (next === 46) {
          var next2 = this.input.charCodeAt(this.pos + 2);
          if (next2 < 48 || next2 > 57) {
            return this.finishOp(types$1.questionDot, 2);
          }
        }
        if (next === 63) {
          if (ecmaVersion2 >= 12) {
            var next2$1 = this.input.charCodeAt(this.pos + 2);
            if (next2$1 === 61) {
              return this.finishOp(types$1.assign, 3);
            }
          }
          return this.finishOp(types$1.coalesce, 2);
        }
      }
      return this.finishOp(types$1.question, 1);
    };
    pp.readToken_numberSign = function() {
      var ecmaVersion2 = this.options.ecmaVersion;
      var code = 35;
      if (ecmaVersion2 >= 13) {
        ++this.pos;
        code = this.fullCharCodeAtPos();
        if (isIdentifierStart(code, true) || code === 92) {
          return this.finishToken(types$1.privateId, this.readWord1());
        }
      }
      this.raise(this.pos, "Unexpected character '" + codePointToString(code) + "'");
    };
    pp.getTokenFromCode = function(code) {
      switch (code) {
        // The interpretation of a dot depends on whether it is followed
        // by a digit or another two dots.
        case 46:
          return this.readToken_dot();
        // Punctuation tokens.
        case 40:
          ++this.pos;
          return this.finishToken(types$1.parenL);
        case 41:
          ++this.pos;
          return this.finishToken(types$1.parenR);
        case 59:
          ++this.pos;
          return this.finishToken(types$1.semi);
        case 44:
          ++this.pos;
          return this.finishToken(types$1.comma);
        case 91:
          ++this.pos;
          return this.finishToken(types$1.bracketL);
        case 93:
          ++this.pos;
          return this.finishToken(types$1.bracketR);
        case 123:
          ++this.pos;
          return this.finishToken(types$1.braceL);
        case 125:
          ++this.pos;
          return this.finishToken(types$1.braceR);
        case 58:
          ++this.pos;
          return this.finishToken(types$1.colon);
        case 96:
          if (this.options.ecmaVersion < 6) {
            break;
          }
          ++this.pos;
          return this.finishToken(types$1.backQuote);
        case 48:
          var next = this.input.charCodeAt(this.pos + 1);
          if (next === 120 || next === 88) {
            return this.readRadixNumber(16);
          }
          if (this.options.ecmaVersion >= 6) {
            if (next === 111 || next === 79) {
              return this.readRadixNumber(8);
            }
            if (next === 98 || next === 66) {
              return this.readRadixNumber(2);
            }
          }
        // Anything else beginning with a digit is an integer, octal
        // number, or float.
        case 49:
        case 50:
        case 51:
        case 52:
        case 53:
        case 54:
        case 55:
        case 56:
        case 57:
          return this.readNumber(false);
        // Quotes produce strings.
        case 34:
        case 39:
          return this.readString(code);
        // Operators are parsed inline in tiny state machines. '=' (61) is
        // often referred to. `finishOp` simply skips the amount of
        // characters it is given as second argument, and returns a token
        // of the type given by its first argument.
        case 47:
          return this.readToken_slash();
        case 37:
        case 42:
          return this.readToken_mult_modulo_exp(code);
        case 124:
        case 38:
          return this.readToken_pipe_amp(code);
        case 94:
          return this.readToken_caret();
        case 43:
        case 45:
          return this.readToken_plus_min(code);
        case 60:
        case 62:
          return this.readToken_lt_gt(code);
        case 61:
        case 33:
          return this.readToken_eq_excl(code);
        case 63:
          return this.readToken_question();
        case 126:
          return this.finishOp(types$1.prefix, 1);
        case 35:
          return this.readToken_numberSign();
      }
      this.raise(this.pos, "Unexpected character '" + codePointToString(code) + "'");
    };
    pp.finishOp = function(type2, size) {
      var str2 = this.input.slice(this.pos, this.pos + size);
      this.pos += size;
      return this.finishToken(type2, str2);
    };
    pp.readRegexp = function() {
      var escaped, inClass, start = this.pos;
      for (; ; ) {
        if (this.pos >= this.input.length) {
          this.raise(start, "Unterminated regular expression");
        }
        var ch = this.input.charAt(this.pos);
        if (lineBreak.test(ch)) {
          this.raise(start, "Unterminated regular expression");
        }
        if (!escaped) {
          if (ch === "[") {
            inClass = true;
          } else if (ch === "]" && inClass) {
            inClass = false;
          } else if (ch === "/" && !inClass) {
            break;
          }
          escaped = ch === "\\";
        } else {
          escaped = false;
        }
        ++this.pos;
      }
      var pattern = this.input.slice(start, this.pos);
      ++this.pos;
      var flagsStart = this.pos;
      var flags = this.readWord1();
      if (this.containsEsc) {
        this.unexpected(flagsStart);
      }
      var state = this.regexpState || (this.regexpState = new RegExpValidationState(this));
      state.reset(start, pattern, flags);
      this.validateRegExpFlags(state);
      this.validateRegExpPattern(state);
      var value = null;
      try {
        value = new RegExp(pattern, flags);
      } catch (e) {
      }
      return this.finishToken(types$1.regexp, { pattern, flags, value });
    };
    pp.readInt = function(radix, len, maybeLegacyOctalNumericLiteral) {
      var allowSeparators = this.options.ecmaVersion >= 12 && len === void 0;
      var isLegacyOctalNumericLiteral = maybeLegacyOctalNumericLiteral && this.input.charCodeAt(this.pos) === 48;
      var start = this.pos, total = 0, lastCode = 0;
      for (var i2 = 0, e = len == null ? Infinity : len; i2 < e; ++i2, ++this.pos) {
        var code = this.input.charCodeAt(this.pos), val = void 0;
        if (allowSeparators && code === 95) {
          if (isLegacyOctalNumericLiteral) {
            this.raiseRecoverable(this.pos, "Numeric separator is not allowed in legacy octal numeric literals");
          }
          if (lastCode === 95) {
            this.raiseRecoverable(this.pos, "Numeric separator must be exactly one underscore");
          }
          if (i2 === 0) {
            this.raiseRecoverable(this.pos, "Numeric separator is not allowed at the first of digits");
          }
          lastCode = code;
          continue;
        }
        if (code >= 97) {
          val = code - 97 + 10;
        } else if (code >= 65) {
          val = code - 65 + 10;
        } else if (code >= 48 && code <= 57) {
          val = code - 48;
        } else {
          val = Infinity;
        }
        if (val >= radix) {
          break;
        }
        lastCode = code;
        total = total * radix + val;
      }
      if (allowSeparators && lastCode === 95) {
        this.raiseRecoverable(this.pos - 1, "Numeric separator is not allowed at the last of digits");
      }
      if (this.pos === start || len != null && this.pos - start !== len) {
        return null;
      }
      return total;
    };
    pp.readRadixNumber = function(radix) {
      var start = this.pos;
      this.pos += 2;
      var val = this.readInt(radix);
      if (val == null) {
        this.raise(this.start + 2, "Expected number in radix " + radix);
      }
      if (this.options.ecmaVersion >= 11 && this.input.charCodeAt(this.pos) === 110) {
        val = stringToBigInt(this.input.slice(start, this.pos));
        ++this.pos;
      } else if (isIdentifierStart(this.fullCharCodeAtPos())) {
        this.raise(this.pos, "Identifier directly after number");
      }
      return this.finishToken(types$1.num, val);
    };
    pp.readNumber = function(startsWithDot) {
      var start = this.pos;
      if (!startsWithDot && this.readInt(10, void 0, true) === null) {
        this.raise(start, "Invalid number");
      }
      var octal = this.pos - start >= 2 && this.input.charCodeAt(start) === 48;
      if (octal && this.strict) {
        this.raise(start, "Invalid number");
      }
      var next = this.input.charCodeAt(this.pos);
      if (!octal && !startsWithDot && this.options.ecmaVersion >= 11 && next === 110) {
        var val$1 = stringToBigInt(this.input.slice(start, this.pos));
        ++this.pos;
        if (isIdentifierStart(this.fullCharCodeAtPos())) {
          this.raise(this.pos, "Identifier directly after number");
        }
        return this.finishToken(types$1.num, val$1);
      }
      if (octal && /[89]/.test(this.input.slice(start, this.pos))) {
        octal = false;
      }
      if (next === 46 && !octal) {
        ++this.pos;
        this.readInt(10);
        next = this.input.charCodeAt(this.pos);
      }
      if ((next === 69 || next === 101) && !octal) {
        next = this.input.charCodeAt(++this.pos);
        if (next === 43 || next === 45) {
          ++this.pos;
        }
        if (this.readInt(10) === null) {
          this.raise(start, "Invalid number");
        }
      }
      if (isIdentifierStart(this.fullCharCodeAtPos())) {
        this.raise(this.pos, "Identifier directly after number");
      }
      var val = stringToNumber(this.input.slice(start, this.pos), octal);
      return this.finishToken(types$1.num, val);
    };
    pp.readCodePoint = function() {
      var ch = this.input.charCodeAt(this.pos), code;
      if (ch === 123) {
        if (this.options.ecmaVersion < 6) {
          this.unexpected();
        }
        var codePos = ++this.pos;
        code = this.readHexChar(this.input.indexOf("}", this.pos) - this.pos);
        ++this.pos;
        if (code > 1114111) {
          this.invalidStringToken(codePos, "Code point out of bounds");
        }
      } else {
        code = this.readHexChar(4);
      }
      return code;
    };
    pp.readString = function(quote) {
      var out = "", chunkStart = ++this.pos;
      for (; ; ) {
        if (this.pos >= this.input.length) {
          this.raise(this.start, "Unterminated string constant");
        }
        var ch = this.input.charCodeAt(this.pos);
        if (ch === quote) {
          break;
        }
        if (ch === 92) {
          out += this.input.slice(chunkStart, this.pos);
          out += this.readEscapedChar(false);
          chunkStart = this.pos;
        } else if (ch === 8232 || ch === 8233) {
          if (this.options.ecmaVersion < 10) {
            this.raise(this.start, "Unterminated string constant");
          }
          ++this.pos;
          if (this.options.locations) {
            this.curLine++;
            this.lineStart = this.pos;
          }
        } else {
          if (isNewLine(ch)) {
            this.raise(this.start, "Unterminated string constant");
          }
          ++this.pos;
        }
      }
      out += this.input.slice(chunkStart, this.pos++);
      return this.finishToken(types$1.string, out);
    };
    INVALID_TEMPLATE_ESCAPE_ERROR = {};
    pp.tryReadTemplateToken = function() {
      this.inTemplateElement = true;
      try {
        this.readTmplToken();
      } catch (err) {
        if (err === INVALID_TEMPLATE_ESCAPE_ERROR) {
          this.readInvalidTemplateToken();
        } else {
          throw err;
        }
      }
      this.inTemplateElement = false;
    };
    pp.invalidStringToken = function(position, message) {
      if (this.inTemplateElement && this.options.ecmaVersion >= 9) {
        throw INVALID_TEMPLATE_ESCAPE_ERROR;
      } else {
        this.raise(position, message);
      }
    };
    pp.readTmplToken = function() {
      var out = "", chunkStart = this.pos;
      for (; ; ) {
        if (this.pos >= this.input.length) {
          this.raise(this.start, "Unterminated template");
        }
        var ch = this.input.charCodeAt(this.pos);
        if (ch === 96 || ch === 36 && this.input.charCodeAt(this.pos + 1) === 123) {
          if (this.pos === this.start && (this.type === types$1.template || this.type === types$1.invalidTemplate)) {
            if (ch === 36) {
              this.pos += 2;
              return this.finishToken(types$1.dollarBraceL);
            } else {
              ++this.pos;
              return this.finishToken(types$1.backQuote);
            }
          }
          out += this.input.slice(chunkStart, this.pos);
          return this.finishToken(types$1.template, out);
        }
        if (ch === 92) {
          out += this.input.slice(chunkStart, this.pos);
          out += this.readEscapedChar(true);
          chunkStart = this.pos;
        } else if (isNewLine(ch)) {
          out += this.input.slice(chunkStart, this.pos);
          ++this.pos;
          switch (ch) {
            case 13:
              if (this.input.charCodeAt(this.pos) === 10) {
                ++this.pos;
              }
            case 10:
              out += "\n";
              break;
            default:
              out += String.fromCharCode(ch);
              break;
          }
          if (this.options.locations) {
            ++this.curLine;
            this.lineStart = this.pos;
          }
          chunkStart = this.pos;
        } else {
          ++this.pos;
        }
      }
    };
    pp.readInvalidTemplateToken = function() {
      for (; this.pos < this.input.length; this.pos++) {
        switch (this.input[this.pos]) {
          case "\\":
            ++this.pos;
            break;
          case "$":
            if (this.input[this.pos + 1] !== "{") {
              break;
            }
          // fall through
          case "`":
            return this.finishToken(types$1.invalidTemplate, this.input.slice(this.start, this.pos));
          case "\r":
            if (this.input[this.pos + 1] === "\n") {
              ++this.pos;
            }
          // fall through
          case "\n":
          case "\u2028":
          case "\u2029":
            ++this.curLine;
            this.lineStart = this.pos + 1;
            break;
        }
      }
      this.raise(this.start, "Unterminated template");
    };
    pp.readEscapedChar = function(inTemplate) {
      var ch = this.input.charCodeAt(++this.pos);
      ++this.pos;
      switch (ch) {
        case 110:
          return "\n";
        // 'n' -> '\n'
        case 114:
          return "\r";
        // 'r' -> '\r'
        case 120:
          return String.fromCharCode(this.readHexChar(2));
        // 'x'
        case 117:
          return codePointToString(this.readCodePoint());
        // 'u'
        case 116:
          return "	";
        // 't' -> '\t'
        case 98:
          return "\b";
        // 'b' -> '\b'
        case 118:
          return "\v";
        // 'v' -> '\u000b'
        case 102:
          return "\f";
        // 'f' -> '\f'
        case 13:
          if (this.input.charCodeAt(this.pos) === 10) {
            ++this.pos;
          }
        // '\r\n'
        case 10:
          if (this.options.locations) {
            this.lineStart = this.pos;
            ++this.curLine;
          }
          return "";
        case 56:
        case 57:
          if (this.strict) {
            this.invalidStringToken(
              this.pos - 1,
              "Invalid escape sequence"
            );
          }
          if (inTemplate) {
            var codePos = this.pos - 1;
            this.invalidStringToken(
              codePos,
              "Invalid escape sequence in template string"
            );
          }
        default:
          if (ch >= 48 && ch <= 55) {
            var octalStr = this.input.substr(this.pos - 1, 3).match(/^[0-7]+/)[0];
            var octal = parseInt(octalStr, 8);
            if (octal > 255) {
              octalStr = octalStr.slice(0, -1);
              octal = parseInt(octalStr, 8);
            }
            this.pos += octalStr.length - 1;
            ch = this.input.charCodeAt(this.pos);
            if ((octalStr !== "0" || ch === 56 || ch === 57) && (this.strict || inTemplate)) {
              this.invalidStringToken(
                this.pos - 1 - octalStr.length,
                inTemplate ? "Octal literal in template string" : "Octal literal in strict mode"
              );
            }
            return String.fromCharCode(octal);
          }
          if (isNewLine(ch)) {
            if (this.options.locations) {
              this.lineStart = this.pos;
              ++this.curLine;
            }
            return "";
          }
          return String.fromCharCode(ch);
      }
    };
    pp.readHexChar = function(len) {
      var codePos = this.pos;
      var n = this.readInt(16, len);
      if (n === null) {
        this.invalidStringToken(codePos, "Bad character escape sequence");
      }
      return n;
    };
    pp.readWord1 = function() {
      this.containsEsc = false;
      var word = "", first = true, chunkStart = this.pos;
      var astral = this.options.ecmaVersion >= 6;
      while (this.pos < this.input.length) {
        var ch = this.fullCharCodeAtPos();
        if (isIdentifierChar(ch, astral)) {
          this.pos += ch <= 65535 ? 1 : 2;
        } else if (ch === 92) {
          this.containsEsc = true;
          word += this.input.slice(chunkStart, this.pos);
          var escStart = this.pos;
          if (this.input.charCodeAt(++this.pos) !== 117) {
            this.invalidStringToken(this.pos, "Expecting Unicode escape sequence \\uXXXX");
          }
          ++this.pos;
          var esc = this.readCodePoint();
          if (!(first ? isIdentifierStart : isIdentifierChar)(esc, astral)) {
            this.invalidStringToken(escStart, "Invalid Unicode escape");
          }
          word += codePointToString(esc);
          chunkStart = this.pos;
        } else {
          break;
        }
        first = false;
      }
      return word + this.input.slice(chunkStart, this.pos);
    };
    pp.readWord = function() {
      var word = this.readWord1();
      var type2 = types$1.name;
      if (this.keywords.test(word)) {
        type2 = keywords[word];
      }
      return this.finishToken(type2, word);
    };
    version = "8.18.0";
    Parser.acorn = {
      Parser,
      version,
      defaultOptions,
      Position,
      SourceLocation,
      getLineInfo,
      Node,
      TokenType,
      tokTypes: types$1,
      keywordTypes: keywords,
      TokContext,
      tokContexts: types2,
      isIdentifierChar,
      isIdentifierStart,
      Token,
      isNewLine,
      lineBreak,
      lineBreakG,
      nonASCIIwhitespace
    };
  }
});

// src/dsh/bun-closure.ts
var init_bun_closure = __esm({
  "src/dsh/bun-closure.ts"() {
    "use strict";
    init_acorn();
  }
});

// src/dsh/compatibility.json
var compatibility_default;
var init_compatibility = __esm({
  "src/dsh/compatibility.json"() {
    compatibility_default = {
      version: 2,
      target_dsh_version: "0.2.1-rc.2",
      provided_services: [
        "tools",
        "commands",
        "prompt",
        "storage",
        "skills",
        "avatars",
        "mcp",
        "shellHooks",
        "logger",
        "events",
        "reflect",
        "registry",
        "sessions",
        "userQuestions",
        "subagents",
        "agents",
        "systemPrompt"
      ],
      refused_services: [
        "shellHooks",
        "loader",
        "approval",
        "agents",
        "subagents",
        "sessions",
        "llm",
        "sandboxPolicy",
        "credentials",
        "fs",
        "subprocess",
        "systemPrompt",
        "runtimeLoop",
        "tools",
        "commands",
        "prompt",
        "storage",
        "skills",
        "avatars",
        "mcp",
        "logger",
        "userQuestions"
      ],
      row_bridges: {
        "@deepseek-ai/dsh-mcp-client": "mcp",
        "@deepseek-ai/dsh-skill-filesystem": "skills",
        "@deepseek-ai/dsh-hooks-claude-code": "hooks-claude-code",
        "@deepseek-ai/dsh-hooks-codex": "hooks-codex",
        "@deepseek-ai/dsh-persona": "persona"
      },
      module_singletons: {
        "@codewhale/dsh-composition": "dsh-composition",
        "@deepseek-ai/cordis-plugin-loader": "dsh-loader",
        "@deepseek-ai/cordis-plugin-include": "dsh-include",
        "@deepseek-ai/cordis-plugin-group": "dsh-group",
        "@deepseek-ai/cordis": "cordis",
        "@deepseek-ai/schemastery": "schemastery",
        "@deepseek-ai/cosmokit": "cosmokit",
        cosmokit: "cosmokit",
        "@deepseek-ai/dsh-tools": "dsh-tools",
        "@deepseek-ai/dsh-util-values": "dsh-util-values",
        "@deepseek-ai/dsh-commands/brand": "dsh-commands-brand",
        "@deepseek-ai/dsh-llm": "dsh-llm-values",
        "@deepseek-ai/dsh-session": "dsh-session-values",
        "@deepseek-ai/dsh-system-prompt": "dsh-system-prompt-values",
        "@deepseek-ai/dsh-user-questions": "dsh-user-questions-values"
      },
      definition_api: {
        package: "@deepseek-ai/dsh-tools",
        version: "0.2.1-rc.2",
        license: "MIT",
        tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-tools/-/dsh-tools-0.2.1-rc.2.tgz",
        integrity: "sha512-K3gET3VPbtxM66pkeRsEGrZ+bQKiBqrTbNXiIreBKu0hP+qPvlSwhiSI5MTnFp2tMFK5rpuYMZosRIdbfrbJ/g==",
        source_file: "lib/index.js",
        source_sha256: "1e53932e7c4213ea24a7d87f06960b7a7702cb21f8a5741c97242b4a470a442a",
        source_lines: [
          [
            8,
            888
          ],
          [
            2625,
            2655
          ]
        ],
        license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be"
      },
      command_followup: "one-text-message-submit-on-success",
      pure_api: {
        file: "src/dsh/dsh-message-compat.js",
        source_sha256: "aac208e72eac3601b02c5a4ce95bfeb82f1bb3ba0d1e906d786d1e25609e9857",
        exports: {
          "@deepseek-ai/dsh-llm": [
            "createUserMessage",
            "HarnessError"
          ],
          "@deepseek-ai/dsh-session": [
            "SessionId"
          ],
          "@deepseek-ai/dsh-system-prompt": [
            "PERSONA_PREFIX_SECTION"
          ],
          "@deepseek-ai/dsh-user-questions": [
            "UserQuestionError"
          ]
        },
        packages: [
          {
            package: "@deepseek-ai/dsh-brand",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-brand/-/dsh-brand-0.2.1-rc.2.tgz",
            integrity: "sha512-T2kcygikRS8rv8cKjgho2x381EgGprv5yQmvHl4/iYbuHE19OW+SLGtHWbV27PGdSHz5U8CJxasYzFosKW4e2A==",
            tarball_sha256: "abc43c2daa0c476881cd3441d57e662e386f6e07bf60fbde8c9e41e126f70716",
            source_file: "lib/index.js",
            source_sha256: "14feb59256b891982b2017d73ed03ee65e2a7365f48e4902a856acb0e52d5eb0",
            excerpts: [
              {
                name: "brandString",
                lines: [
                  20,
                  22
                ],
                sha256: "2f0b62ba6507db54836a99e24d1d219e598b6140e575d2cf5c63d86a18e5e8ac"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          },
          {
            package: "@deepseek-ai/dsh-util-crypto",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-util-crypto/-/dsh-util-crypto-0.2.1-rc.2.tgz",
            integrity: "sha512-V4SxvXL4ZbY5RYJckVtmo3qz4YsA52pYKvDF13+8hsDCEmQIv9YfmLRMA61pjK/i05pGMe3p1pPhAB6L+NOIEA==",
            tarball_sha256: "333710abf757d423118d9d48f8376d5731eea802bbefeb9f1182db6d58360195",
            source_file: "lib/index.js",
            source_sha256: "640b26c8e5a1156c6c8e8749a22f805c9c0e34444778eb4ee9529be3e88ec862",
            excerpts: [
              {
                name: "randomUUID",
                lines: [
                  27,
                  33
                ],
                sha256: "e6c3b47d5cf366a149204c36d1697e658f61c48bfd96e66ec1c96deec3a8f4e2"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          },
          {
            package: "@deepseek-ai/dsh-llm",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-llm/-/dsh-llm-0.2.1-rc.2.tgz",
            integrity: "sha512-3xfoMDHEpUJGcH2Lu+lNXUJYRBclYfBkdCG9Au02VzIjYAm9BrFmc/MNuaVTg0vwgvPpTsd6Fx1IeGtfmQFvlg==",
            tarball_sha256: "00e45b51cf850817c7a27c00203a1715853f9e476ff44256e895f8aafda2b8ba",
            source_file: "lib/index.js",
            source_sha256: "d18d8ae4c8d961e628708c1e4698e09716a0ebb80e55a672e0bde1029bec1155",
            excerpts: [
              {
                name: "createMessage",
                lines: [
                  37,
                  42
                ],
                sha256: "74baa44c9dea1821d97bd4fe592b21baf31f60eaf06962a269fcf2ec88e352ae"
              },
              {
                name: "createUserMessage",
                lines: [
                  59,
                  64
                ],
                sha256: "2c4929c2c18e511aa0fefe216eb50e703191057bb15ba1412ddcfdcc8bfc020f"
              },
              {
                name: "HarnessError declaration",
                lines: [
                  126,
                  126
                ],
                sha256: "d260a23e0a20850d3b623cbeed54626b47d43fa74e000c5d529dc0a341ab63ee"
              },
              {
                name: "HarnessError body",
                lines: [
                  128,
                  134
                ],
                sha256: "30ee24ef29e412b1ab2f0ca89ab600bb0260022d1479d43ca2ad21173b774ccf"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          },
          {
            package: "@deepseek-ai/dsh-session",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-session/-/dsh-session-0.2.1-rc.2.tgz",
            integrity: "sha512-WDA4lIrXtG5nf1dn2wrA4UzEr7D6e8T3AdPpVSZ7eY4N4ICMDZ0RYuD4GkM+iSb7WkLojdO8ljr0etiIMEbqJg==",
            tarball_sha256: "72b538e5e120e1f79c923ad527742640fe56cba50db0a7bacdc12ad73e0225e7",
            source_file: "lib/index.js",
            source_sha256: "2f5dd07cb5c744a29abbb8003a8a4cefd9e582c6e2538074ca299a0ddd02a3bf",
            excerpts: [
              {
                name: "SessionId",
                lines: [
                  13,
                  15
                ],
                sha256: "68a5778f0586d4e1c219ce6ae3cec3120a57446bbb91f371b4ba208533f48996"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          },
          {
            package: "@deepseek-ai/dsh-system-prompt",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-system-prompt/-/dsh-system-prompt-0.2.1-rc.2.tgz",
            integrity: "sha512-X1WG2FMyfZapAkcCYyTl0UO5vVqDcCsA9wECJY7eeDTLHJDsqrilQJg2DRgHQzM+2/7T01QyzFZhKd7hfALJxg==",
            tarball_sha256: "d186153582b801c3f16a84d1a0651910957be7498929381a48ae6fe49666206d",
            source_file: "lib/index.js",
            source_sha256: "7be9f7aafbf345d01ddcc12fd297b16e9a1d41283a6a04073b867becca0bf020",
            excerpts: [
              {
                name: "PERSONA_PREFIX_SECTION",
                lines: [
                  59,
                  59
                ],
                sha256: "11338254c278626eea6887b9cb5dc94d16f2ab4dae4bd7e555ea608c4b15648f"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          },
          {
            package: "@deepseek-ai/dsh-util-values",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-util-values/-/dsh-util-values-0.2.1-rc.2.tgz",
            integrity: "sha512-mgnByPadOgZtoBxbwMI184li494O6sNn66/NvdqN0RIrzAJmFGpFxpjFkQtQN/yetwWhlx7Ic17+6p7eKl8DNg==",
            tarball_sha256: "2b273a4eae9cd3a40a57f82a28489b763cd66bddc5c91953e5473e2e45e740cb",
            source_file: "lib/index.js",
            source_sha256: "4322a2aacdfe38476d2bbc62338120beea1e1660346d7db5b6c1e1f927a26b7c",
            excerpts: [
              {
                name: "deepFreeze",
                lines: [
                  824,
                  860
                ],
                sha256: "66ee7f28877afa912496ba944a581ac74e7b87390e6a47c4c640d8f477bdc6e2"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: false
          },
          {
            package: "@deepseek-ai/dsh-user-questions",
            version: "0.2.1-rc.2",
            license: "MIT",
            tarball: "https://registry.npmjs.org/@deepseek-ai/dsh-user-questions/-/dsh-user-questions-0.2.1-rc.2.tgz",
            integrity: "sha512-XVsvMCjxHuDX4YxhwWqRi1IExp7evWVyQ1Gis+90ZBmRNRH/MujjboE+QVc79sdg+p79tzJmqPUDxPj1JOyjzQ==",
            tarball_sha256: "f5194abbe1b3412ad56487b5e6f1aae455cce8bbf29899f824218b2ddd02ab69",
            source_file: "lib/index.js",
            source_sha256: "e1c51afac2e31be8593c6426976e5f31dbe2a709b85ae1ea43ed406146f54265",
            excerpts: [
              {
                name: "UserQuestionError",
                lines: [
                  429,
                  434
                ],
                sha256: "5d49a4ecdf40039b26824f3575a79820b48ed011f90641870f44fe5a41bf5ced"
              }
            ],
            license_sha256: "ebb4f09972aee8608be255debaf78451a68e95c290f55c240dec2ecfa16ea6be",
            license_text_sha256: "fe195482bb2bddf34ebced7531d6afb11c33fa42f248472736ad9fa9b9eb503b",
            excerpted: true
          }
        ],
        reuse: {
          package: "@deepseek-ai/dsh-util-values",
          version: "0.1.7-alpha.2",
          file: "lib/index.js",
          symbol: "deepFreeze",
          lines: [
            194,
            230
          ],
          sha256: "66ee7f28877afa912496ba944a581ac74e7b87390e6a47c4c640d8f477bdc6e2"
        },
        constructor_names: [
          "HarnessError",
          "UserQuestionError"
        ]
      },
      tool_caller_projection: "readonly-id-session-header-cwd",
      session_methods: {
        flush: "invocation-bound-acknowledged-checkpoint-with-host-redaction-policy"
      },
      user_question_methods: {
        ask: "invocation-bound-root-agent-typed-answers-no-wait"
      },
      subagent_methods: {
        getProvider: "spawn-only-independent-context",
        startActivation: "invocation-bound-caller-or-parent-delivery-text-prompt-model-persona-deny",
        result: "text-output-typed-stop-reason",
        dispose: "exact-activation"
      },
      agents_methods: {},
      system_prompt_methods: {
        section: "child-persona-slot-in-agent-created-only"
      }
    };
  }
});

// src/dsh/resolve-hooks.ts
import { fileURLToPath as fileURLToPath2, pathToFileURL as pathToFileURL2 } from "node:url";
import { resolve as resolve2, isAbsolute as isAbsolute2, dirname } from "node:path";
function keyIn(closure, path) {
  const canonical = insideKey(closure.canonicalRoot, path);
  if (canonical !== void 0) return canonical;
  for (const root of closure.rawRoots) {
    const raw = insideKey(root, path);
    if (raw !== void 0) return raw;
  }
}
function admitReviewedClosure(baseUrl, files) {
  const rawRoot = resolve2(fileURLToPath2(baseUrl));
  const canonicalRoot = canonicalPath(rawRoot);
  const id = pathKey(canonicalRoot);
  const accepted = /* @__PURE__ */ Object.create(null);
  const keys = Object.keys(files);
  if (keys.length > 4096) throw new Error("reviewed composition closure exceeds its file limit");
  for (const key of keys) {
    if (!key || key.split("/").some((part) => !part || part === "." || part === "..") || key.includes("\\") || key.includes(":") || isAbsolute2(key) || !/^[a-f0-9]{64}$/.test(files[key])) throw new Error("invalid reviewed composition closure file");
    accepted[key] = files[key];
  }
  const existing = reviewedClosures.get(id);
  if (existing) {
    if (Object.keys(existing.files).length !== keys.length || keys.some((key) => existing.files[key] !== accepted[key])) throw new Error("the same composition root carries different file receipts");
    existing.refs++;
    existing.rawRoots.add(rawRoot);
  } else reviewedClosures.set(id, { files: Object.freeze(accepted), refs: 1, canonicalRoot, rawRoots: /* @__PURE__ */ new Set([rawRoot]) });
  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    const current2 = reviewedClosures.get(id);
    if (current2 && --current2.refs === 0) reviewedClosures.delete(id);
  };
}
async function importReviewedModule(baseUrl, path) {
  const rawRoot = resolve2(fileURLToPath2(baseUrl));
  const id = pathKey(canonicalPath(rawRoot));
  const receipt = reviewedClosures.get(id);
  if (!receipt) throw new Error("composition module closure is no longer admitted");
  const entry = resolve2(rawRoot, path);
  const admitted = closureAt(pathToFileURL2(entry).href);
  if (!admitted || admitted.root !== id || !(admitted.path in receipt.files)) throw new Error("composition module is absent from reviewed closure");
  return import(pathToFileURL2(entry).href);
}
function closureAt(url) {
  if (!url?.startsWith("file:")) return;
  const path = resolve2(fileURLToPath2(url));
  for (const [root, receipt] of reviewedClosures) {
    const inside = keyIn(receipt, path);
    if (inside !== void 0) return { root, receipt, path: inside };
  }
}
var SINGLETONS, reviewedClosures;
var init_resolve_hooks = __esm({
  "src/dsh/resolve-hooks.ts"() {
    "use strict";
    init_canonical_path();
    init_runtime();
    init_bun_closure();
    init_compatibility();
    SINGLETONS = compatibility_default.module_singletons;
    reviewedClosures = /* @__PURE__ */ new Map();
  }
});

// src/dsh/composition.ts
var composition_exports = {};
__export(composition_exports, {
  ReviewedLoader: () => ReviewedLoader,
  compositionRowProblem: () => compositionRowProblem,
  hasReviewedRowBridge: () => hasReviewedRowBridge,
  installCompositionLoader: () => installCompositionLoader,
  mountReviewedComposition: () => mountReviewedComposition,
  mountReviewedPreset: () => mountReviewedPreset,
  parsePresetComposition: () => parsePresetComposition,
  reviewComposition: () => reviewComposition
});
import { createHash as createHash2 } from "node:crypto";
import { readFile as readFile2 } from "node:fs/promises";
import { isAbsolute as isAbsolute3, relative as relative2, resolve as resolve3, sep as sep2 } from "node:path";
import { fileURLToPath as fileURLToPath3 } from "node:url";
function digest(bytes) {
  return createHash2("sha256").update(bytes).digest("hex");
}
function plainRelative(path) {
  return typeof path === "string" && path.length > 0 && path.length <= 4096 && !isAbsolute3(path) && !path.startsWith("/") && !/[\\:\u0000-\u001f\u007f]/u.test(path) && path.split("/").every((part) => part !== "" && part !== "." && part !== "..");
}
function boundData(root) {
  let nodes = 0;
  let bytes = 0;
  const ancestors = /* @__PURE__ */ new Set();
  function visit(value, depth) {
    if (++nodes > MAX_DATA_NODES || depth > MAX_DEPTH) throw new Error("composition data exceeds its resource limit");
    if (typeof value === "string") bytes += Buffer.byteLength(value);
    if (bytes > MAX_DATA_BYTES) throw new Error("composition expanded text exceeds 4 MiB");
    if (value === null || typeof value !== "object") return;
    if (ancestors.has(value)) throw new Error("cyclic configuration data is unsupported");
    ancestors.add(value);
    if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) {
      throw new Error("composition configuration must be plain data");
    }
    for (const [key, child] of Object.entries(value)) {
      bytes += Buffer.byteLength(key);
      visit(child, depth + 1);
    }
    ancestors.delete(value);
  }
  visit(root, 0);
}
function assignmentSafe(value) {
  if (Object.hasOwn(value, "__proto__")) throw new Error("prototype-mutating entry field is unsupported");
}
function checkRows(rows, depth = 0, count = { value: 0 }) {
  if (depth > MAX_DEPTH) throw new Error("composition entry nesting exceeds 32 levels");
  for (const row of rows) {
    if (++count.value > MAX_ROWS) throw new Error("composition exceeds 1024 entry rows");
    if (row === null || typeof row !== "object" || Array.isArray(row)) throw new Error("entry row must be an object");
    assignmentSafe(row);
    if (row.id !== void 0 && (typeof row.id !== "string" || row.id.length > 512)) throw new Error("entry id must be a bounded string");
    if (!(row.group === true && row.name === void 0) && (typeof row.name !== "string" || row.name.length === 0 || row.name.length > 4096)) throw new Error("entry name must be a bounded string");
    if ((row.group || row.name === "cordis:group" || row.name === "@deepseek-ai/cordis-plugin-group") && Array.isArray(row.config)) checkRows(row.config, depth + 1, count);
  }
}
function parsePresetComposition(source) {
  if (Buffer.byteLength(source) > MAX_SOURCE_BYTES) throw new Error("preset source exceeds its limit");
  const rows = load(source, { schema: entryListSchema });
  boundData(rows);
  if (!Array.isArray(rows)) throw new Error("preset composition must be an entry list");
  checkRows(rows);
  return rows;
}
function reviewComposition(spec) {
  if (!spec || spec.version !== 1 || !Array.isArray(spec.layers) || spec.layers.length === 0 || spec.layers.length > MAX_LAYERS) {
    throw new Error("composition needs 1–64 reviewed layers");
  }
  let sourceBytes = 0;
  const patches = [];
  const locations = [];
  const layers = [];
  for (const layer of spec.layers) {
    if (!layer || !plainRelative(layer.path) || typeof layer.source !== "string" || typeof layer.sha256 !== "string" || !SHA256.test(layer.sha256)) throw new Error("invalid reviewed layer");
    sourceBytes += Buffer.byteLength(layer.source);
    if (sourceBytes > MAX_SOURCE_BYTES || digest(layer.source) !== layer.sha256) throw new Error("layer source changed or exceeds 1 MiB");
    let parsed;
    try {
      parsed = load(layer.source, { schema: entryListSchema });
    } catch {
      throw new Error("invalid DSH patch data");
    }
    if (!Array.isArray(parsed)) throw new Error("DSH patch layer must be a list");
    boundData(parsed);
    for (const patch of parsed) {
      if (!patch || typeof patch !== "object" || Array.isArray(patch)) throw new Error("patch must be an object");
      assignmentSafe(patch);
      if (patch.insert !== void 0 && patch.insert !== null) {
        if (!Array.isArray(patch.insert)) throw new Error("patch insert must be a list");
        checkRows(patch.insert);
      }
    }
    patches.push(...parsed);
    locations.push(...parsed.map((_, i2) => ({ layer: layer.path, patch: i2 + 1 })));
    layers.push({ path: layer.path, sha256: layer.sha256 });
  }
  boundData(patches);
  const warnings = [];
  const skipped = [];
  let entries = [];
  for (const [i2, patch] of patches.entries()) {
    entries = applyEntryPatches(entries, [patch], (message, ...args) => {
      let index = 0;
      const reason = message.replace(/%C/g, () => JSON.stringify(args[index++]));
      warnings.push(reason);
      skipped.push({ ...typeof patch.id === "string" ? { row: patch.id } : {}, ...typeof patch.name === "string" ? { package: patch.name } : {}, reason, ...locations[i2] });
    });
  }
  boundData(entries);
  checkRows(entries);
  return { version: 1, entries, warnings, skipped, layers };
}
function hasReviewedRowBridge(name) {
  return Object.hasOwn(compatibility_default.row_bridges, name);
}
function compositionRowProblem(name) {
  return UNSUPPORTED_ROWS.get(name);
}
function installCompositionLoader(root) {
  if (loaders.has(root)) return;
  const ctx = root.isolate("loader");
  const fiber = ctx.plugin(ReviewedLoader, {});
  loaders.set(root, { ctx, fiber });
}
function validateInventory(group) {
  for (const row of group.data) {
    const entry = group.tree.store[row.id];
    if (!entry || entry.parent !== group || entry.options !== row) {
      throw new Error(`composition row ${JSON.stringify(row.id ?? row.name)} was not created`);
    }
    if (entry.disabled) continue;
    if (entry.subgroup) validateInventory(entry.subgroup);
    if (entry.subtree) validateInventory(entry.subtree.root);
  }
}
async function mountReviewedComposition(ctx, baseUrl, spec) {
  const review = reviewComposition(spec);
  function groups(rows) {
    for (const row of rows) {
      if (row.group === true && row.name === void 0) row.name = "cordis:group";
      if (row.group === true && Array.isArray(row.config)) groups(row.config);
    }
  }
  groups(review.entries);
  const installed = loaders.get(ctx.root);
  if (!installed) throw new Error("composition loader was not installed by this host");
  await installed.fiber.await();
  const loaderCtx = ctx.extend({
    baseUrl,
    [Context.isolate]: Object.assign(Object.create(ctx[Context.isolate]), {
      loader: installed.ctx[Context.isolate].loader
    })
  });
  const config = { baseUrl, spec, review, releaseClosure: admitReviewedClosure(baseUrl, spec.files) };
  const carrier = loaderCtx.plugin(ReviewedCarrier, config);
  let disposed = false;
  const dispose = async () => {
    if (disposed) return;
    disposed = true;
    try {
      await mountedTrees.get(config)?.dispose();
      await carrier.dispose();
    } finally {
      config.releaseClosure();
    }
  };
  ctx.effect(() => dispose, "reviewed DSH composition teardown");
  try {
    await carrier.await();
    const mount = mountedTrees.get(config);
    if (!mount || carrier.state !== FiberState.ACTIVE) throw new Error("reviewed composition carrier did not activate");
    return { tree: mount.tree, dispose };
  } catch (error) {
    await dispose().catch(() => void 0);
    throw error;
  }
}
function leakedServices(ctx, mount) {
  const rootIsolate = ctx.root[Context.isolate];
  const leaked = [];
  for (const key of Object.getOwnPropertySymbols(ctx.reflect.store)) {
    const impl = ctx.reflect.store[key];
    if (impl === void 0) continue;
    let current2 = impl.fiber;
    while (current2 !== mount && current2.parent.fiber !== current2) current2 = current2.parent.fiber;
    if (current2 === mount && rootIsolate[impl.name] === key) leaked.push(impl.name);
  }
  return leaked.sort((left, right) => left.localeCompare(right));
}
async function mountReviewedPreset(ctx, baseUrl, spec, catalog, selected) {
  if (!selected) throw new Error("agent-presets: explicit selection is required when no default is configured");
  const entries = structuredClone(reviewComposition(spec).entries);
  let found = 0;
  function prepare(rows) {
    for (const row of rows) {
      if (row.group === true || row.name === "cordis:group" || row.name === "@deepseek-ai/cordis-plugin-group") {
        prepare(row.config);
      } else if (row.name === AGENT_PRESETS) {
        found++;
        row.isolate = { ...row.isolate, agentPresets: true };
        row.config = { codewhale_reviewed: { catalog, selected, base_url: baseUrl } };
      }
    }
  }
  prepare(entries);
  if (found !== 1) throw new Error("exactly one raw roster row is required for a selected preset");
  const source = JSON.stringify([{ insert: entries }]);
  const top = await mountReviewedComposition(ctx, baseUrl, { ...spec, layers: [{ path: "selected-preset.json", sha256: digest(source), source }] });
  try {
    const entry = [...top.tree.entries()].find((entry2) => entry2.options.name === AGENT_PRESETS && !entry2.disabled);
    const roster = entry?.ctx.get("agentPresets");
    if (!roster) throw new Error("selected raw roster did not become active");
    await roster.mountSelected();
    return top;
  } catch (error) {
    await top.dispose();
    throw error;
  }
}
var MAX_LAYERS, MAX_SOURCE_BYTES, MAX_DATA_BYTES, MAX_DATA_NODES, MAX_ROWS, MAX_DEPTH, SHA256, UNSUPPORTED_ROWS, ReviewedGroup, ReviewedLoader, loaders, ReviewedTree, mountedTrees, ReviewedCarrier;
var init_composition = __esm({
  "src/dsh/composition.ts"() {
    "use strict";
    init_lib2();
    init_fiber_state();
    init_src();
    init_src2();
    init_js_yaml();
    init_mcp2();
    init_skill_filesystem();
    init_shell_hooks();
    init_resolve_hooks();
    init_persona();
    init_agent_presets();
    init_compatibility();
    MAX_LAYERS = 64;
    MAX_SOURCE_BYTES = 1024 * 1024;
    MAX_DATA_BYTES = 4 * 1024 * 1024;
    MAX_DATA_NODES = 1e5;
    MAX_ROWS = 1024;
    MAX_DEPTH = 32;
    SHA256 = /^[a-f0-9]{64}$/;
    UNSUPPORTED_ROWS = /* @__PURE__ */ new Map();
    ReviewedGroup = class extends Group {
      static inject = ["loader"];
    };
    ReviewedLoader = class extends Loader {
      constructor(ctx, config) {
        super(ctx, config);
        this.builtins.group = ReviewedGroup;
      }
    };
    loaders = /* @__PURE__ */ new WeakMap();
    ReviewedTree = class extends EntryTree {
      base;
      modules;
      spec;
      constructor(ctx, base, spec) {
        super(ctx);
        this.base = base;
        this.spec = spec;
        const modules = /* @__PURE__ */ new Map();
        if (!Array.isArray(spec.modules) || spec.modules.length > MAX_ROWS || !spec.files || Object.keys(spec.files).length > 4096) throw new Error("invalid reviewed module closure");
        for (const module of spec.modules) {
          if (!module || typeof module.name !== "string" || module.name.length === 0 || module.name.length > 4096 || modules.has(module.name) || !plainRelative(module.path) || !SHA256.test(module.sha256) || spec.files[module.path] !== module.sha256) throw new Error("module map differs from reviewed closure");
          modules.set(module.name, module);
        }
        this.modules = modules;
      }
      /** A reviewed composition is input; teardown and self-disposal never rewrite it. */
      write() {
      }
      import(name) {
        if (name === PERSONA) return reviewedPersonaModule;
        if (name === AGENT_PRESETS) return defineReviewedAgentPresets(mountReviewedComposition);
        if (name === "cordis:group" || name === "@deepseek-ai/cordis-plugin-group") return ReviewedGroup;
        if (name === "@deepseek-ai/dsh-mcp-client") return reviewedMcpModule();
        if (name === "@deepseek-ai/dsh-skill-filesystem") return reviewedSkillModule();
        if (name === "@deepseek-ai/dsh-hooks-claude-code") return reviewedHookModule("claude-code", fileURLToPath3(this.base), this.spec.files);
        if (name === "@deepseek-ai/dsh-hooks-codex") return reviewedHookModule("codex", fileURLToPath3(this.base), this.spec.files);
        if (UNSUPPORTED_ROWS.has(name)) throw new Error(`${name}: ${UNSUPPORTED_ROWS.get(name)}`);
        const module = this.modules.get(name);
        if (!module) throw new Error(`row requires ${JSON.stringify(name)}, absent from the reviewed module closure`);
        return this.importReviewed(module);
      }
      async importReviewed(module) {
        const root = fileURLToPath3(this.base);
        const path = resolve3(root, module.path);
        const inside = relative2(root, path);
        if (inside.startsWith(`..${sep2}`) || inside === ".." || isAbsolute3(inside)) throw new Error("module escapes reviewed bundle");
        if (this.spec.files[module.path] !== module.sha256 || digest(await readFile2(path)) !== module.sha256) throw new Error("module changed after review");
        return importReviewedModule(this.base, module.path);
      }
    };
    mountedTrees = /* @__PURE__ */ new WeakMap();
    ReviewedCarrier = class {
      constructor(ctx, config) {
        this.ctx = ctx;
        this.config = config;
        this.tree = new ReviewedTree(ctx, config.baseUrl, config.spec);
        mountedTrees.set(config, { tree: this.tree, dispose: () => this.dispose() });
      }
      ctx;
      config;
      static inject = ["loader"];
      tree;
      disposed = false;
      async dispose() {
        if (this.disposed) return;
        this.disposed = true;
        const fibers = [...new Set([...this.tree.entries()].map((entry) => entry.fiber).filter((fiber) => fiber !== void 0))].reverse();
        const pending = this.tree.getTasks();
        let stopError;
        try {
          this.tree.root.stop();
        } catch (error) {
          stopError = error;
        }
        const outcomes = await Promise.allSettled([
          ...pending,
          ...fibers.map((fiber) => Promise.resolve().then(() => fiber.dispose()))
        ]);
        try {
          await this.tree.await();
          if (stopError !== void 0) throw stopError;
          const failed = outcomes.find((outcome) => outcome.status === "rejected");
          if (failed) throw failed.reason;
        } finally {
          this.config.releaseClosure();
        }
      }
      async *[Service.init]() {
        yield () => this.dispose();
        await this.tree.root.update(this.config.review.entries);
        await this.tree.await();
        validateInventory(this.tree.root);
        for (const entry of this.tree.entries()) {
          if (entry.disabled) continue;
          if (!entry.fiber || entry.fiber.uid === null) throw new Error(`composition row ${JSON.stringify(entry.options.id ?? entry.options.name)} failed activation`);
          await entry.fiber.await();
          const missing = Object.keys(entry.fiber.inject).filter((name) => entry.fiber.ctx.get(name) === void 0);
          if (missing.length || entry.fiber.state !== FiberState.ACTIVE) throw new Error(`composition row ${JSON.stringify(entry.options.id ?? entry.options.name)} is not active`);
        }
        const leaked = leakedServices(this.ctx, this.ctx.fiber);
        if (leaked.length) throw new Error(`composition published shared-root services: ${leaked.join(", ")}`);
      }
    };
  }
});

// src/protocol.generated.ts
var MAGIC_ASCII;
var init_protocol_generated = __esm({
  "src/protocol.generated.ts"() {
    "use strict";
    MAGIC_ASCII = "CWX1";
  }
});

// src/protocol.ts
var MAGIC;
var init_protocol = __esm({
  "src/protocol.ts"() {
    "use strict";
    init_protocol_generated();
    init_protocol_generated();
    MAGIC = Buffer.from(MAGIC_ASCII, "ascii");
  }
});

// src/rpc.ts
var init_rpc = __esm({
  "src/rpc.ts"() {
    "use strict";
    init_protocol();
  }
});

// node_modules/@deepseek-ai/dsh-util-values/lib/index.js
var init_lib3 = __esm({
  "node_modules/@deepseek-ai/dsh-util-values/lib/index.js"() {
  }
});

// src/dsh/dsh-message-compat.js
var HarnessError, UserQuestionError;
var init_dsh_message_compat = __esm({
  "src/dsh/dsh-message-compat.js"() {
    "use strict";
    init_lib3();
    HarnessError = class extends Error {
      code;
      constructor(message, code, options) {
        super(message, options);
        this.code = code;
        this.name = new.target.name;
      }
    };
    UserQuestionError = class extends HarnessError {
      constructor(message, code, options) {
        super(message, code, options);
        this.name = "UserQuestionError";
      }
    };
    Object.defineProperty(HarnessError, "name", { value: "HarnessError" });
    Object.defineProperty(UserQuestionError, "name", { value: "UserQuestionError" });
  }
});

// src/dsh/dsh-tools-compat.js
var init_dsh_tools_compat = __esm({
  "src/dsh/dsh-tools-compat.js"() {
    "use strict";
    init_lib3();
  }
});

// src/shims/core.ts
var init_core = __esm({
  "src/shims/core.ts"() {
    "use strict";
    init_protocol();
    init_json();
    init_rpc();
  }
});

// src/shims/git.ts
import { AsyncLocalStorage } from "node:async_hooks";
var gitInvocations;
var init_git = __esm({
  "src/shims/git.ts"() {
    "use strict";
    gitInvocations = new AsyncLocalStorage();
  }
});

// src/shims/shell-hooks.ts
var init_shell_hooks2 = __esm({
  "src/shims/shell-hooks.ts"() {
    "use strict";
  }
});

// src/shims/hooks.ts
var init_hooks = __esm({
  "src/shims/hooks.ts"() {
    "use strict";
    init_json();
  }
});

// src/shims/storage.ts
var STORAGE_LIMITS, MAX_RECORD_BYTES;
var init_storage = __esm({
  "src/shims/storage.ts"() {
    "use strict";
    init_json();
    STORAGE_LIMITS = Object.freeze({ keyBytes: 128, valueBytes: 128 * 1024, totalBytes: 4 * 1024 * 1024, keys: 1024 });
    MAX_RECORD_BYTES = STORAGE_LIMITS.valueBytes + STORAGE_LIMITS.keyBytes * 6 + 128;
  }
});

// src/shims/avatars.ts
var init_avatars = __esm({
  "src/shims/avatars.ts"() {
    "use strict";
    init_owned();
  }
});

// src/tier.ts
var init_tier = __esm({
  "src/tier.ts"() {
    "use strict";
  }
});

// src/shims/commands.ts
var NO_ATTACHMENTS;
var init_commands = __esm({
  "src/shims/commands.ts"() {
    "use strict";
    init_json();
    NO_ATTACHMENTS = Object.freeze([]);
  }
});

// src/root.ts
import { AsyncLocalStorage as AsyncLocalStorage2 } from "node:async_hooks";
var REFUSED_SERVICES, PROVIDED_SERVICES, ownerStorage;
var init_root = __esm({
  "src/root.ts"() {
    "use strict";
    init_composition();
    init_protocol();
    init_rpc();
    init_dsh_message_compat();
    init_resolve_hooks();
    init_compatibility();
    init_dsh_tools_compat();
    init_json();
    init_core();
    init_git();
    init_owned();
    init_shell_hooks2();
    init_hooks();
    init_prompt();
    init_storage();
    init_mcp();
    init_avatars();
    init_skills();
    init_tier();
    init_commands();
    REFUSED_SERVICES = new Set(compatibility_default.refused_services);
    PROVIDED_SERVICES = new Set(compatibility_default.provided_services);
    ownerStorage = new AsyncLocalStorage2();
  }
});

// src/dsh/persona.ts
function normalizePersonaConfig(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("persona config must be plain data");
  const config = value;
  if (Object.keys(config).some((key) => !["prefix", "suffix", "complete", "includeRuntimeContext"].includes(key))) throw new TypeError("persona config has an unsupported field");
  if (config.complete !== void 0 && config.complete !== false) throw new TypeError("persona replacement belongs to the Core prompt and cannot be imported");
  if (config.includeRuntimeContext !== void 0 && config.includeRuntimeContext !== true) throw new TypeError("persona runtime-context suppression belongs to the Core prompt and cannot be imported");
  if (typeof config.prefix !== "string" || config.suffix !== void 0 && typeof config.suffix !== "string") throw new TypeError("persona prefix and suffix must be text");
  const result = { prefix: config.prefix, suffix: config.suffix ?? "" };
  for (const [position, text2] of Object.entries(result)) {
    if (Buffer.byteLength(text2, "utf8") > MAX_PROMPT_SECTION_BYTES) throw new RangeError("persona text exceeds 4096 UTF-8 bytes");
    if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(text2)) throw new TypeError("persona text contains control characters");
    validatePromptTemplate(text2);
    if (text2.trim().length > 0) normalizePromptSection({ id: `persona-${position}`, text: text2, interpolate: "model-cwd" });
  }
  return Object.freeze(result);
}
var PERSONA, reviewedPersonaModule;
var init_persona = __esm({
  "src/dsh/persona.ts"() {
    "use strict";
    init_prompt();
    init_root();
    PERSONA = "@deepseek-ai/dsh-persona";
    reviewedPersonaModule = Object.freeze({
      name: "persona",
      inject: ["prompt"],
      apply(ctx, value) {
        if (!ownerStorage.getStore()?.scope) throw new Error("persona requires an exact Core-selected entry scope");
        const config = normalizePersonaConfig(value);
        for (const [position, text2] of Object.entries(config)) {
          if (text2.trim().length > 0) ctx.prompt.registerSection({ id: `persona-${position}`, text: text2, interpolate: "model-cwd" });
        }
      }
    });
  }
});

// src/dsh/upstream/agent-presets/metadata.ts
import { join } from "node:path";
function text(value) {
  if (typeof value !== "string") return void 0;
  const trimmed = value.trim();
  return trimmed === "" ? void 0 : trimmed;
}
async function readPresetMetadata(directory, readFile3) {
  let raw;
  try {
    raw = await readFile3(join(directory, METADATA_FILE), "utf8");
  } catch {
    return {};
  }
  let parsed;
  try {
    parsed = yaml.load(raw);
  } catch {
    return {};
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
  const record = parsed;
  const name = text(record.name);
  const description = text(record.description);
  const order = typeof record.order === "number" && Number.isFinite(record.order) ? record.order : void 0;
  return {
    ...name === void 0 ? {} : { name },
    ...description === void 0 ? {} : { description },
    ...order === void 0 ? {} : { order }
  };
}
var METADATA_FILE;
var init_metadata = __esm({
  "src/dsh/upstream/agent-presets/metadata.ts"() {
    "use strict";
    init_js_yaml();
    METADATA_FILE = "preset.yml";
  }
});

// src/dsh/upstream/agent-presets/preset.ts
var PRESET_ID;
var init_preset = __esm({
  "src/dsh/upstream/agent-presets/preset.ts"() {
    "use strict";
    PRESET_ID = /^[a-z0-9][a-z0-9-]*$/;
  }
});

// src/dsh/upstream/agent-presets/specifier.ts
import { isAbsolute as isAbsolute4 } from "node:path";
import { pathToFileURL as pathToFileURL3 } from "node:url";
function classifyRowSpecifier(name) {
  if (name.startsWith("cordis:")) return { kind: "builtin", specifier: name };
  if (name.startsWith(".")) return { kind: "preset", specifier: name };
  if (name.startsWith("file:")) return { kind: "file", specifier: name };
  if (isAbsolute4(name)) return { kind: "file", specifier: pathToFileURL3(name).href };
  return { kind: "package", specifier: name };
}
var init_specifier = __esm({
  "src/dsh/upstream/agent-presets/specifier.ts"() {
    "use strict";
  }
});

// src/dsh/upstream/agent-presets/discovery.ts
import { isBuiltin as isBuiltin2 } from "node:module";
import { join as join2, resolve as resolve4 } from "node:path";
import { fileURLToPath as fileURLToPath4, pathToFileURL as pathToFileURL4 } from "node:url";
function entryListProblem(rows, at2 = "") {
  if (!Array.isArray(rows)) {
    return at2 === "" ? "the composition must be a top-level list of plugin rows" : `group ${at2} must hold a list of plugin rows`;
  }
  for (const [index, row] of rows.entries()) {
    const label = at2 === "" ? `row ${String(index + 1)}` : `${at2} row ${String(index + 1)}`;
    if (typeof row !== "object" || row === null || Array.isArray(row)) {
      return `${label} is not a plugin row (expected a map with a "name")`;
    }
    const { name, group, config } = row;
    if (typeof name !== "string" || name === "") {
      return `${label} names no plugin (a "name" string is required)`;
    }
    if (group === true) {
      const nested = entryListProblem(config, label);
      if (nested !== void 0) return nested;
    }
  }
  return void 0;
}
async function rowResolves(row, presetBase, harnessBase, resolves, io) {
  if (row.kind === "builtin") return true;
  if (row.kind === "package") return isBuiltin2(row.specifier) || resolves(row.specifier, harnessBase);
  const url = row.kind === "file" ? new URL(row.specifier) : new URL(row.specifier, presetBase);
  return await isFile(fileURLToPath4(url), io);
}
async function unresolvableRows(rows, presetBase, harnessBase, resolves, io, at2 = "") {
  const found = [];
  for (const [index, entry] of rows.entries()) {
    const row = entry;
    if (Boolean(row.disabled)) continue;
    const positional = at2 === "" ? `row ${String(index + 1)}` : `${at2} row ${String(index + 1)}`;
    if (row.group === true) {
      found.push(...await unresolvableRows(row.config, presetBase, harnessBase, resolves, io, positional));
      continue;
    }
    if (await rowResolves(classifyRowSpecifier(row.name), presetBase, harnessBase, resolves, io)) continue;
    const label = typeof row.id === "string" && row.id !== "" ? `row "${row.id}"` : positional;
    found.push({ label, name: row.name });
  }
  return found;
}
async function compositionProblem(path, harnessBase, resolves, io) {
  let content;
  try {
    content = await io.readFile(path, "utf8");
  } catch {
    return `the composition file ${COMPOSITION_FILE} cannot be read`;
  }
  let rows;
  try {
    rows = load(content, { schema: entryListSchema });
  } catch (error) {
    const full = error instanceof Error ? error.message : String(error);
    return `the composition is not valid YAML: ${full.replace(/\n[\s\S]*$/, "")}`;
  }
  const shape = entryListProblem(rows);
  if (shape !== void 0) return shape;
  const presetBase = new URL(".", pathToFileURL4(path)).href;
  let unresolvable;
  try {
    unresolvable = await unresolvableRows(rows, presetBase, harnessBase, resolves, io);
  } catch (error) {
    const full = error instanceof Error ? error.message : String(error);
    return `the composition's plugins cannot be checked: ${full.replace(/\n[\s\S]*$/, "")}`;
  }
  const [first] = unresolvable;
  if (first === void 0) return void 0;
  if (unresolvable.length === 1) {
    return `${first.label} names a plugin that cannot be resolved: ${first.name}`;
  }
  return `${String(unresolvable.length)} rows name plugins that cannot be resolved:
` + unresolvable.map((row) => `- ${row.label}: ${row.name}`).join("\n");
}
async function isFile(path, io) {
  try {
    return (await io.stat(path)).isFile();
  } catch {
    return false;
  }
}
async function scanRoot(root, harnessBase, resolves, io) {
  const dir = resolve4(root.path);
  let children;
  try {
    children = await io.readdir(dir, { withFileTypes: true });
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw new Error(`agent-presets: cannot read preset root ${dir}: ${String(error)}`, { cause: error });
  }
  const found = [];
  for (const child of children) {
    if (!child.isDirectory() || !PRESET_ID.test(child.name)) continue;
    const directory = join2(dir, child.name);
    const path = join2(directory, COMPOSITION_FILE);
    const broken = await isFile(path, io) ? await compositionProblem(path, harnessBase, resolves, io) : `the composition file ${COMPOSITION_FILE} is missing — the directory still occupies the id; delete it or restore the file`;
    const metadata = await readPresetMetadata(directory, io.readFile);
    found.push({
      id: child.name,
      trust: root.trust,
      path,
      ...metadata,
      ...broken === void 0 ? {} : { broken }
    });
  }
  return found.sort((left, right) => {
    const leftOrder = left.order ?? Number.POSITIVE_INFINITY;
    const rightOrder = right.order ?? Number.POSITIVE_INFINITY;
    const byOrder = leftOrder === rightOrder ? 0 : leftOrder - rightOrder;
    return byOrder === 0 ? left.id.localeCompare(right.id) : byOrder;
  });
}
async function discoverPresets(roots, harnessBase, resolves, io) {
  const byId = /* @__PURE__ */ new Map();
  for (const root of roots) {
    for (const preset of await scanRoot(root, harnessBase, resolves, io)) {
      if (byId.has(preset.id)) continue;
      byId.set(preset.id, preset);
    }
  }
  return [...byId.values()];
}
var COMPOSITION_FILE, SHIPPED_PRESET_ROOT;
var init_discovery = __esm({
  "src/dsh/upstream/agent-presets/discovery.ts"() {
    "use strict";
    init_js_yaml();
    init_src2();
    init_metadata();
    init_preset();
    init_specifier();
    COMPOSITION_FILE = "agent.cordis.yml";
    SHIPPED_PRESET_ROOT = fileURLToPath4(new URL("../presets/", import.meta.url));
  }
});

// src/dsh/upstream/agent-presets/composition-inventory.ts
function disabledContribution(value, evaluateExpression) {
  if (isJsExpr(value)) {
    try {
      return Boolean(evaluateExpression(value.__jsExpr));
    } catch {
      return "conditional";
    }
  }
  return Boolean(value);
}
function combineDisabled(outer, own) {
  if (outer === true || own === true) return true;
  if (outer === "conditional" || own === "conditional") return "conditional";
  return false;
}
function flattenRows(rows, outerDisabled, evaluateExpression, found) {
  for (const value of rows) {
    const row = value;
    const disabled = combineDisabled(outerDisabled, disabledContribution(row.disabled, evaluateExpression));
    if (row.group === true) {
      flattenRows(row.config, disabled, evaluateExpression, found);
      continue;
    }
    found.push({
      entryId: typeof row.id === "string" && row.id !== "" ? row.id : null,
      moduleName: row.name,
      enabled: disabled === true ? false : disabled === "conditional" ? "conditional" : true,
      ...isJsExpr(row.disabled) ? { condition: row.disabled.__jsExpr } : {}
    });
  }
}
async function fileComposition(path, evaluateExpression, readFile3) {
  let rows;
  try {
    rows = load(await readFile3(path, "utf8"), { schema: entryListSchema });
  } catch (error) {
    return { broken: error instanceof Error ? error.message : String(error) };
  }
  const problem = entryListProblem(rows);
  if (problem !== void 0) return { broken: problem };
  const found = [];
  flattenRows(rows, false, evaluateExpression, found);
  return { rows: found };
}
function mountedCompositionRows(tree) {
  const found = [];
  for (const entry of tree.entries()) {
    if (entry.options.group) continue;
    found.push({
      entryId: entry.id,
      moduleName: entry.options.name,
      enabled: !entry.disabled,
      ...isJsExpr(entry.options.disabled) ? { condition: entry.options.disabled.__jsExpr } : {},
      ...entry.fiber === void 0 ? {} : { fiberState: entry.fiber.state }
    });
  }
  return found;
}
var init_composition_inventory = __esm({
  "src/dsh/upstream/agent-presets/composition-inventory.ts"() {
    "use strict";
    init_js_yaml();
    init_src();
    init_src2();
    init_discovery();
  }
});

// src/dsh/agent-presets.ts
import { createHash as createHash3 } from "node:crypto";
import { resolve as resolve5, relative as relative3, dirname as dirname3, sep as sep3 } from "node:path";
import { fileURLToPath as fileURLToPath5, pathToFileURL as pathToFileURL5 } from "node:url";
function plain(path) {
  return typeof path === "string" && path.length > 0 && path.length <= 4096 && !/[\\:\u0000-\u001f\u007f]/u.test(path) && !path.startsWith("/") && path.split("/").every((p) => p !== "" && p !== "." && p !== "..");
}
function fail(message) {
  throw new Error(`agent-presets: ${message}`);
}
function containedPackageModule(name, documents, files) {
  if (!name || /[\\:\u0000-\u0020\u007f]/u.test(name)) return;
  const parts = name.split("/");
  const n = name.startsWith("@") ? 2 : 1;
  if (parts.length < n || parts.some((p) => !p || p === "." || p === "..")) return;
  const pkg = parts.slice(0, n).join("/");
  const packageRoot = `node_modules/${pkg}`;
  const source = documents.get(`${packageRoot}/package.json`);
  if (source === void 0) return;
  let data2;
  try {
    data2 = JSON.parse(source);
  } catch {
    return;
  }
  if (data2.name !== pkg || data2.type !== "module") return;
  const subpath = parts.length === n ? "." : `./${parts.slice(n).join("/")}`;
  const refused = /* @__PURE__ */ Symbol("unsupported package target");
  function target(value) {
    if (typeof value === "string") return value;
    if (!value || Array.isArray(value) || typeof value !== "object") return refused;
    for (const [condition, child] of Object.entries(value)) {
      if (["node", "import", "default"].includes(condition)) {
        const picked = target(child);
        if (picked !== void 0) return picked;
      }
    }
  }
  let entry;
  if (data2.exports !== void 0) {
    const value = data2.exports;
    entry = value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).some((k) => k.startsWith(".")) ? target(value[subpath]) : subpath === "." ? target(value) : void 0;
  } else if (subpath === ".") entry = typeof data2.main === "string" ? data2.main : "./index.js";
  else entry = subpath;
  if (typeof entry !== "string" || !entry.startsWith("./") || !plain(entry.slice(2))) return;
  const path = `${packageRoot}/${entry.slice(2)}`;
  if (!/\.(mjs|js|mts)$/.test(path) || !SHA.test(files[path] ?? "")) return;
  return { name, path, sha256: files[path] };
}
async function reviewAgentPresets(input) {
  const { composition } = input;
  const files = composition.files;
  if (!files || Object.keys(files).length > 4096 || !Array.isArray(input.directories) || input.directories.length > 4096) fail("invalid source inventory");
  const base = resolve5("codewhale-reviewed-agent-presets");
  const baseUrl = pathToFileURL5(`${base}${sep3}`).href;
  function key(path) {
    const inside = relative3(base, path).split(sep3).join("/");
    if (!plain(inside)) fail("path escapes the admitted source root");
    return inside;
  }
  const documents = /* @__PURE__ */ new Map();
  let bytes = 0;
  for (const doc of input.documents) {
    if (!plain(doc.path) || !SHA.test(doc.sha256) || files[doc.path] !== doc.sha256 || hash(doc.source) !== doc.sha256 || documents.has(doc.path)) fail("document changed or is outside the source receipt");
    bytes += Buffer.byteLength(doc.source);
    if (Buffer.byteLength(doc.source) > 1024 * 1024 || bytes > 4 * 1024 * 1024) fail("documents exceed their bound");
    documents.set(doc.path, doc.source);
  }
  const directories = new Set(input.directories);
  for (const path of directories) if (!plain(path)) fail("invalid source directory");
  for (const path of Object.keys(files)) if (!plain(path) || !SHA.test(files[path])) fail("invalid source file receipt");
  const invalid = /* @__PURE__ */ new Set();
  const parsed = /* @__PURE__ */ new Map();
  const io = {
    async readFile(path) {
      const relative4 = key(path);
      if (invalid.has(relative4)) throw new Error("composition exceeds its data/shape bound");
      const text2 = documents.get(relative4);
      if (text2 === void 0) throw new Error("document not admitted");
      return text2;
    },
    async stat(path) {
      return { isFile: () => Object.hasOwn(files, key(path)) };
    },
    async readdir(path) {
      const prefix = `${key(path)}/`;
      if (!directories.has(prefix.slice(0, -1))) {
        const e = new Error("missing root");
        Object.assign(e, { code: "ENOENT" });
        throw e;
      }
      return [...directories].filter((p) => p.startsWith(prefix) && !p.slice(prefix.length).includes("/")).map((p) => ({ name: p.slice(prefix.length), isDirectory: () => true }));
    }
  };
  const { reviewComposition: reviewComposition2, parsePresetComposition: parsePresetComposition2, compositionRowProblem: compositionRowProblem2, hasReviewedRowBridge: hasReviewedRowBridge2 } = await Promise.resolve().then(() => (init_composition(), composition_exports));
  for (const [path, source] of documents) {
    if (!path.endsWith("/agent.cordis.yml")) continue;
    try {
      parsed.set(path, parsePresetComposition2(source));
    } catch {
      invalid.add(path);
    }
  }
  const top = reviewComposition2(composition);
  const rosterRows = [];
  function visit(rows2) {
    for (const row of rows2) {
      if (row.group === true || row.name === "cordis:group" || row.name === "@deepseek-ai/cordis-plugin-group") visit(row.config ?? []);
      else if (row.name === AGENT_PRESETS) rosterRows.push(row);
    }
  }
  visit(top.entries);
  if (rosterRows.length !== 1) fail("exactly one raw roster row is required");
  const config = rosterRows[0].config ?? {};
  if (!config || Array.isArray(config) || typeof config !== "object" || Object.keys(config).some((k) => !["default", "roots", "includeShippedRoot", "includeUserRoot"].includes(k))) fail("roster configuration must be literal upstream fields");
  for (const k of ["includeShippedRoot", "includeUserRoot"]) if (config[k] !== void 0 && typeof config[k] !== "boolean") fail("root gates must be literal booleans");
  if (config.default !== void 0 && (typeof config.default !== "string" || !PRESET_ID.test(config.default) || config.default.length > 64)) fail("invalid default id");
  const roots = [];
  if (config.includeShippedRoot !== false) {
    const candidates = [...documents.entries()].filter(([p, text2]) => {
      if (p !== "package.json" && !p.endsWith("/package.json")) return false;
      try {
        return JSON.parse(text2).name === AGENT_PRESETS;
      } catch {
        return false;
      }
    });
    if (candidates.length !== 1) fail("shipped root requires exactly one admitted agent-presets package");
    const folder = dirname3(candidates[0][0]).split(sep3).join("/");
    const root = folder === "." ? "presets" : `${folder}/presets`;
    if (!directories.has(root)) fail("shipped preset root was not admitted");
    roots.push({ path: resolve5(base, root), trust: "system" });
  }
  if (config.roots !== void 0 && !Array.isArray(config.roots)) fail("roots must be a literal list");
  for (const root of config.roots ?? []) {
    if (!root || Object.keys(root).some((k) => !["path", "trust"].includes(k)) || !plain(root.path) || !["system", "user"].includes(root.trust)) fail("configured roots must stay inside the admitted package");
    if (!directories.has(root.path)) fail("configured preset root was not admitted");
    roots.push({ path: resolve5(base, root.path), trust: root.trust });
  }
  if (config.includeUserRoot !== false) {
    if (!directories.has(".agent-presets")) fail("user root requires an admitted .agent-presets tree; ambient home discovery is refused");
    roots.push({ path: resolve5(base, ".agent-presets"), trust: "user" });
  }
  if (roots.length > 64) fail("too many roots");
  const declared = new Map(composition.modules.map((m) => [m.name, m]));
  if (declared.size !== composition.modules.length) fail("duplicate module name receipt");
  const found = await discoverPresets(roots, baseUrl, (name) => hasReviewedRowBridge2(name) || declared.has(name) || containedPackageModule(name, documents, files) !== void 0, io);
  if (found.length > MAX_PRESETS) fail("too many presets");
  const prepared = [];
  const rows = [];
  const inventory = /* @__PURE__ */ Object.create(null);
  for (const preset of found) {
    let resolveRows2 = function(children) {
      for (const row of children) {
        if (Boolean(row.disabled) && !isJsExpr(row.disabled)) continue;
        if (row.group === true || row.name === "cordis:group" || row.name === "@deepseek-ai/cordis-plugin-group") {
          resolveRows2(row.config ?? []);
          continue;
        }
        const specifier = classifyRowSpecifier(row.name);
        if (row.name === PERSONA) normalizePersonaConfig(row.config);
        if (specifier.kind === "builtin" || hasReviewedRowBridge2(row.name)) continue;
        if (row.name === AGENT_PRESETS) fail("nested roster cannot create a second selection authority");
        const problem = compositionRowProblem2(row.name);
        if (problem) fail(problem);
        let receipt;
        if (specifier.kind === "package") receipt = declared.get(row.name) ?? containedPackageModule(row.name, documents, files);
        else {
          const url = specifier.kind === "file" ? new URL(specifier.specifier) : new URL(specifier.specifier, pathToFileURL5(preset.path).href);
          const path2 = key(fileURLToPath5(url));
          if (files[path2] !== void 0) receipt = { name: row.name, path: path2, sha256: files[path2] };
        }
        if (!receipt || files[receipt.path] !== receipt.sha256) {
          if (isJsExpr(row.disabled)) continue;
          fail("row module has no exact admitted receipt");
        }
        modules.set(row.name, receipt);
      }
    };
    var resolveRows = resolveRows2;
    if (preset.id.length > 64 || (preset.name?.length ?? 0) > 1024 || (preset.description?.length ?? 0) > 4096) fail("preset metadata exceeds its bound");
    const metadata = {
      id: preset.id,
      trust: preset.trust,
      is_default: preset.id === config.default,
      ...preset.name === void 0 ? {} : { name: preset.name },
      ...preset.description === void 0 ? {} : { description: preset.description },
      ...preset.order === void 0 ? {} : { order: preset.order },
      ...preset.broken === void 0 ? {} : { broken: preset.broken }
    };
    rows.push(metadata);
    const path = key(preset.path);
    inventory[preset.id] = { path, ...preset.broken !== void 0 ? { broken: preset.broken } : await fileComposition(preset.path, () => {
      throw new Error("conditional until mounted");
    }, io.readFile) };
    if (preset.broken !== void 0) continue;
    const source = await io.readFile(preset.path, "utf8");
    const entries = parsed.get(key(preset.path));
    const modules = /* @__PURE__ */ new Map();
    try {
      resolveRows2(entries);
    } catch (error) {
      metadata.broken = error instanceof Error ? error.message : "preset preparation refused";
      continue;
    }
    const layer = JSON.stringify([{ insert: entries }]);
    const spec = { version: 1, layers: [{ path: key(preset.path), sha256: hash(layer), source: layer }], modules: [...modules.values()], files };
    reviewComposition2(spec);
    prepared.push({ metadata, composition: spec, source });
  }
  if (config.default !== void 0 && !prepared.some((p) => p.metadata.id === config.default)) fail("default preset is missing or broken");
  if (!prepared.length) fail("no admitted usable preset");
  if (Buffer.byteLength(JSON.stringify(inventory)) > 4 * 1024 * 1024) fail("composition inventory exceeds its bound");
  return { catalog: { version: 1, ...config.default === void 0 ? {} : { default: config.default }, mode_selection_enabled: true, presets: rows, inventory }, presets: prepared };
}
function defineReviewedAgentPresets(mount) {
  return class ReviewedAgentPresets extends Service {
    static inject = ["loader"];
    snapshot;
    selfCtx;
    constructor(ctx, config) {
      super(ctx, "agentPresets");
      this.selfCtx = ctx;
      const snapshot = config?.codewhale_reviewed;
      if (!snapshot || snapshot.catalog?.version !== 1 || !snapshot.catalog.presets.some((p) => p.id === snapshot.selected?.metadata.id && !p.broken)) fail("raw roster needs an installer-minted preset receipt");
      this.snapshot = structuredClone(snapshot);
    }
    mounting;
    mounted;
    mountSelected() {
      return this.mounting ??= mount(this.selfCtx, this.snapshot.base_url, this.snapshot.selected.composition).then((result) => this.mounted = result);
    }
    async list() {
      return this.snapshot.catalog.presets.map((row) => ({ ...structuredClone(row), path: fileURLToPath5(new URL(this.snapshot.catalog.inventory[row.id].path, this.snapshot.base_url)) }));
    }
    async remoteExportList() {
      return { presets: this.snapshot.catalog.presets.map(({ is_default, entry, ...row }) => ({ ...structuredClone(row), isDefault: is_default })), authorable: false, defaultId: this.defaultId, modeSelectionEnabled: this.snapshot.catalog.mode_selection_enabled };
    }
    get defaultId() {
      return this.snapshot.catalog.default;
    }
    get authorable() {
      return false;
    }
    async resolve(id = this.defaultId) {
      const row = (await this.list()).find((p) => p.id === id);
      if (!row) fail("preset not found");
      return row;
    }
    composedPreset() {
      return this.snapshot.selected.metadata.id;
    }
    async compositionInventory() {
      return this.snapshot.catalog.presets.map((row) => {
        const identity = { id: row.id, trust: row.trust, ...row.name === void 0 ? {} : { name: row.name }, isDefault: row.id === this.defaultId };
        if (row.id === this.composedPreset() && this.mounted) return { ...identity, rows: mountedCompositionRows(this.mounted.tree) };
        const read = this.snapshot.catalog.inventory[row.id];
        if (row.broken || "broken" in read) return { ...identity, broken: row.broken ?? ("broken" in read ? read.broken : void 0), rows: [] };
        return { ...identity, rows: structuredClone(read.rows) };
      });
    }
    async readDocument(id) {
      if (id !== this.composedPreset()) fail("read another preset through Core file authority");
      return { agentPreset: id, trust: this.snapshot.selected.metadata.trust, content: this.snapshot.selected.source };
    }
    select() {
      fail("select through Core AgentProfile/native preset selection before a turn");
    }
    recompose() {
      fail("Core owns the captured agent selection and blank-session check");
    }
    copy() {
      fail("copy through Core file/install/review/reload authority");
    }
    remove() {
      fail("delete through Core file/install/review/reload authority");
    }
  };
}
var AGENT_PRESETS, MAX_PRESETS, hash, SHA;
var init_agent_presets = __esm({
  "src/dsh/agent-presets.ts"() {
    init_persona();
    init_lib2();
    init_discovery();
    init_preset();
    init_specifier();
    init_composition_inventory();
    init_src();
    AGENT_PRESETS = "@deepseek-ai/dsh-agent-presets";
    MAX_PRESETS = 64;
    hash = (bytes) => createHash3("sha256").update(bytes).digest("hex");
    SHA = /^[a-f0-9]{64}$/;
  }
});
init_agent_presets();
export {
  AGENT_PRESETS,
  containedPackageModule,
  defineReviewedAgentPresets,
  reviewAgentPresets
};
